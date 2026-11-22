//! Presentation facts for chat items, computed once on the guest side so the
//! UI can group and summarise work without parsing provider payloads.
//!
//! Adapted from T3 Code's work-log presentation
//! (packages/client-runtime/src/work-log, MIT, Copyright (c) 2026 T3 Tools Inc.):
//! tool calls are classified into a few actions (read, edit, command, …) and
//! described by one short subject line.
use std::path::Path;

use serde_json::Value;

use crate::model::{ChatItem, ChatItemKind, ToolAction};

const SUBJECT_MAX: usize = 240;

/// Fill `action`, `subject` and `files` for tool-like items.
pub fn present(item: &mut ChatItem, root: &Path) {
    let action = match item.kind {
        ChatItemKind::CommandExecution => Some(command_action(&unwrap_shell(&item.text))),
        ChatItemKind::FileChange => Some(ToolAction::Edit),
        ChatItemKind::Tool => Some(tool_action(&item.title, &item.text)),
        _ => None,
    };
    item.action = action;
    let Some(action) = action else {
        item.subject.clear();
        item.files.clear();
        return;
    };
    let input = serde_json::from_str::<Value>(&item.text).ok();
    let field = |names: &[&str]| input.as_ref().and_then(|value| string_field(value, names));

    item.files = match item.kind {
        ChatItemKind::FileChange if !item.changes.is_empty() => item
            .changes
            .iter()
            .map(|change| change.path.clone())
            .collect(),
        ChatItemKind::FileChange => change_paths(&item.detail)
            .into_iter()
            .map(|path| relative(&path, root))
            .collect(),
        _ if action == ToolAction::Edit => edit_paths(input.as_ref())
            .into_iter()
            .map(|path| relative(&path, root))
            .collect(),
        _ => Vec::new(),
    };

    let command = || {
        if item.kind == ChatItemKind::CommandExecution {
            Some(unwrap_shell(&item.text))
        } else {
            field(&["command", "cmd"]).map(|command| unwrap_shell(&command))
        }
    };
    let subject = match action {
        ToolAction::Command => command(),
        ToolAction::Read => field(&["file_path", "filePath", "path", "notebook_path", "file"])
            .or_else(|| command().map(|command| command_target(&command).unwrap_or(command)))
            .map(|path| relative(&path, root)),
        ToolAction::Edit => (!item.files.is_empty()).then(|| item.files.join(", ")),
        ToolAction::CodeSearch => {
            let pattern = field(&["pattern", "query", "regex", "glob"]);
            let path = field(&["path", "directory", "dir"]).map(|path| relative(&path, root));
            match (pattern, path) {
                (Some(pattern), Some(path)) => Some(format!("{pattern} in {path}")),
                (Some(pattern), None) => Some(pattern),
                (None, Some(path)) => Some(path),
                (None, None) => command().map(|command| {
                    command_search(&command)
                        .map(|(pattern, path)| match path {
                            Some(path) => format!("{pattern} in {}", relative(&path, root)),
                            None => pattern,
                        })
                        .unwrap_or(command)
                }),
            }
        }
        ToolAction::WebSearch => field(&["url", "query", "q"]).or_else(|| {
            serde_json::from_str::<Value>(&item.detail)
                .ok()
                .and_then(|value| string_field(&value, &["query", "url"]))
        }),
        ToolAction::Agent => field(&["description", "prompt", "task"]),
        ToolAction::Plan => None,
        ToolAction::Other => None,
    };
    item.subject = subject.map(|text| one_line(&text)).unwrap_or_default();
}

/// Strip `bash -lc '…'` style wrappers so the real command shows.
pub fn unwrap_shell(command: &str) -> String {
    let trimmed = command.trim();
    for shell in [
        "/bin/bash",
        "bash",
        "/bin/sh",
        "sh",
        "/bin/zsh",
        "zsh",
        "/usr/bin/env bash",
    ] {
        for flag in [" -lc ", " -c ", " -l -c "] {
            let prefix = format!("{shell}{flag}");
            if let Some(rest) = trimmed.strip_prefix(&prefix) {
                let rest = rest.trim();
                for quote in ['\'', '"'] {
                    if rest.len() >= 2 && rest.starts_with(quote) && rest.ends_with(quote) {
                        return rest[1..rest.len() - 1].to_string();
                    }
                }
                return rest.to_string();
            }
        }
    }
    trimmed.to_string()
}

/// A shell command that only looks at files counts as a read or a search.
fn command_action(command: &str) -> ToolAction {
    let first = command
        .split(['|', ';', '&'])
        .next()
        .unwrap_or("")
        .trim_start_matches(|c: char| c == '(' || c.is_whitespace());
    let mut words = first
        .split_whitespace()
        .skip_while(|word| word.contains('=') && !word.starts_with('-'));
    let program = words.next().unwrap_or("").rsplit('/').next().unwrap_or("");
    match program {
        "cat" | "head" | "tail" | "nl" | "less" | "bat" | "wc" => ToolAction::Read,
        "sed" if first.contains(" -n") => ToolAction::Read,
        "rg" | "grep" | "egrep" | "fgrep" | "ag" | "ack" | "find" | "fd" | "ls" | "tree" => {
            ToolAction::CodeSearch
        }
        _ => ToolAction::Command,
    }
}

/// Words of the first command in a pipeline, quotes removed.
fn command_words(command: &str) -> Vec<String> {
    let first = command.split(['|', ';', '&']).next().unwrap_or("");
    first
        .split_whitespace()
        .skip_while(|word| word.contains('=') && !word.starts_with('-'))
        .map(|word| word.trim_matches(|c| c == '\'' || c == '"').to_string())
        .collect()
}

/// The file a read-only command looks at: its last plain argument.
fn command_target(command: &str) -> Option<String> {
    let words = command_words(command);
    let program = words.first()?.rsplit('/').next()?.to_string();
    words.into_iter().skip(1).rfind(|word| {
        !word.starts_with('-')
            && !(program == "sed"
                && word.ends_with('p')
                && word[..word.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ','))
            && !word.chars().all(|c| c.is_ascii_digit() || c == ',')
    })
}

/// `rg -n pattern path` → (pattern, path); `ls dir` / `find dir` → (dir, None).
fn command_search(command: &str) -> Option<(String, Option<String>)> {
    let words = command_words(command);
    let program = words.first()?.rsplit('/').next()?.to_string();
    let mut plain = words
        .into_iter()
        .skip(1)
        .filter(|word| !word.starts_with('-'));
    match program.as_str() {
        "rg" | "grep" | "egrep" | "fgrep" | "ag" | "ack" => {
            let pattern = plain.next()?;
            Some((pattern, plain.next()))
        }
        _ => plain.next().map(|path| (path, None)),
    }
}

fn tool_action(title: &str, input: &str) -> ToolAction {
    // MCP tools are named `mcp__server__tool`; classify by the tool itself.
    let name = title
        .rsplit("__")
        .next()
        .unwrap_or(title)
        .to_ascii_lowercase()
        .replace(['_', '-', ' '], "");
    match name.as_str() {
        "read" | "readfile" | "view" | "viewimage" | "imageview" | "notebookread" | "cat" => {
            ToolAction::Read
        }
        "edit"
        | "multiedit"
        | "write"
        | "writefile"
        | "notebookedit"
        | "patch"
        | "applypatch"
        | "strreplace"
        | "strreplacebasededittool"
        | "create"
        | "filechange" => ToolAction::Edit,
        "bash" | "shell" | "exec" | "run" | "terminal" | "commandexecution" | "localshell" => {
            let command = serde_json::from_str::<Value>(input)
                .ok()
                .and_then(|value| string_field(&value, &["command", "cmd"]))
                .unwrap_or_default();
            command_action(&unwrap_shell(&command))
        }
        "grep" | "glob" | "find" | "ls" | "list" | "search" | "codesearch" | "rg"
        | "filesearch" => ToolAction::CodeSearch,
        "webfetch" | "websearch" | "fetch" | "browse" => ToolAction::WebSearch,
        "todowrite" | "todoread" | "updateplan" | "plan" => ToolAction::Plan,
        "task" | "agent" | "subagent" | "spawnagent" => ToolAction::Agent,
        _ => ToolAction::Other,
    }
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    let object = value.as_object()?;
    names.iter().find_map(|name| match object.get(*name)? {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        Value::Array(parts) if !parts.is_empty() && parts.iter().all(Value::is_string) => Some(
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    })
}

fn edit_paths(input: Option<&Value>) -> Vec<String> {
    let Some(input) = input else {
        return Vec::new();
    };
    if let Some(path) = string_field(input, &["file_path", "filePath", "path", "notebook_path"]) {
        return vec![path];
    }
    // apply_patch style input: "*** Update File: src/a.rs"
    let patch = string_field(input, &["patch", "input", "patchText"]).unwrap_or_default();
    patch_paths(&patch)
}

fn patch_paths(patch: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in patch.lines() {
        for marker in ["*** Update File: ", "*** Add File: ", "*** Delete File: "] {
            if let Some(path) = line.strip_prefix(marker) {
                push_unique(&mut paths, path.trim());
            }
        }
    }
    paths
}

/// Codex `fileChange.changes`: an array of `{path, kind, diff}` or a map
/// keyed by path.
fn change_paths(detail: &str) -> Vec<String> {
    let mut paths = Vec::new();
    match serde_json::from_str::<Value>(detail) {
        Ok(Value::Array(changes)) => {
            for change in changes {
                if let Some(path) = string_field(&change, &["path", "file", "filePath"]) {
                    push_unique(&mut paths, &path);
                }
            }
        }
        Ok(Value::Object(map)) => {
            if let Some(path) = string_field(&Value::Object(map.clone()), &["path", "filePath"]) {
                push_unique(&mut paths, &path);
            } else {
                for key in map.keys() {
                    push_unique(&mut paths, key);
                }
            }
        }
        _ => paths = patch_paths(detail),
    }
    paths
}

fn push_unique(paths: &mut Vec<String>, path: &str) {
    if !path.is_empty() && !paths.iter().any(|known| known == path) {
        paths.push(path.to_string());
    }
}

fn relative(path: &str, root: &Path) -> String {
    Path::new(path)
        .strip_prefix(root)
        .ok()
        .map(|rest| rest.display().to_string())
        .filter(|rest| !rest.is_empty())
        .unwrap_or_else(|| path.to_string())
}

fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let more = text.trim().lines().count() > 1;
    let mut out: String = line.chars().take(SUBJECT_MAX).collect();
    if more || line.chars().count() > SUBJECT_MAX {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChatItemStatus;

    fn item(kind: ChatItemKind, title: &str, text: &str, detail: &str) -> ChatItem {
        ChatItem {
            id: "i".into(),
            ordinal: 0,
            revision: 0,
            turn_id: String::new(),
            kind,
            status: ChatItemStatus::Completed,
            title: title.into(),
            text: text.into(),
            detail: detail.into(),
            request_id: None,
            questions: vec![],
            attachments: vec![],
            started_at: 0,
            updated_at: 0,
            action: None,
            subject: String::new(),
            files: vec![],
            approval: None,
            changes: vec![],
            task_id: None,
        }
    }

    fn present_one(mut value: ChatItem) -> ChatItem {
        present(&mut value, Path::new("/home/dev/app"));
        value
    }

    #[test]
    fn shell_wrappers_are_removed() {
        assert_eq!(
            unwrap_shell("/bin/bash -lc 'rg -n foo src'"),
            "rg -n foo src"
        );
        assert_eq!(unwrap_shell("bash -c \"npm test\""), "npm test");
        assert_eq!(unwrap_shell("cargo build"), "cargo build");
    }

    #[test]
    fn commands_that_only_look_are_reads_or_searches() {
        let read = present_one(item(
            ChatItemKind::CommandExecution,
            "Command",
            "/bin/bash -lc \"sed -n '1,80p' src/main.rs\"",
            "",
        ));
        assert_eq!(read.action, Some(ToolAction::Read));
        assert_eq!(read.subject, "src/main.rs");
        let tail = present_one(item(
            ChatItemKind::CommandExecution,
            "Command",
            "sed -n 5p scripts/setup",
            "",
        ));
        assert_eq!(tail.subject, "scripts/setup");
        let cat = present_one(item(
            ChatItemKind::CommandExecution,
            "Command",
            "cat /home/dev/app/README.md",
            "",
        ));
        assert_eq!(cat.subject, "README.md");
        let search = present_one(item(
            ChatItemKind::CommandExecution,
            "Command",
            "bash -lc 'rg -n TODO src'",
            "",
        ));
        assert_eq!(
            (search.action, search.subject.as_str()),
            (Some(ToolAction::CodeSearch), "TODO in src")
        );
        let list = present_one(item(
            ChatItemKind::CommandExecution,
            "Command",
            "ls -la core",
            "",
        ));
        assert_eq!(list.subject, "core");
        let run = present_one(item(
            ChatItemKind::CommandExecution,
            "Command",
            "FOO=1 cargo test",
            "",
        ));
        assert_eq!(run.action, Some(ToolAction::Command));
    }

    #[test]
    fn primary_opencode_and_pi_tools_are_classified() {
        let read = present_one(item(
            ChatItemKind::Tool,
            "Read",
            r#"{"file_path":"/home/dev/app/src/lib.rs"}"#,
            "",
        ));
        assert_eq!(
            (read.action, read.subject.as_str()),
            (Some(ToolAction::Read), "src/lib.rs")
        );
        let edit = present_one(item(
            ChatItemKind::Tool,
            "edit",
            r#"{"filePath":"/home/dev/app/a.ts","oldString":"x"}"#,
            "",
        ));
        assert_eq!(
            (edit.action, edit.files.clone()),
            (Some(ToolAction::Edit), vec!["a.ts".to_string()])
        );
        let grep = present_one(item(
            ChatItemKind::Tool,
            "Grep",
            r#"{"pattern":"fn main","path":"/home/dev/app/src"}"#,
            "",
        ));
        assert_eq!(
            (grep.action, grep.subject.as_str()),
            (Some(ToolAction::CodeSearch), "fn main in src")
        );
        let bash = present_one(item(
            ChatItemKind::Tool,
            "bash",
            r#"{"command":"ls -la"}"#,
            "",
        ));
        assert_eq!(bash.action, Some(ToolAction::CodeSearch));
        let todo = present_one(item(ChatItemKind::Tool, "TodoWrite", "{}", ""));
        assert_eq!(todo.action, Some(ToolAction::Plan));
        let mcp = present_one(item(
            ChatItemKind::Tool,
            "mcp__github__create_issue",
            "{}",
            "",
        ));
        assert_eq!(mcp.action, Some(ToolAction::Other));
    }

    #[test]
    fn codex_file_changes_list_their_paths() {
        let change = present_one(item(
            ChatItemKind::FileChange,
            "File changes",
            "",
            r#"[{"path":"/home/dev/app/src/a.rs","kind":{"type":"update"}},{"path":"README.md","kind":{"type":"add"}}]"#,
        ));
        assert_eq!(
            change.files,
            vec!["src/a.rs".to_string(), "README.md".to_string()]
        );
        assert_eq!(change.subject, "src/a.rs, README.md");
        let patch = present_one(item(
            ChatItemKind::Tool,
            "apply_patch",
            r#"{"input":"*** Begin Patch\n*** Update File: x.rs\n*** Add File: y.rs\n"}"#,
            "",
        ));
        assert_eq!(patch.files, vec!["x.rs".to_string(), "y.rs".to_string()]);
    }

    #[test]
    fn messages_have_no_action_and_subjects_are_one_line() {
        let message = present_one(item(ChatItemKind::AssistantMessage, "", "hello", ""));
        assert_eq!(message.action, None);
        let long = present_one(item(
            ChatItemKind::CommandExecution,
            "",
            "echo one\necho two",
            "",
        ));
        assert_eq!(long.subject, "echo one…");
    }
}
