use std::fs;
use std::io::Write;

use rusqlite::Connection;
use serde_json::json;

use crate::{Filter, Price, Usage, sources};

fn filter() -> Filter {
    Filter {
        limit: 100,
        ..Filter::default()
    }
}

fn write_lines(path: &std::path::Path, lines: &[serde_json::Value]) {
    let mut file = fs::File::create(path).unwrap();
    for line in lines {
        writeln!(file, "{line}").unwrap();
    }
}

#[test]
fn imports_claude_pi_and_codex_without_double_counting() {
    let dir = tempfile::tempdir().unwrap();
    let mut usage = Usage::open(&dir.path().join("usage.db")).unwrap();
    let mut sync = crate::SyncResult::default();
    let claude = dir.path().join("claude.jsonl");
    let assistant = |output, stop: Option<&str>| json!({"type":"assistant","sessionId":"s", "timestamp":"2026-07-10T03:00:00Z", "message":{"id":"m1","model":"claude-opus-4-6","stop_reason":stop,"usage":{"input_tokens":100,"output_tokens":output,"cache_read_input_tokens":30,"cache_creation_input_tokens":20}}});
    write_lines(
        &claude,
        &[assistant(1, None), assistant(12, Some("end_turn"))],
    );
    sources::sync_file(&mut usage.conn, "claude", &claude, &mut sync).unwrap();
    sources::sync_file(&mut usage.conn, "claude", &claude, &mut sync).unwrap();
    assert_eq!(sync.imported, 1);
    let first = usage.dashboard(&filter()).unwrap();
    assert_eq!(first.summary.requests, 1);
    assert_eq!(
        (
            first.summary.input,
            first.summary.output,
            first.summary.cache_read,
            first.summary.cache_write
        ),
        (100, 12, 30, 20)
    );

    let pi = dir.path().join("pi.jsonl");
    write_lines(
        &pi,
        &[
            json!({"type":"session","id":"pi-s"}),
            json!({"type":"message","id":"pi-m","timestamp":"2026-07-10T04:00:00Z","message":{"role":"assistant","model":"pi-model","provider":"provider","usage":{"input":5,"output":3,"cacheRead":4,"cacheWrite":2,"cost":{"total":0.02}}}}),
        ],
    );
    sources::sync_file(&mut usage.conn, "pi", &pi, &mut sync).unwrap();
    let codex = dir.path().join("codex.jsonl");
    let event = |input, cached, output, last_input, last_cached, last_output| json!({"type":"event_msg","timestamp":"2026-07-10T05:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":input,"cached_input_tokens":cached,"output_tokens":output},"last_token_usage":{"input_tokens":last_input,"cached_input_tokens":last_cached,"output_tokens":last_output}}}});
    write_lines(
        &codex,
        &[
            json!({"type":"session_meta","payload":{"id":"cx"}}),
            json!({"type":"turn_context","payload":{"model":"gpt-5"}}),
            event(100, 80, 10, 100, 80, 10),
            event(100, 80, 10, 100, 80, 10),
            event(160, 100, 15, 60, 20, 5),
        ],
    );
    sources::sync_file(&mut usage.conn, "codex", &codex, &mut sync).unwrap();
    sources::sync_file(&mut usage.conn, "codex", &codex, &mut sync).unwrap();
    let all = usage.dashboard(&filter()).unwrap();
    assert_eq!(all.total, 4);
    assert_eq!(all.summary.unpriced_requests, 3);
    assert_eq!(all.summary.cost, 0.02);
    assert_eq!(
        (
            all.summary.input,
            all.summary.output,
            all.summary.cache_read,
            all.summary.cache_write
        ),
        (165, 30, 134, 22)
    );
    let codex_only = usage
        .dashboard(&Filter {
            agent: Some("codex".into()),
            ..filter()
        })
        .unwrap();
    assert_eq!(
        (
            codex_only.total,
            codex_only.summary.input,
            codex_only.summary.cache_read
        ),
        (2, 60, 100)
    );
    assert!(
        all.logs
            .iter()
            .any(|r| r.agent == "pi" && r.cost == Some(0.02))
    );
}

#[test]
fn codex_fork_does_not_count_inherited_usage_twice() {
    let dir = tempfile::tempdir().unwrap();
    let mut usage = Usage::open(&dir.path().join("usage.db")).unwrap();
    let path = dir.path().join("fork.jsonl");
    let event = |time: &str, input, last| json!({"type":"event_msg","timestamp":time,"payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":input},"last_token_usage":{"input_tokens":last}}}});
    write_lines(
        &path,
        &[
            json!({"type":"session_meta","timestamp":"2026-07-10T03:00:00Z","payload":{"id":"child","forked_from_id":"parent"}}),
            event("2026-07-10T02:59:00Z", 100, 100),
            event("2026-07-10T03:01:00Z", 150, 50),
        ],
    );
    sources::sync_file(
        &mut usage.conn,
        "codex",
        &path,
        &mut crate::SyncResult::default(),
    )
    .unwrap();
    let result = usage.dashboard(&filter()).unwrap();
    assert_eq!((result.total, result.summary.input), (1, 50));
}

#[test]
fn incomplete_line_is_retried_and_updated() {
    let dir = tempfile::tempdir().unwrap();
    let mut usage = Usage::open(&dir.path().join("usage.db")).unwrap();
    let path = dir.path().join("active.jsonl");
    let line = json!({"type":"assistant","sessionId":"s", "message":{"id":"m","model":"x","usage":{"input_tokens":5,"output_tokens":1}}}).to_string();
    let split = line.len() / 2;
    fs::write(&path, &line[..split]).unwrap();
    let mut sync = crate::SyncResult::default();
    sources::sync_file(&mut usage.conn, "claude", &path, &mut sync).unwrap();
    assert_eq!(usage.dashboard(&filter()).unwrap().total, 0);
    write!(
        fs::OpenOptions::new().append(true).open(&path).unwrap(),
        "{}",
        &line[split..]
    )
    .unwrap();
    sources::sync_file(&mut usage.conn, "claude", &path, &mut sync).unwrap();
    assert_eq!(usage.dashboard(&filter()).unwrap().total, 1);
    writeln!(fs::OpenOptions::new().append(true).open(&path).unwrap()).unwrap();
    sources::sync_file(&mut usage.conn, "claude", &path, &mut sync).unwrap();
    assert_eq!(usage.dashboard(&filter()).unwrap().total, 1);
}

#[test]
fn reads_both_opencode_layouts_and_preserves_reported_cost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("opencode.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE message(id TEXT,session_id TEXT,data TEXT,time_updated INTEGER); CREATE TABLE session_message(id TEXT,session_id TEXT,data TEXT,type TEXT,time_updated INTEGER);").unwrap();
    let message = json!({"role":"assistant","modelID":"custom","time":{"created":1783652400000i64,"completed":1783652401000i64},"tokens":{"input":12,"output":5,"cache":{"read":9,"write":2}},"cost":0.04}).to_string();
    db.execute("INSERT INTO message VALUES ('m','s',?1,1)", [&message])
        .unwrap();
    db.execute(
        "INSERT INTO session_message VALUES ('m','s',?1,'assistant',2)",
        [&message],
    )
    .unwrap();
    db.execute(
        "INSERT INTO session_message VALUES ('m2','s',?1,'assistant',2)",
        [&message],
    )
    .unwrap();
    let mut usage = Usage::open(&dir.path().join("usage.db")).unwrap();
    let mut sync = crate::SyncResult::default();
    sources::sync_opencode(&mut usage.conn, &path, &mut sync).unwrap();
    sources::sync_opencode(&mut usage.conn, &path, &mut sync).unwrap();
    let all = usage.dashboard(&filter()).unwrap();
    assert_eq!(sync.imported, 2);
    assert_eq!(
        (all.total, all.summary.input, all.summary.cache_read),
        (2, 24, 18)
    );
    assert!((all.summary.cost - 0.08).abs() < 1e-12);
}

fn antigravity_transcript(root: &std::path::Path, session: &str) -> std::path::PathBuf {
    let path = root
        .join("brain")
        .join(session)
        .join(".system_generated/logs/transcript_full.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

fn sync_antigravity(usage: &mut Usage, home: &std::path::Path) -> u64 {
    let mut sync = crate::SyncResult::default();
    sources::sync_antigravity(&mut usage.conn, home, &mut sync);
    assert!(sync.errors.is_empty(), "{:?}", sync.errors);
    sync.imported
}

#[test]
fn antigravity_counts_the_upstream_mock_usage_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = antigravity_transcript(&dir.path().join(".gemini/antigravity-cli"), "mock");
    // Counters from cc-switch v4.0.38's recorded agy mock transcript.
    fs::write(&path, concat!(
        "{\"step_index\":0,\"source\":\"USER_EXPLICIT\",\"type\":\"USER_INPUT\",\"status\":\"DONE\",\"created_at\":\"2026-10-13T19:52:43Z\",\"content\":\"Say fixture reply only\"}\n",
        "{\"step_index\":1,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-10-13T19:52:43Z\",\"input_tokens\":80,\"cache_read_tokens\":20,\"output_tokens\":12,\"content\":\"fixture reply\"}"
    )).unwrap();
    let mut usage = Usage::open(&dir.path().join("usage.db")).unwrap();
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 1);
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 0);
    let dashboard = usage
        .dashboard(&Filter {
            agent: Some("antigravity".into()),
            ..filter()
        })
        .unwrap();
    assert_eq!(dashboard.total, 1);
    assert_eq!(dashboard.summary.unpriced_requests, 1);
    assert_eq!(dashboard.summary.cost, 0.0);
    assert_eq!(dashboard.agents[0].agent, "antigravity");
    let log = &dashboard.logs[0];
    assert_eq!(log.cost, None);
    assert_eq!(log.id, "antigravity:mock:1");
    assert_eq!(log.session_id.as_deref(), Some("mock"));
    assert_eq!(log.model, "unknown");
    assert_eq!(log.status.as_deref(), Some("DONE"));
    assert_eq!(log.timestamp, 1_791_921_163_000);
    assert_eq!(
        (log.input, log.output, log.cache_read, log.cache_write),
        (80, 12, 20, 0)
    );
}

#[test]
fn antigravity_uses_latest_steps_and_updates_lower_corrections_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let cli = dir.path().join(".gemini/antigravity-cli");
    let ide = dir.path().join(".gemini/antigravity");
    let transcript = antigravity_transcript(&cli, "session");
    let step = |status, output| {
        json!({"step_index":3,"type":"PLANNER_RESPONSE","status":status,
        "created_at":1_791_921_163,"input_tokens":80,"output_tokens":output,"thinking_tokens":20,
        "cache_read_tokens":20,"cache_write_tokens":15})
    };
    let supplemental =
        json!({"step_index":2,"type":"PLANNER_RESPONSE","status":"DONE","input_tokens":4});
    write_lines(
        &antigravity_transcript(&ide, "session"),
        &[supplemental.clone(), step("DONE", 90)],
    );
    write_lines(
        &antigravity_transcript(&ide, "ide-subagent"),
        std::slice::from_ref(&supplemental),
    );
    write_lines(
        &antigravity_transcript(
            &dir.path().join(".cc-switch/agy/key/antigravity-cli"),
            "private",
        ),
        &[supplemental],
    );
    let selection = json!({"step_index":1,"type":"USER_INPUT","content":"The user changed setting `Model Selection` from None to Gemini 3.8 Flash (High). No need to comment"});
    let running =
        json!({"step_index":4,"type":"PLANNER_RESPONSE","status":"RUNNING","input_tokens":10});
    let zero = json!({"step_index":5,"type":"PLANNER_RESPONSE","status":"DONE","model":"ignored"});
    let tool = json!({"step_index":6,"type":"TOOL_RESPONSE","status":"DONE","input_tokens":10});
    write_lines(
        &transcript,
        &[
            step("DONE", 42),
            step("DONE", 12),
            selection.clone(),
            running,
            zero,
            tool,
        ],
    );
    let mut usage = Usage::open(&dir.path().join("usage.db")).unwrap();
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 2);
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 0);
    let before = usage.dashboard(&filter()).unwrap();
    assert_eq!((before.total, before.summary.output), (2, 12));
    let log = &before.logs[0];
    assert_eq!(log.model, "gemini-3.8-flash-high");
    assert_eq!(log.timestamp, 1_791_921_163_000);
    assert_eq!((log.input, log.cache_read, log.cache_write), (80, 20, 15));

    write_lines(&transcript, &[selection, step("DONE", 4)]);
    let line = json!({"step_index":4,"type":"PLANNER_RESPONSE","status":"DONE","input_tokens":10})
        .to_string();
    let (start, end) = line.split_at(line.len() / 2);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    write!(file, "{start}").unwrap();
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 1);
    let corrected = usage.dashboard(&filter()).unwrap();
    assert_eq!((corrected.total, corrected.summary.output), (2, 4));
    assert_eq!(corrected.logs[0].id, log.id);
    write!(file, "{end}").unwrap();
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 1);
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 0);
    assert_eq!(usage.dashboard(&filter()).unwrap().total, 3);

    // The same conversation can move to the IDE. Its corrected record keeps its ID.
    fs::rename(&transcript, antigravity_transcript(&ide, "session")).unwrap();
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 0);
    assert_eq!(usage.dashboard(&filter()).unwrap().total, 3);
}

fn wire_bytes(field: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = wire_varint((field << 3) | 2);
    bytes.extend(wire_varint(payload.len() as u64));
    bytes.extend(payload);
    bytes
}

fn wire_varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    while value >= 128 {
        bytes.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    bytes.push(value as u8);
    bytes
}

fn native_generation(base: &str, tier: &str) -> Vec<u8> {
    let mut response = wire_bytes(19, base.as_bytes());
    response.extend(wire_bytes(21, tier.as_bytes()));
    // Field 11 is a 1.2s first token wait, field 12 is 0.8s of streaming.
    let mut wait = vec![8, 1, 16];
    wait.extend(wire_varint(200_000_000));
    let mut stream = vec![16];
    stream.extend(wire_varint(800_000_000));
    response.extend(wire_bytes(11, &wait));
    response.extend(wire_bytes(12, &stream));
    wire_bytes(1, &response)
}

#[test]
fn antigravity_backfills_native_orchestrator_models_and_timing_from_wal() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join(".gemini/antigravity-cli");
    let transcript = antigravity_transcript(&root, "native");
    let mut rows = vec![
        json!({"step_index":0,"type":"USER_INPUT","content":"changed setting `Model Selection` from None to Orchestrator (Neon). No need to comment"}),
        json!({"step_index":1,"type":"PLANNER_RESPONSE","status":"DONE","input_tokens":80,"output_tokens":12}),
        json!({"step_index":3,"type":"PLANNER_RESPONSE","status":"DONE","model":"gemini-flash-worker-agy","input_tokens":3}),
        json!({"step_index":4,"type":"PLANNER_RESPONSE","status":"DONE","model":"Gemini 4 Argon (Medium)","input_tokens":4}),
        json!({"step_index":5,"type":"PLANNER_RESPONSE","status":"DONE","model":"gemini-flash-worker-agy","input_tokens":5}),
    ];
    write_lines(&transcript, &rows);
    let mut usage = Usage::open(&dir.path().join("usage.db")).unwrap();
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 4);
    let detail = |usage: &Usage, step| {
        usage
            .dashboard(&filter())
            .unwrap()
            .logs
            .into_iter()
            .find(|log| log.id == format!("antigravity:native:{step}"))
            .unwrap()
    };
    assert_eq!(detail(&usage, 1).model, "orchestrator-neon");
    assert_eq!(detail(&usage, 3).model, "gemini-4-hydrogen");
    assert_eq!(detail(&usage, 1).duration_ms, None);

    let path = root.join("conversations/native.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE steps(idx INTEGER PRIMARY KEY, step_type INTEGER, metadata BLOB);
        CREATE TABLE gen_metadata(idx INTEGER PRIMARY KEY, data BLOB);",
    )
    .unwrap();
    for (step, index, base, tier) in [
        (1, 0, "gemini-pro-orchestrator-neon", "Orchestrator (Neon)"),
        (
            3,
            7,
            "gemini-flash-worker-agy",
            "Gemini 4 Hydrogen (Orchestrator)",
        ),
        (4, 2, "gemini-4-carbon", "gemini-4-carbon-high"),
        (5, 9, "", "Gemini Flash"),
    ] {
        let reference = if index == 0 { vec![] } else { vec![24, index] };
        db.execute(
            "INSERT INTO steps VALUES (?1,15,?2)",
            rusqlite::params![step, wire_bytes(20, &reference)],
        )
        .unwrap();
        db.execute(
            "INSERT INTO gen_metadata VALUES (?1,?2)",
            rusqlite::params![index, native_generation(base, tier)],
        )
        .unwrap();
    }
    assert!(path.with_extension("db-wal").is_file());
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 4);
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 0);
    for (step, model) in [
        (1, "gemini-4-neon"),
        (3, "gemini-4-hydrogen-orchestrator"),
        (4, "gemini-4-argon-medium"),
        (5, "gemini-4-hydrogen"),
    ] {
        let log = detail(&usage, step);
        assert_eq!(log.model, model);
        assert_eq!(log.duration_ms, Some(2_000));
    }
    db.execute(
        "UPDATE gen_metadata SET data=?1 WHERE idx=7",
        [native_generation(
            "gemini-flash-worker-agy",
            "gemini-4-hydrogen-tiered-high",
        )],
    )
    .unwrap();
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 1);
    assert_eq!(detail(&usage, 3).model, "gemini-4-hydrogen-tiered-high");

    db.execute_batch("DROP TABLE gen_metadata").unwrap();
    rows[1]["model"] = json!("gemini-pro-orchestrator-neon");
    rows[1]["output_tokens"] = json!(2);
    rows[2]["input_tokens"] = json!(1);
    rows[3]["model"] = json!("gemini-flash-worker-agy");
    write_lines(&transcript, &rows);
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 3);
    assert_eq!(detail(&usage, 1).model, "gemini-4-neon");
    assert_eq!(detail(&usage, 1).output, 2);
    assert_eq!(detail(&usage, 1).duration_ms, Some(2_000));
    assert_eq!(detail(&usage, 3).model, "gemini-4-hydrogen-tiered-high");
    assert_eq!(detail(&usage, 4).model, "gemini-4-hydrogen");
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 0);
}

#[test]
#[cfg(unix)]
fn antigravity_ignores_transcript_and_database_links_outside_default_roots() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let cli = dir.path().join(".gemini/antigravity-cli");
    let transcript = antigravity_transcript(&cli, "valid");
    let outside = antigravity_transcript(external.path(), "outside");
    let response =
        json!({"step_index":1,"type":"PLANNER_RESPONSE","status":"DONE","input_tokens":4});
    write_lines(&transcript, std::slice::from_ref(&response));
    write_lines(&outside, &[response]);
    symlink(&outside, antigravity_transcript(&cli, "file-link")).unwrap();
    symlink(external.path(), dir.path().join(".gemini/antigravity")).unwrap();
    let external_db = external.path().join("valid.db");
    let db = Connection::open(&external_db).unwrap();
    db.execute_batch(
        "CREATE TABLE steps(idx, step_type, metadata); CREATE TABLE gen_metadata(idx, data);",
    )
    .unwrap();
    db.execute("INSERT INTO steps VALUES (1,15,?1)", [wire_bytes(20, &[])])
        .unwrap();
    db.execute(
        "INSERT INTO gen_metadata VALUES (0,?1)",
        [native_generation("gemini-4-carbon", "")],
    )
    .unwrap();
    fs::create_dir(cli.join("conversations")).unwrap();
    symlink(&external_db, cli.join("conversations/valid.db")).unwrap();
    let mut usage = Usage::open(&dir.path().join("usage.db")).unwrap();
    assert_eq!(sync_antigravity(&mut usage, dir.path()), 1);
    let logs = usage.dashboard(&filter()).unwrap().logs;
    assert_eq!(logs[0].id, "antigravity:valid:1");
    assert_eq!(logs[0].model, "unknown");
    assert_eq!(logs[0].duration_ms, None);
}

#[test]
fn prices_filters_pagination_and_host_day_are_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let mut usage = Usage::open(&dir.path().join("usage.db")).unwrap();
    let path = dir.path().join("claude.jsonl");
    write_lines(
        &path,
        &[
            json!({"type":"assistant","sessionId":"s","timestamp":"2026-07-10T01:30:00Z","message":{"id":"m","model":"custom","usage":{"input_tokens":1000000,"output_tokens":10}}}),
        ],
    );
    sources::sync_file(
        &mut usage.conn,
        "claude",
        &path,
        &mut crate::SyncResult::default(),
    )
    .unwrap();
    let unknown = usage.dashboard(&filter()).unwrap();
    assert_eq!(unknown.summary.cost, 0.0);
    assert_eq!(unknown.summary.unpriced_requests, 1);
    assert_eq!(unknown.logs[0].cost, None);
    usage
        .save_price(&Price {
            model: "custom".into(),
            input: 2.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
        })
        .unwrap();
    let result = usage
        .dashboard(&Filter {
            host_zone: Some("America/Los_Angeles".into()),
            clock_offset: 1000,
            ..filter()
        })
        .unwrap();
    assert_eq!(result.summary.cost, 2.0);
    assert_eq!(result.summary.unpriced_requests, 0);
    let start = chrono::DateTime::parse_from_rfc3339("2026-07-09T07:00:01Z")
        .unwrap()
        .timestamp_millis();
    assert_eq!(result.daily[0].timestamp, start);
    assert_eq!(
        usage
            .dashboard(&Filter {
                offset: 1,
                ..filter()
            })
            .unwrap()
            .logs
            .len(),
        0
    );
    assert_eq!(
        usage
            .dashboard(&Filter {
                from_ms: Some(1783652400001),
                ..filter()
            })
            .unwrap()
            .total,
        0
    );
    assert!(
        usage
            .save_price(&Price {
                model: "bad".into(),
                input: -1.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0
            })
            .is_err()
    );
}
