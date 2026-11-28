//! How each agent is started over ACP, and the per-agent facts the protocol
//! leaves open: which session options carry the chat's permission choice.

use crate::error::Result;
use crate::model::{AgentKind, ChatConfigOption, ChatPermissionMode};

use super::super::context::RuntimeSpec;

pub struct Launch {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// OpenCode decides permissions from its config, which ACP cannot set per
/// session. It asks for every edit, shell command and outside path, and the
/// client answers from the chat's permission mode, so a mode switch takes
/// effect at once and the user still sees what was allowed.
const OPENCODE_CONFIG: &str =
    r#"{"permission":{"edit":"ask","shell":"ask","bash":"ask","external_directory":"ask"}}"#;

pub fn launch(agent: AgentKind, spec: &RuntimeSpec) -> Result<Launch> {
    let binary = spec.binary.display().to_string();
    let mut env = spec.env.clone();
    let argv = match agent {
        AgentKind::Opencode => {
            env.push(("OPENCODE_CONFIG_CONTENT".into(), OPENCODE_CONFIG.into()));
            vec![binary, "acp".into()]
        }
        AgentKind::Antigravity => vec![binary, "acp".into()],
        // The adapters are ACP servers themselves; `runtimes::agent_env` points
        // them at the agent CLI from Setup.
        AgentKind::Claude | AgentKind::Codex | AgentKind::Pi => vec![binary],
    };
    Ok(Launch { argv, env })
}

/// Option categories the permission picker owns for this agent: it sets them,
/// so the UI does not show them as separate selectors.
pub fn owned_categories(agent: AgentKind) -> &'static [&'static str] {
    match agent {
        AgentKind::Claude | AgentKind::Antigravity => &["mode"],
        AgentKind::Codex => &["mode", "collaboration_mode"],
        // OpenCode's Build/Plan agents and Pi's levels are the agent's own choice.
        AgentKind::Opencode | AgentKind::Pi => &[],
    }
}

/// The option values that carry a permission choice, per category and in
/// the order to apply them. Each list is tried in order; the first value the
/// agent offers wins.
pub fn settings_for(
    agent: AgentKind,
    permission: ChatPermissionMode,
) -> Vec<(&'static str, &'static [&'static str])> {
    use ChatPermissionMode::*;
    match agent {
        AgentKind::Claude => vec![(
            "mode",
            match permission {
                ApprovalRequired => &["default"],
                AutoAcceptEdits => &["acceptEdits"],
                Auto => &["auto", "acceptEdits"],
                Plan => &["plan"],
                FullAccess => &["bypassPermissions"],
            },
        )],
        // Codex plans through its collaboration mode; the sandbox stays as it is.
        AgentKind::Codex => match permission {
            Plan => vec![("collaboration_mode", &["plan"])],
            other => vec![
                ("collaboration_mode", &["default"]),
                (
                    "mode",
                    match other {
                        ApprovalRequired => &["read-only"],
                        AutoAcceptEdits => &["workspace-write"],
                        Auto => &["agent"],
                        _ => &["agent-full-access"],
                    },
                ),
            ],
        },
        AgentKind::Antigravity => vec![(
            "mode",
            match permission {
                ApprovalRequired => &["default"],
                AutoAcceptEdits => &["accept-edits"],
                // `agy acp` 1.5.x offers `default`, `accept-edits`, and `plan`.
                // Use native `auto` if a newer CLI advertises it; otherwise run
                // in `accept-edits` and auto-approve routine requests client-side.
                Auto => &["auto", "accept-edits"],
                Plan => &["plan"],
                FullAccess => &["auto", "accept-edits"],
            },
        )],
        AgentKind::Opencode | AgentKind::Pi => vec![],
    }
}

pub fn use_agent_default(agent: AgentKind, model: &str) -> bool {
    agent == AgentKind::Antigravity && model == crate::agents::antigravity::DEFAULT_MODEL
}

/// The agent enforces its own permission mode. Any requests it still sends
/// need a user decision.
pub fn native_permissions(agent: AgentKind) -> bool {
    agent == AgentKind::Antigravity
}

/// The permission choice the agent's current options stand for, so a change
/// the agent makes itself (leaving plan mode, say) shows in the picker.
pub fn permission_from(
    agent: AgentKind,
    options: &[ChatConfigOption],
) -> Option<ChatPermissionMode> {
    use ChatPermissionMode::*;
    let current = |category: &str| {
        options
            .iter()
            .find(|option| option.category.as_deref() == Some(category))
            .map(|option| option.current.as_str())
    };
    match agent {
        AgentKind::Claude => match current("mode")? {
            "default" => Some(ApprovalRequired),
            "acceptEdits" => Some(AutoAcceptEdits),
            "auto" => Some(Auto),
            "plan" => Some(Plan),
            "bypassPermissions" => Some(FullAccess),
            _ => None,
        },
        AgentKind::Codex => {
            if current("collaboration_mode") == Some("plan") {
                return Some(Plan);
            }
            match current("mode")? {
                "read-only" => Some(ApprovalRequired),
                "workspace-write" => Some(AutoAcceptEdits),
                "agent" => Some(Auto),
                "agent-full-access" => Some(FullAccess),
                _ => None,
            }
        }
        AgentKind::Antigravity => match current("mode")? {
            "default" => Some(ApprovalRequired),
            "accept-edits" => Some(AutoAcceptEdits),
            "auto" => Some(Auto),
            "plan" => Some(Plan),
            _ => None,
        },
        AgentKind::Opencode | AgentKind::Pi => None,
    }
}

/// Whether the agent's current options already satisfy `permission` (including
/// fallback native modes such as Antigravity `accept-edits` for `Auto` and
/// `FullAccess`), so routine `config_option_update` frames do not downgrade
/// the user's chosen permission mode.
pub fn mode_matches_permission(
    agent: AgentKind,
    options: &[ChatConfigOption],
    permission: ChatPermissionMode,
) -> bool {
    let settings = settings_for(agent, permission);
    if settings.is_empty() {
        return false;
    }
    settings.iter().all(|(category, wanted)| {
        options
            .iter()
            .find(|option| option.category.as_deref() == Some(*category))
            .is_some_and(|option| wanted.contains(&option.current.as_str()))
    })
}

/// Shell commands that remain gated behind user approval even in `Auto` mode.
fn is_risky_command(command: &str) -> bool {
    let lower = command.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    let normalized = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    const RISKY_TOKENS: &[&str] = &[
        "sudo ",
        "su -",
        "rm -rf",
        "rm -fr",
        "mkfs",
        "dd if=",
        "shutdown",
        "reboot",
        "poweroff",
        "halt",
        "chmod -r 777",
        "chown -r",
        "git push --force",
        "git push -f",
        "git reset --hard",
        "git clean -fd",
        "git clean -fx",
        "> /dev/",
        "| sh",
        "| bash",
        "| zsh",
        "kill -9",
        "killall",
        "pkill -9",
        "docker system prune",
        "drop table",
        "drop database",
    ];
    RISKY_TOKENS.iter().any(|pattern| normalized.contains(pattern))
}

/// Whether the client answers a permission request itself under this mode.
pub fn auto_allows_request(
    agent: AgentKind,
    permission: ChatPermissionMode,
    tool_kind: &str,
    command_text: &str,
    is_plan: bool,
) -> bool {
    if is_plan || tool_kind == "switch_mode" {
        return false;
    }
    match permission {
        ChatPermissionMode::ApprovalRequired | ChatPermissionMode::Plan => false,
        ChatPermissionMode::FullAccess => true,
        ChatPermissionMode::AutoAcceptEdits => {
            if native_permissions(agent) {
                false
            } else {
                matches!(
                    tool_kind,
                    "read" | "search" | "edit" | "delete" | "move" | "think"
                )
            }
        }
        ChatPermissionMode::Auto => {
            if agent == AgentKind::Antigravity {
                if tool_kind == "execute" {
                    !is_risky_command(command_text)
                } else {
                    true
                }
            } else if native_permissions(agent) {
                false
            } else {
                matches!(
                    tool_kind,
                    "read" | "search" | "edit" | "delete" | "move" | "think"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ChatConfigChoice;

    fn option(category: &str, current: &str) -> ChatConfigOption {
        ChatConfigOption {
            id: category.into(),
            name: category.into(),
            description: None,
            category: Some(category.into()),
            kind: "select".into(),
            current: current.into(),
            choices: vec![ChatConfigChoice {
                value: current.into(),
                name: current.into(),
                description: None,
                group: None,
            }],
            hidden: false,
        }
    }

    #[test]
    fn every_permission_choice_round_trips_through_the_agent_options() {
        for agent in [AgentKind::Claude, AgentKind::Codex] {
            for permission in [
                ChatPermissionMode::ApprovalRequired,
                ChatPermissionMode::AutoAcceptEdits,
                ChatPermissionMode::Auto,
                ChatPermissionMode::Plan,
                ChatPermissionMode::FullAccess,
            ] {
                let mut options = vec![
                    option("mode", "workspace-write"),
                    option("collaboration_mode", "default"),
                ];
                for (category, values) in settings_for(agent, permission) {
                    let target = options
                        .iter_mut()
                        .find(|option| option.category.as_deref() == Some(category))
                        .unwrap();
                    target.current = values[0].to_string();
                }
                assert_eq!(
                    permission_from(agent, &options),
                    Some(permission),
                    "{agent:?} {permission:?}"
                );
            }
        }
    }

    #[test]
    fn antigravity_permission_modes_use_the_cli_ids() {
        use ChatPermissionMode::*;
        for (permission, expected) in [
            (ApprovalRequired, &["default"][..]),
            (AutoAcceptEdits, &["accept-edits"][..]),
            (Auto, &["auto", "accept-edits"][..]),
            (Plan, &["plan"][..]),
            (FullAccess, &["auto", "accept-edits"][..]),
        ] {
            let agent = AgentKind::Antigravity;
            assert_eq!(settings_for(agent, permission), vec![("mode", expected)]);
            assert!(mode_matches_permission(
                agent,
                &[option("mode", expected.last().unwrap())],
                permission
            ));
        }
        // Fallback `accept-edits` keeps `Auto` / `FullAccess`, while leaving `Plan` syncs back.
        assert!(mode_matches_permission(
            AgentKind::Antigravity,
            &[option("mode", "accept-edits")],
            Auto
        ));
        assert!(!mode_matches_permission(
            AgentKind::Antigravity,
            &[option("mode", "accept-edits")],
            Plan
        ));
        // Auto approves routine commands/tools on Antigravity and gates risky commands + plans.
        assert!(auto_allows_request(
            AgentKind::Antigravity,
            Auto,
            "execute",
            "git status",
            false
        ));
        assert!(!auto_allows_request(
            AgentKind::Antigravity,
            Auto,
            "execute",
            "sudo rm -rf /tmp/foo",
            false
        ));
        assert!(!auto_allows_request(
            AgentKind::Antigravity,
            Auto,
            "other",
            "",
            true
        ));
        assert!(auto_allows_request(
            AgentKind::Antigravity,
            FullAccess,
            "execute",
            "sudo rm -rf /tmp/foo",
            false
        ));
        assert!(!auto_allows_request(
            AgentKind::Antigravity,
            ApprovalRequired,
            "execute",
            "ls",
            false
        ));
    }

    #[test]
    fn opencode_and_pi_keep_their_own_modes() {
        assert!(settings_for(AgentKind::Opencode, ChatPermissionMode::Plan).is_empty());
        assert!(owned_categories(AgentKind::Pi).is_empty());
        assert_eq!(
            permission_from(AgentKind::Opencode, &[option("mode", "plan")]),
            None
        );
    }
}
