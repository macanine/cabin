//! Hydrate Antigravity (`agy`) subagent transcripts from `agy`'s native brain logs.
//!
//! `agy acp` executes `invoke_subagent` asynchronously and does not stream child
//! session frames over ACP JSON-RPC. Instead it writes the parent conversation's
//! launch records and returned subagent reports to
//! `~/.gemini/antigravity-cli/brain/<parent_id>/.system_generated/logs/transcript.jsonl`,
//! and each subagent's own thinking, tool calls, command outputs, and final reply
//! to `~/.gemini/antigravity-cli/brain/<child_id>/.system_generated/logs/transcript.jsonl`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::super::context::{ChatContext, blank_item};
use crate::chat::{Sink, emit};
use crate::model::{
    AgentKind, AppEvent, ChatItem, ChatItemKind, ChatItemStatus, SubagentState, SubagentTask,
    ToolAction,
};
use crate::store::Store;

/// Bounded per-file read cap so a runaway log never exhausts memory.
const MAX_LOG_BYTES: u64 = 8 * 1024 * 1024;
const MAX_OUTPUT_CHARS: usize = 32_000;

fn default_brain_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".gemini/antigravity-cli/brain"))
}

fn transcript_path(brain_dir: &Path, conversation_id: &str, full: bool) -> PathBuf {
    let name = if full {
        "transcript_full.jsonl"
    } else {
        "transcript.jsonl"
    };
    brain_dir
        .join(conversation_id)
        .join(".system_generated/logs")
        .join(name)
}

fn read_bounded(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() == 0 || meta.len() > MAX_LOG_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn parse_rfc3339_millis(raw: Option<&str>) -> Option<i64> {
    let text = raw?.trim();
    if text.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// Unquote a JSON-stringified arg value (agy stores tool_calls args as JSON strings).
fn unquote_arg(value: &Value) -> Option<String> {
    match value {
        Value::String(raw) => {
            if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
                serde_json::from_str::<String>(raw)
                    .ok()
                    .or_else(|| Some(raw.clone()))
            } else {
                Some(raw.clone())
            }
        }
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn clean_command_output(raw: &str) -> String {
    let mut lines = raw.lines().peekable();
    while let Some(line) = lines.peek() {
        if line.starts_with("Created At:") || line.starts_with("Completed At:") {
            lines.next();
        } else {
            break;
        }
    }
    let text = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    if text.chars().count() > MAX_OUTPUT_CHARS {
        let mut out: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
        out.push_str("\n…[truncated]");
        out
    } else {
        text
    }
}

struct ParentLog {
    /// Child conversation IDs in launch order.
    launched_ids: Vec<String>,
    /// Role -> child conversation IDs reported by `manage_subagents`.
    by_role: HashMap<String, Vec<String>>,
    /// Child conversation ID -> report message sent back to parent via `send_message`.
    reports: HashMap<String, (String, Option<i64>)>,
}

fn parse_parent_log(text: &str) -> ParentLog {
    let mut launched_ids = Vec::new();
    let mut launched_seen = HashSet::new();
    let mut by_role: HashMap<String, Vec<String>> = HashMap::new();
    let mut reports: HashMap<String, (String, Option<i64>)> = HashMap::new();

    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = entry["type"].as_str().unwrap_or_default();
        let content = entry["content"].as_str().unwrap_or_default();
        if content.is_empty() {
            continue;
        }

        if kind == "GENERIC" {
            // 1. "Created the following subagent:\n{ \"conversationId\": \"...\" }"
            if let Some(pos) = content.find("Created the following subagent:")
                && let Some(brace) = content[pos..].find('{')
            {
                let slice = &content[pos + brace..];
                let mut de = serde_json::Deserializer::from_str(slice).into_iter::<Value>();
                if let Some(Ok(obj)) = de.next()
                    && let Some(cid) = obj["conversationId"].as_str().filter(|s| !s.is_empty())
                    && launched_seen.insert(cid.to_string())
                {
                    launched_ids.push(cid.to_string());
                }
            }
            // 2. "You have N active subagent(s):\n[{\"role\":...,\"conversationId\":...}]"
            if let Some(pos) = content.find("subagent(s):")
                && let Some(bracket) = content[pos..].find('[')
            {
                let slice = &content[pos + bracket..];
                let mut de = serde_json::Deserializer::from_str(slice).into_iter::<Value>();
                if let Some(Ok(Value::Array(items))) = de.next() {
                    for item in items {
                        let Some(cid) = item["conversationId"].as_str().filter(|s| !s.is_empty())
                        else {
                            continue;
                        };
                        if launched_seen.insert(cid.to_string()) {
                            launched_ids.push(cid.to_string());
                        }
                        if let Some(role) = item["role"].as_str().map(str::trim)
                            && !role.is_empty()
                        {
                            let list = by_role.entry(role.to_string()).or_default();
                            if !list.iter().any(|existing| existing == cid) {
                                list.push(cid.to_string());
                            }
                        }
                    }
                }
            }
        } else if kind == "SYSTEM_MESSAGE"
            && let Some(marker) = content.find("[Message]")
        {
            let tail = &content[marker..];
            if let Some(sender_pos) = tail.find("sender=")
                && let Some(content_pos) = tail.find(" content=")
                && content_pos > sender_pos
            {
                let sender = tail[sender_pos + "sender=".len()..content_pos]
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim();
                let body_raw = &tail[content_pos + " content=".len()..];
                let body = body_raw
                    .strip_suffix("</SYSTEM_MESSAGE>")
                    .unwrap_or(body_raw)
                    .trim();
                if !sender.is_empty() && !body.is_empty() {
                    let ts = parse_rfc3339_millis(entry["created_at"].as_str());
                    reports.insert(sender.to_string(), (body.to_string(), ts));
                }
            }
        }
    }

    ParentLog {
        launched_ids,
        by_role,
        reports,
    }
}

struct ChildTranscript {
    prompt_text: String,
    items: Vec<ChatItem>,
    started_at: Option<i64>,
    updated_at: Option<i64>,
    sent_report: bool,
}

fn tool_action_for_name(name: &str) -> Option<ToolAction> {
    match name {
        "run_command" => Some(ToolAction::Command),
        "read_file" | "view_file" | "list_dir" => Some(ToolAction::Read),
        "write_file" | "edit_file" | "replace_file_content" => Some(ToolAction::Edit),
        "grep_search" | "find_by_name" => Some(ToolAction::CodeSearch),
        "search_web" | "read_url_content" => Some(ToolAction::WebSearch),
        "update_steps" => Some(ToolAction::Plan),
        _ => None,
    }
}

fn parse_child_transcript(
    child_id: &str,
    compact_text: &str,
    full_text: Option<&str>,
    parent_report: Option<&str>,
    root: &Path,
) -> ChildTranscript {
    let full_lines: Vec<&str> = full_text.map(|t| t.lines().collect()).unwrap_or_default();
    let entries: Vec<Value> = compact_text
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let entry: Value = serde_json::from_str(line).ok()?;
            let truncated = entry["truncated_fields"]
                .as_array()
                .is_some_and(|arr| !arr.is_empty());
            if truncated
                && let Some(full_line) = full_lines.get(idx)
                && let Ok(full_entry) = serde_json::from_str::<Value>(full_line)
            {
                return Some(full_entry);
            }
            Some(entry)
        })
        .collect();

    let mut prompt_text = String::new();
    let mut items: Vec<ChatItem> = Vec::new();
    let mut started_at: Option<i64> = None;
    let mut updated_at: Option<i64> = None;
    let mut sent_report = parent_report.is_some();
    let mut report_text: Option<String> = parent_report.map(str::to_string);
    let mut pending_tool_indices: Vec<usize> = Vec::new();

    for (idx, entry) in entries.iter().enumerate() {
        let ts = parse_rfc3339_millis(entry["created_at"].as_str());
        if let Some(ms) = ts {
            started_at.get_or_insert(ms);
            updated_at = Some(updated_at.map_or(ms, |old| old.max(ms)));
        }

        let step = entry["step_index"].as_u64().unwrap_or(idx as u64);
        let kind = entry["type"].as_str().unwrap_or_default();
        let status = if entry["status"].as_str() == Some("RUNNING") {
            ChatItemStatus::Running
        } else {
            ChatItemStatus::Completed
        };

        if step == 0 && kind == "SYSTEM_MESSAGE" {
            prompt_text = entry["content"].as_str().unwrap_or_default().to_string();
            continue;
        }

        if kind == "PLANNER_RESPONSE" {
            pending_tool_indices.clear();

            if let Some(thinking) = entry["thinking"].as_str().map(str::trim)
                && !thinking.is_empty()
            {
                let mut item = blank_item(
                    &format!("agy-sub:{child_id}:think:{step}"),
                    ChatItemKind::Reasoning,
                    status,
                );
                item.title = "Thinking".into();
                item.text = thinking.to_string();
                if let Some(ms) = ts {
                    item.started_at = ms;
                    item.updated_at = ms;
                }
                items.push(item);
            }

            if let Some(calls) = entry["tool_calls"].as_array() {
                for (call_idx, call) in calls.iter().enumerate() {
                    let name = call["name"].as_str().unwrap_or("tool");
                    let args = &call["args"];
                    if name == "send_message" {
                        sent_report = true;
                        if report_text.is_none()
                            && let Some(msg) = args
                                .get("Message")
                                .and_then(unquote_arg)
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                        {
                            report_text = Some(msg);
                        }
                        continue;
                    }

                    let item_id = format!("agy-sub:{child_id}:tool:{step}:{call_idx}");
                    let mut item = if name == "run_command" {
                        let cmd = args
                            .get("CommandLine")
                            .and_then(unquote_arg)
                            .unwrap_or_default();
                        let mut item = blank_item(&item_id, ChatItemKind::CommandExecution, status);
                        item.title = "Command".into();
                        item.text = cmd.clone();
                        item.subject = cmd;
                        item
                    } else {
                        let mut item = blank_item(&item_id, ChatItemKind::Tool, status);
                        item.title = name.to_string();
                        item.action = tool_action_for_name(name);
                        item.text = args.to_string();
                        if let Some(path) = args
                            .get("Path")
                            .or_else(|| args.get("AbsolutePath"))
                            .or_else(|| args.get("TargetFile"))
                            .and_then(unquote_arg)
                        {
                            item.subject = path;
                        }
                        item
                    };
                    if let Some(ms) = ts {
                        item.started_at = ms;
                        item.updated_at = ms;
                    }
                    crate::chat_view::present(&mut item, root);
                    let pos = items.len();
                    items.push(item);
                    pending_tool_indices.push(pos);
                }
            }

            if let Some(content) = entry["content"].as_str().map(str::trim)
                && !content.is_empty()
            {
                let mut item = blank_item(
                    &format!("agy-sub:{child_id}:msg:{step}"),
                    ChatItemKind::AssistantMessage,
                    status,
                );
                item.text = content.to_string();
                if let Some(ms) = ts {
                    item.started_at = ms;
                    item.updated_at = ms;
                }
                items.push(item);
            }
        } else if kind == "GENERIC"
            && let Some(content) = entry["content"].as_str()
        {
            let cleaned = clean_command_output(content);
            if cleaned.starts_with("Message sent to ") {
                sent_report = true;
                continue;
            }
            if !cleaned.is_empty() && !pending_tool_indices.is_empty() {
                let target_idx = pending_tool_indices.remove(0);
                if let Some(tool_item) = items.get_mut(target_idx) {
                    tool_item.detail = cleaned;
                    if let Some(ms) = ts {
                        tool_item.updated_at = ms;
                    }
                }
            }
        }
    }

    // Place the structured report sent back to the parent as the primary closing
    // assistant message (avoiding duplicating if it matches the last message).
    if let Some(report) = report_text.filter(|text| !text.trim().is_empty()) {
        let duplicate = items
            .iter()
            .rev()
            .find(|item| item.kind == ChatItemKind::AssistantMessage)
            .is_some_and(|item| item.text.trim() == report.trim());
        if !duplicate {
            let mut item = blank_item(
                &format!("agy-sub:{child_id}:report"),
                ChatItemKind::AssistantMessage,
                ChatItemStatus::Completed,
            );
            item.text = report;
            if let Some(ms) = updated_at {
                item.started_at = ms;
                item.updated_at = ms;
            }
            items.push(item);
        }
    }

    for (ordinal, item) in items.iter_mut().enumerate() {
        item.ordinal = (ordinal + 1) as u32;
        item.revision = 1;
    }

    ChildTranscript {
        prompt_text,
        items,
        started_at,
        updated_at,
        sent_report,
    }
}

fn prompt_needle(detail: &str) -> Option<String> {
    let line = detail
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let needle: String = line.chars().take(64).collect();
    (needle.chars().count() >= 8).then_some(needle)
}

fn load_parent_and_children(
    brain_dir: &Path,
    native_session_id: &str,
    root: &Path,
) -> Option<(ParentLog, HashMap<String, ChildTranscript>)> {
    if native_session_id.is_empty() {
        return None;
    }
    let parent_text = read_bounded(&transcript_path(brain_dir, native_session_id, false))?;
    let parent = parse_parent_log(&parent_text);
    if parent.launched_ids.is_empty() {
        return None;
    }

    let mut children: HashMap<String, ChildTranscript> = HashMap::new();
    for cid in &parent.launched_ids {
        let Some(compact) = read_bounded(&transcript_path(brain_dir, cid, false)) else {
            continue;
        };
        let full = read_bounded(&transcript_path(brain_dir, cid, true));
        let report = parent.reports.get(cid).map(|(text, _)| text.as_str());
        children.insert(
            cid.clone(),
            parse_child_transcript(cid, &compact, full.as_deref(), report, root),
        );
    }
    Some((parent, children))
}

fn match_tasks_to_children(
    parent: &ParentLog,
    children: &HashMap<String, ChildTranscript>,
    tasks: &[SubagentTask],
) -> Vec<(usize, String)> {
    let mut used_children: HashSet<String> = HashSet::new();
    let mut matched: Vec<(usize, String)> = Vec::new();

    // Pass 1: exact prompt needle match inside child step 0 SYSTEM_MESSAGE.
    for (idx, task) in tasks.iter().enumerate() {
        let Some(needle) = prompt_needle(&task.detail) else {
            continue;
        };
        if let Some(cid) = parent.launched_ids.iter().find(|cid| {
            !used_children.contains(*cid)
                && children
                    .get(*cid)
                    .is_some_and(|child| child.prompt_text.contains(&needle))
        }) {
            used_children.insert(cid.clone());
            matched.push((idx, cid.clone()));
        }
    }

    // Pass 2: exact role match from `manage_subagents`.
    let matched_indices: HashSet<usize> = matched.iter().map(|(i, _)| *i).collect();
    for (idx, task) in tasks.iter().enumerate() {
        if matched_indices.contains(&idx) {
            continue;
        }
        if let Some(cids) = parent.by_role.get(task.label.trim())
            && let Some(cid) = cids.iter().find(|cid| !used_children.contains(*cid))
        {
            used_children.insert(cid.clone());
            matched.push((idx, cid.clone()));
        }
    }

    // Pass 3: launch-order fallback for remaining unmatched tasks.
    let matched_indices: HashSet<usize> = matched.iter().map(|(i, _)| *i).collect();
    let mut remaining_cids = parent
        .launched_ids
        .iter()
        .filter(|cid| !used_children.contains(*cid));
    for idx in 0..tasks.len() {
        if matched_indices.contains(&idx) {
            continue;
        }
        if let Some(cid) = remaining_cids.next() {
            matched.push((idx, cid.clone()));
        }
    }
    matched
}

/// Resolve an Antigravity child conversation UUID (`<uuid>:<call_id>` prefix)
/// to the launched parent `SubagentTask.id`.
pub(super) fn resolve_child_task_id(
    native_session_id: &str,
    child_uuid: &str,
    ctx: &ChatContext,
    root: &Path,
) -> Option<String> {
    let brain_dir = default_brain_dir()?;
    let Ok(tasks) = ctx.store.chat_subagents(&ctx.id) else {
        return None;
    };
    if tasks.is_empty() {
        return None;
    }
    let (parent, children) = load_parent_and_children(&brain_dir, native_session_id, root)?;
    let matched = match_tasks_to_children(&parent, &children, &tasks);
    matched
        .into_iter()
        .find(|(_, cid)| cid == child_uuid)
        .map(|(idx, _)| tasks[idx].id.clone())
}

fn hydrate_tasks(
    brain_dir: &Path,
    native_session_id: &str,
    root: &Path,
    tasks: &[SubagentTask],
    parent_running: bool,
) -> Vec<SubagentTask> {
    if tasks.is_empty() {
        return Vec::new();
    }
    let Some((parent, children)) = load_parent_and_children(brain_dir, native_session_id, root)
    else {
        return Vec::new();
    };
    let matched = match_tasks_to_children(&parent, &children, tasks);

    let mut updated = Vec::new();
    for (idx, cid) in matched {
        let Some(child) = children.get(&cid) else {
            continue;
        };
        let current = &tasks[idx];
        let mut next = current.clone();
        if let Some(start) = child.started_at {
            next.started_at = start;
        }
        let report_ts = parent.reports.get(&cid).and_then(|(_, ts)| *ts);
        if let Some(end) = report_ts.or(child.updated_at) {
            next.updated_at = end.max(next.started_at);
        }
        next.state = if child.sent_report || !parent_running {
            if current.state == SubagentState::Failed {
                SubagentState::Failed
            } else {
                SubagentState::Completed
            }
        } else {
            SubagentState::Active
        };
        if !child.items.is_empty() {
            next.items = child.items.clone();
        }
        if next != *current {
            updated.push(next);
        }
    }
    updated
}

/// Live sync called from an active Antigravity ACP session.
pub fn sync_live(native_session_id: &str, root: &Path, ctx: &ChatContext, parent_running: bool) {
    let Some(brain_dir) = default_brain_dir() else {
        return;
    };
    let Ok(tasks) = ctx.store.chat_subagents(&ctx.id) else {
        return;
    };
    for task in hydrate_tasks(&brain_dir, native_session_id, root, &tasks, parent_running) {
        if let Ok(saved) = ctx.store.put_chat_subagent(&ctx.id, task) {
            emit(
                &ctx.sink,
                AppEvent::ChatSubagent {
                    id: ctx.id.clone(),
                    task: saved,
                },
            );
        }
    }
}

/// Snapshot backfill for stored Antigravity sessions (e.g. sessions completed
/// before or between live updates).
pub fn enrich_stored(
    store: &Store,
    sink: &std::sync::RwLock<Option<Sink>>,
    session_id: &str,
    parent_running: bool,
) {
    let Ok(session) = store.session(session_id) else {
        return;
    };
    if session.agent != Some(AgentKind::Antigravity) {
        return;
    }
    let Ok(Some(native_id)) = store.chat_thread(session_id) else {
        return;
    };
    let Ok(tasks) = store.chat_subagents(session_id) else {
        return;
    };
    if tasks.is_empty()
        || !tasks
            .iter()
            .any(|t| t.items.is_empty() || t.state == SubagentState::Active)
    {
        return;
    }
    let Some(brain_dir) = default_brain_dir() else {
        return;
    };
    let root = session
        .worktree
        .as_ref()
        .map(|wt| PathBuf::from(&wt.path))
        .or_else(|| {
            store
                .workspace(&session.workspace_id)
                .ok()
                .map(|ws| PathBuf::from(ws.path))
        })
        .unwrap_or_else(|| PathBuf::from("/"));

    for task in hydrate_tasks(&brain_dir, &native_id, &root, &tasks, parent_running) {
        if let Ok(saved) = store.put_chat_subagent(session_id, task) {
            emit(
                sink,
                AppEvent::ChatSubagent {
                    id: session_id.to_string(),
                    task: saved,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hydrates_antigravity_subagent_steps_and_report_from_brain_logs() {
        let tmp = std::env::temp_dir().join(format!(
            "cabin-agy-sub-test-{}",
            crate::util::now_millis()
        ));
        let parent_id = "parent-uuid-1";
        let child_id = "child-uuid-1";
        let parent_dir = tmp.join(parent_id).join(".system_generated/logs");
        let child_dir = tmp.join(child_id).join(".system_generated/logs");
        std::fs::create_dir_all(&parent_dir).unwrap();
        std::fs::create_dir_all(&child_dir).unwrap();

        let parent_lines = [
            json!({
                "step_index": 3,
                "type": "GENERIC",
                "status": "DONE",
                "created_at": "2026-11-28T04:21:06Z",
                "content": format!("Created At: x\nCreated the following subagent:\n{{\"conversationId\": \"{child_id}\"}}")
            }),
            json!({
                "step_index": 12,
                "type": "SYSTEM_MESSAGE",
                "status": "DONE",
                "created_at": "2026-11-28T04:21:33Z",
                "content": format!("<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-28T04:21:33Z sender={child_id} priority=HIGH content=### Probe Report\n- processes: 170\n</SYSTEM_MESSAGE>")
            }),
        ];
        std::fs::write(
            parent_dir.join("transcript.jsonl"),
            parent_lines
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        let child_lines = [
            json!({
                "step_index": 0,
                "type": "SYSTEM_MESSAGE",
                "status": "DONE",
                "created_at": "2026-11-28T04:21:10Z",
                "content": "探测本机的进程、系统服务与定时任务信息"
            }),
            json!({
                "step_index": 1,
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-28T04:21:11Z",
                "thinking": "Checking process counts and systemd services.",
                "tool_calls": [{
                    "name": "run_command",
                    "args": {"CommandLine": "\"ps -e | wc -l\""}
                }]
            }),
            json!({
                "step_index": 2,
                "type": "GENERIC",
                "status": "DONE",
                "created_at": "2026-11-28T04:21:14Z",
                "content": "Created At: x\nCompleted At: y\n170\n"
            }),
            json!({
                "step_index": 3,
                "type": "PLANNER_RESPONSE",
                "status": "DONE",
                "created_at": "2026-11-28T04:21:32Z",
                "content": "已完成探测并发送给父级代理。"
            }),
        ];
        std::fs::write(
            child_dir.join("transcript.jsonl"),
            child_lines
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();

        let tasks = vec![SubagentTask {
            id: "acp-agent:tool:call_1".into(),
            label: "Process & Service Prober".into(),
            subagent_type: Some("self".into()),
            detail: "探测本机的进程、系统服务与定时任务信息".into(),
            state: SubagentState::Active,
            origin: "acp".into(),
            started_at: 1000,
            updated_at: 1000,
            items: vec![],
        }];

        let hydrated = hydrate_tasks(&tmp, parent_id, Path::new("/workspace"), &tasks, true);
        assert_eq!(hydrated.len(), 1);
        let sub = &hydrated[0];
        assert_eq!(sub.state, SubagentState::Completed);
        assert!(sub.updated_at > sub.started_at);
        assert!(sub.items.iter().any(|i| i.kind == ChatItemKind::Reasoning));
        let cmd = sub
            .items
            .iter()
            .find(|i| i.kind == ChatItemKind::CommandExecution)
            .unwrap();
        assert_eq!(cmd.text, "ps -e | wc -l");
        assert_eq!(cmd.detail, "170");
        let last_msg = sub
            .items
            .iter()
            .rev()
            .find(|i| i.kind == ChatItemKind::AssistantMessage)
            .unwrap();
        assert!(last_msg.text.contains("### Probe Report"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
