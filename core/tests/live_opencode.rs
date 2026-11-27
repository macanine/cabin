//! Live acceptance for the native provider runtime, OpenCode v2 only. Gated:
//!   CABIN_LIVE_OPENCODE=1 CABIN_OPENCODE2_BIN=/path/to/opencode2 \
//!   cargo test --test live_opencode -- --ignored --nocapture
//! It drives the production path (`App`: chat create → provider process → real
//! `opencode2`), answers a real approval, checks the real file edit, restarts the
//! app and resumes. There is no Claude/Codex/Pi live test by design.

#![cfg(unix)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use agent_core::app::{App, NewChat};
use agent_core::model::{
    AgentKind, ChatItemKind, ChatItemStatus, ChatPermissionMode, ChatRunState,
};
use agent_core::paths::AppPaths;

fn wait<T>(what: &str, secs: u64, mut probe: impl FnMut() -> Option<T>) -> T {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if let Some(value) = probe() {
            return value;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("timed out waiting for {what}");
}

fn answer_and_wait_ready(app: &App, id: &str, approvals: &mut usize) {
    wait("the turn to finish", 150, || {
        let snapshot = app.chat_snapshot(id).ok()?;
        for item in &snapshot.items {
            if item.kind == ChatItemKind::ApprovalRequest && item.status == ChatItemStatus::Waiting
            {
                let request = item.request_id.clone().unwrap();
                let offered: Vec<_> = item
                    .approval
                    .as_ref()
                    .unwrap()
                    .decisions
                    .iter()
                    .map(|d| d.decision.clone())
                    .collect();
                assert!(
                    !offered.contains(&"acceptForSession".to_string()),
                    "descriptor forbids it: {offered:?}"
                );
                if app
                    .chat_respond(id, &request, "allow", serde_json::Value::Null)
                    .is_ok()
                {
                    *approvals += 1;
                }
            }
        }
        (snapshot.state.state == ChatRunState::Ready
            && snapshot.items.iter().any(|i| {
                i.kind == ChatItemKind::AssistantMessage && i.status == ChatItemStatus::Completed
            }))
        .then_some(())
    });
}

#[test]
#[ignore = "live: needs OpenCode v2 (see module docs)"]
fn opencode_v2_through_the_production_runtime() {
    if std::env::var("CABIN_LIVE_OPENCODE").as_deref() != Ok("1") {
        eprintln!("skipped: set CABIN_LIVE_OPENCODE=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("project");
    std::fs::create_dir_all(&work).unwrap();
    let git = |args: &[&str]| {
        assert!(
            Command::new("git")
                .args([
                    "-C",
                    work.to_str().unwrap(),
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t"
                ])
                .args(args)
                .status()
                .unwrap()
                .success()
        )
    };
    git(&["init", "-q"]);
    git(&["commit", "-q", "--allow-empty", "-m", "init"]);
    let paths = || AppPaths::new(root.path().join("data"), root.path().join("home"));
    let prompt = "Use your file write/edit tool (not the shell) to create notes.txt in the current directory. Its whole content is the text between the quotes, without the quotes: \"Cabin v2 live test\" Then reply done.";

    let app = App::open(paths()).unwrap();
    let workspace = app
        .create_workspace("project", work.to_str().unwrap())
        .unwrap();
    let session = app
        .create_chat(NewChat {
            workspace_id: workspace.id.clone(),
            agent: AgentKind::Opencode,
            prompt: Some(prompt.into()),
            model: Some("opencode/big-pickle".into()),
            attachments: Vec::new(),
            permission: ChatPermissionMode::ApprovalRequired,
            worktree_base: None,
        })
        .unwrap();
    // Model discovery goes through the host as well (no chat needed).
    let models = app
        .agent_models(&workspace.id, AgentKind::Opencode)
        .unwrap();
    assert!(
        models.iter().any(|m| m.id == "opencode/big-pickle"),
        "free model listed: {models:?}"
    );
    let mut approvals = 0;
    answer_and_wait_ready(&app, &session.id, &mut approvals);
    assert!(approvals >= 1, "a real approval was answered");
    assert_eq!(
        std::fs::read_to_string(work.join("notes.txt"))
            .unwrap()
            .trim(),
        "Cabin v2 live test"
    );
    let first = app.chat_snapshot(&session.id).unwrap();
    assert!(
        first
            .items
            .iter()
            .any(|i| i.kind == ChatItemKind::FileChange
                && i.files.iter().any(|f| f == "notes.txt")
                && !i.changes.is_empty()),
        "file change with a structured diff"
    );
    let user_messages = first
        .items
        .iter()
        .filter(|i| i.kind == ChatItemKind::UserMessage)
        .count();
    assert_eq!(user_messages, 1, "one user message, no duplicates");
    let git_status = String::from_utf8(
        Command::new("git")
            .args(["-C", work.to_str().unwrap(), "status", "--porcelain"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(git_status.contains("notes.txt"));
    let ids: Vec<_> = first.items.iter().map(|i| i.id.clone()).collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "unique item ids");

    // Second change source: a file written through the shell is not a provider file-change event,
    // so only the guest's workspace diff can show it.
    app.chat_send(
        &session.id,
        "Use the shell (bash) to run exactly: printf 'from shell' > shell.txt . Then reply done.",
        vec![],
    )
    .unwrap();
    let mut shell_approvals = 0;
    wait("the shell turn and its workspace diff", 150, || {
        let snapshot = app.chat_snapshot(&session.id).ok()?;
        for item in &snapshot.items {
            if item.kind == ChatItemKind::ApprovalRequest
                && item.status == ChatItemStatus::Waiting
                && app
                    .chat_respond(
                        &session.id,
                        item.request_id.as_ref().unwrap(),
                        "allow",
                        serde_json::Value::Null,
                    )
                    .is_ok()
            {
                shell_approvals += 1;
            }
        }
        snapshot
            .items
            .iter()
            .any(|i| {
                i.kind == ChatItemKind::FileChange
                    && i.changes.iter().any(|c| {
                        c.path == "shell.txt"
                            && c.origin == agent_core::model::ChangeOrigin::Workspace
                            && c.patch
                                .as_deref()
                                .is_some_and(|p| p.contains("+from shell"))
                    })
            })
            .then_some(())
    });
    assert_eq!(
        std::fs::read_to_string(work.join("shell.txt")).unwrap(),
        "from shell"
    );
    wait("ready after the shell turn", 60, || {
        (app.chat_snapshot(&session.id).ok()?.state.state == ChatRunState::Ready).then_some(())
    });
    let first = app.chat_snapshot(&session.id).unwrap();

    // Restart the whole app: state is rebuilt from the persisted snapshots.
    app.stop_session(&session.id).unwrap();
    drop(app);
    let app = App::open(paths()).unwrap();
    let rebuilt = app.chat_snapshot(&session.id).unwrap();
    assert_eq!(
        rebuilt.items.len(),
        first.items.len(),
        "no duplication or loss after restart"
    );
    assert_eq!(
        rebuilt
            .items
            .iter()
            .map(|i| (i.id.clone(), i.revision))
            .collect::<Vec<_>>(),
        first
            .items
            .iter()
            .map(|i| (i.id.clone(), i.revision))
            .collect::<Vec<_>>()
    );

    // Resume on the saved native session and answer from it.
    app.start_session(&session.id, true, 100, 30).unwrap();
    wait("the resumed chat to be ready", 120, || {
        (app.chat_snapshot(&session.id).ok()?.state.state == ChatRunState::Ready).then_some(())
    });
    // A model switch on a live chat applies from the next turn.
    app.chat_set_model(&session.id, Some("opencode/big-pickle".into()))
        .unwrap();
    app.chat_send(
        &session.id,
        "What is the exact content of notes.txt? Answer with the content only.",
        vec![],
    )
    .unwrap();
    let mut more = 0;
    wait("the resumed answer", 150, || {
        let snapshot = app.chat_snapshot(&session.id).ok()?;
        for item in &snapshot.items {
            if item.kind == ChatItemKind::ApprovalRequest && item.status == ChatItemStatus::Waiting
            {
                let _ = app.chat_respond(
                    &session.id,
                    item.request_id.as_ref().unwrap(),
                    "allow",
                    serde_json::Value::Null,
                );
                more += 1;
            }
        }
        (snapshot.state.state == ChatRunState::Ready
            && snapshot.items.iter().any(|i| {
                i.kind == ChatItemKind::AssistantMessage
                    && i.text.contains("Cabin v2 live test")
                    && i.id != first.items.last().unwrap().id
            }))
        .then_some(())
    });
    app.stop_session(&session.id).unwrap();
    let _ = Path::new("/");
}

/// ACP-only features on the production path: the session controls the agent
/// reports (config options, slash commands, context use), changing an option,
/// and a message delivered into a running turn (steering).
#[test]
#[ignore = "live: needs OpenCode v2 (see module docs)"]
fn opencode_v2_controls_and_steering() {
    if std::env::var("CABIN_LIVE_OPENCODE").as_deref() != Ok("1") {
        eprintln!("skipped: set CABIN_LIVE_OPENCODE=1");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("project");
    std::fs::create_dir_all(&work).unwrap();
    let app = App::open(AppPaths::new(
        root.path().join("data"),
        root.path().join("home"),
    ))
    .unwrap();
    let workspace = app
        .create_workspace("project", work.to_str().unwrap())
        .unwrap();
    let session = app
        .create_chat(NewChat {
            workspace_id: workspace.id.clone(),
            agent: AgentKind::Opencode,
            prompt: None,
            model: Some("opencode/big-pickle".into()),
            attachments: Vec::new(),
            permission: ChatPermissionMode::FullAccess,
            worktree_base: None,
        })
        .unwrap();
    let controls = wait("the agent's controls", 120, || {
        let snapshot = app.chat_snapshot(&session.id).ok()?;
        (snapshot.state.state == ChatRunState::Ready
            && !snapshot.controls.options.is_empty()
            && !snapshot.controls.commands.is_empty())
        .then_some(snapshot.controls)
    });
    let model = controls
        .options
        .iter()
        .find(|option| option.category.as_deref() == Some("model"))
        .expect("a model selector");
    assert_eq!(
        model.current, "opencode/big-pickle",
        "the chat's model was applied"
    );
    assert!(controls.can_steer, "OpenCode advertises steering");
    assert!(
        controls
            .commands
            .iter()
            .any(|command| command.name == "compact")
    );
    let mode = controls
        .options
        .iter()
        .find(|option| option.category.as_deref() == Some("mode"))
        .expect("a mode selector");
    assert!(mode.choices.iter().any(|choice| choice.value == "plan"));
    app.chat_set_option(&session.id, &mode.id, "plan").unwrap();
    let current = app.chat_snapshot(&session.id).unwrap().controls;
    assert_eq!(
        current
            .options
            .iter()
            .find(|option| option.id == mode.id)
            .unwrap()
            .current,
        "plan"
    );
    app.chat_set_option(&session.id, &mode.id, "build").unwrap();
    assert!(
        app.chat_set_option(&session.id, &mode.id, "no-such-mode")
            .is_err()
    );

    app.chat_send(
        &session.id,
        "Run these shell commands one at a time, each as its own tool call: `sleep 4; echo one`, `sleep 4; echo two`, `sleep 4; echo three`. Then summarize.",
        vec![],
    )
    .unwrap();
    wait("a running command", 120, || {
        let snapshot = app.chat_snapshot(&session.id).ok()?;
        snapshot
            .items
            .iter()
            .any(|item| item.kind == ChatItemKind::CommandExecution)
            .then_some(())
    });
    app.chat_steer(
        &session.id,
        "Change of plan: skip the remaining commands and reply with only the word STEERED.",
        vec![],
    )
    .unwrap();
    let done = wait("the steered turn to finish", 150, || {
        let snapshot = app.chat_snapshot(&session.id).ok()?;
        (snapshot.state.state == ChatRunState::Ready
            && snapshot.items.iter().any(|item| {
                item.kind == ChatItemKind::AssistantMessage && item.text.contains("STEERED")
            }))
        .then_some(snapshot)
    });
    let steered = done
        .items
        .iter()
        .find(|item| item.kind == ChatItemKind::UserMessage && item.text.contains("Change of plan"))
        .expect("the steering message is in the timeline");
    assert_eq!(steered.status, ChatItemStatus::Completed);
    assert_eq!(steered.title, "Steered");
    let reply = done
        .items
        .iter()
        .find(|item| item.kind == ChatItemKind::AssistantMessage && item.text.contains("STEERED"))
        .unwrap();
    assert!(
        reply.ordinal > steered.ordinal,
        "the reply follows the steering message"
    );
    let commands = done
        .items
        .iter()
        .filter(|item| item.kind == ChatItemKind::CommandExecution)
        .count();
    assert!(
        commands < 3,
        "steering skipped the remaining commands ({commands} ran)"
    );
    assert!(
        done.controls
            .context
            .as_ref()
            .is_some_and(|context| context.used > 0)
    );
    assert!(!done.usage.is_empty(), "turn usage recorded");
    app.stop_session(&session.id).unwrap();
}
