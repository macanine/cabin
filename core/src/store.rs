//! SQLite metadata store: workspaces, sessions, operation journal, proxy
//! sources and settings. Agent configs stay in their native files.

use crate::util::LockExt;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Error, Result};
use crate::model::{
    AgentKind, ChatItem, ChatPermissionMode, ChatTurnUsage, ModelOverride, NetworkCheckReport,
    NetworkHistoryEntry, OperationRecord, OperationState, PlanDecision, PlanExecutionStatus,
    PlanReview, PlanReviewStatus, Session, SessionMode, SessionStatus, SubagentTask, Workspace,
};
use crate::util::now_millis;

pub const NETWORK_HISTORY_LIMIT: u32 = 500;

/// Safety valve for a pathological session: snapshots and agent context load
/// the newest items, never an unbounded history. Real chats stay far below it.
const CHAT_ITEM_LIMIT: i64 = 20_000;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    models TEXT NOT NULL DEFAULT '[]',
    archived INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    agent TEXT NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_started_at INTEGER,
    starred INTEGER NOT NULL DEFAULT 0,
    mode TEXT NOT NULL DEFAULT 'terminal',
    worktree TEXT
);
CREATE TABLE IF NOT EXISTS operations (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    state TEXT NOT NULL,
    record TEXT NOT NULL,
    idempotency_key TEXT,
    request TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS operations_created ON operations(created_at DESC);
CREATE TABLE IF NOT EXISTS proxy_sources (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    url TEXT,
    nodes TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS network_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    failed_checks INTEGER NOT NULL,
    report TEXT
);
CREATE TABLE IF NOT EXISTS chat_threads (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    native_ref TEXT,
    model TEXT,
    baseline_model TEXT,
    permission TEXT NOT NULL DEFAULT 'approval-required'
);
CREATE TABLE IF NOT EXISTS chat_items (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (session_id, id)
);
CREATE TABLE IF NOT EXISTS chat_usage (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    turn_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (session_id, turn_id)
);
CREATE TABLE IF NOT EXISTS chat_plans (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    plan_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    execution TEXT NOT NULL,
    kind TEXT NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (session_id, plan_id, revision)
);
CREATE INDEX IF NOT EXISTS chat_plans_session ON chat_plans(session_id, plan_id, revision);
CREATE TABLE IF NOT EXISTS chat_subagents (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (session_id, id)
);
CREATE INDEX IF NOT EXISTS chat_subagents_session ON chat_subagents(session_id, ordinal);
"#;

/// Stored subscription with its raw nodes (see `proxy::subscription`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSource {
    pub id: String,
    pub name: String,
    pub url: Option<String>,
    pub nodes_json: String,
    pub updated_at: i64,
}

#[derive(Debug)]
pub enum PlanClaim {
    Claimed {
        review: PlanReview,
        previous: PlanReviewStatus,
    },
    Conflict(PlanReview),
}

pub struct Store {
    conn: Mutex<Connection>,
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn from_json<T: DeserializeOwned>(text: &str) -> rusqlite::Result<T> {
    serde_json::from_str(text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn workspace_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workspace> {
    let models: String = row.get("models")?;
    Ok(Workspace {
        id: row.get("id")?,
        name: row.get("name")?,
        path: row.get("path")?,
        model_overrides: from_json::<Vec<ModelOverride>>(&models)?,
        archived: row.get::<_, i64>("archived")? != 0,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

/// `sessions.agent` value of a plain terminal session (`Session::agent` is
/// `None`); keeps the column NOT NULL without a schema change.
const TERMINAL: &str = "terminal";

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let agent: String = row.get("agent")?;
    Ok(Session {
        id: row.get("id")?,
        workspace_id: row.get("workspace_id")?,
        name: row.get("name")?,
        agent: if agent == TERMINAL {
            None
        } else {
            Some(AgentKind::parse(&agent).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    error.to_string().into(),
                )
            })?)
        },
        mode: match row.get::<_, String>("mode")?.as_str() {
            "ui" => SessionMode::Ui,
            _ => SessionMode::Terminal,
        },
        archived: row.get::<_, i64>("archived")? != 0,
        starred: row.get::<_, i64>("starred")? != 0,
        worktree: row
            .get::<_, Option<String>>("worktree")?
            .map(|value| from_json(&value))
            .transpose()?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        last_started_at: row.get("last_started_at")?,
        status: SessionStatus::Stopped,
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            crate::fsx::ensure_dir(parent)?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.locked()
    }

    // ------------------------------------------------------------ workspaces

    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM workspaces ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], workspace_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn workspace(&self, id: &str) -> Result<Workspace> {
        self.conn()
            .query_row(
                "SELECT * FROM workspaces WHERE id = ?1",
                [id],
                workspace_row,
            )
            .optional()?
            .ok_or_else(|| Error::NotFound("workspace".into()))
    }

    pub fn insert_workspace(&self, workspace: &Workspace) -> Result<()> {
        self.conn().execute(
            "INSERT INTO workspaces (id, name, path, models, archived, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                workspace.id,
                workspace.name,
                workspace.path,
                json(&workspace.model_overrides)?,
                workspace.archived as i64,
                workspace.created_at,
                workspace.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn update_workspace(&self, workspace: &Workspace) -> Result<()> {
        let changed = self.conn().execute(
            "UPDATE workspaces SET name = ?2, path = ?3, models = ?4, archived = ?5,
             updated_at = ?6 WHERE id = ?1",
            params![
                workspace.id,
                workspace.name,
                workspace.path,
                json(&workspace.model_overrides)?,
                workspace.archived as i64,
                now_millis()
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound("workspace".into()));
        }
        Ok(())
    }

    pub fn delete_workspace(&self, id: &str) -> Result<()> {
        let changed = self
            .conn()
            .execute("DELETE FROM workspaces WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(Error::NotFound("workspace".into()));
        }
        Ok(())
    }

    // -------------------------------------------------------------- sessions

    pub fn sessions(&self) -> Result<Vec<Session>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM sessions ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], session_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn session(&self, id: &str) -> Result<Session> {
        self.conn()
            .query_row("SELECT * FROM sessions WHERE id = ?1", [id], session_row)
            .optional()?
            .ok_or_else(|| Error::NotFound("session".into()))
    }

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        self.conn().execute(
            "INSERT INTO sessions (id, workspace_id, name, agent, archived, created_at, updated_at, last_started_at, starred, mode, worktree)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                session.id,
                session.workspace_id,
                session.name,
                session.agent.map_or(TERMINAL, AgentKind::as_str),
                session.archived as i64,
                session.created_at,
                session.updated_at,
                session.last_started_at,
                session.starred as i64,
                if session.mode == SessionMode::Ui { "ui" } else { "terminal" },
                session.worktree.as_ref().map(json).transpose()?,
            ],
        )?;
        Ok(())
    }

    pub fn update_session(&self, session: &Session) -> Result<()> {
        let changed = self.conn().execute(
            "UPDATE sessions SET name = ?2, agent = ?3, archived = ?4, updated_at = ?5,
             last_started_at = ?6, starred = ?7 WHERE id = ?1",
            params![
                session.id,
                session.name,
                session.agent.map_or(TERMINAL, AgentKind::as_str),
                session.archived as i64,
                now_millis(),
                session.last_started_at,
                session.starred as i64
            ],
        )?;
        if changed == 0 {
            return Err(Error::NotFound("session".into()));
        }
        Ok(())
    }

    pub fn set_session_worktree(
        &self,
        id: &str,
        worktree: Option<&crate::model::SessionWorktree>,
    ) -> Result<()> {
        let changed = self.conn().execute(
            "UPDATE sessions SET worktree = ?2 WHERE id = ?1",
            params![id, worktree.map(json).transpose()?],
        )?;
        if changed == 0 {
            return Err(Error::NotFound("session".into()));
        }
        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        let changed = self
            .conn()
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(Error::NotFound("session".into()));
        }
        Ok(())
    }

    pub fn chat_thread(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT native_ref FROM chat_threads WHERE session_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    pub fn set_chat_thread(&self, id: &str, native_ref: &str) -> Result<()> {
        self.conn().execute("INSERT INTO chat_threads (session_id, native_ref) VALUES (?1, ?2) ON CONFLICT(session_id) DO UPDATE SET native_ref = excluded.native_ref", params![id, native_ref])?;
        Ok(())
    }

    pub fn chat_baseline_model(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT baseline_model FROM chat_threads WHERE session_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }
    pub fn set_chat_baseline_model(&self, id: &str, model: &str) -> Result<()> {
        self.conn().execute("INSERT INTO chat_threads (session_id, baseline_model) VALUES (?1, ?2) ON CONFLICT(session_id) DO UPDATE SET baseline_model = COALESCE(chat_threads.baseline_model, excluded.baseline_model)", params![id, model])?;
        Ok(())
    }

    pub fn chat_model(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT model FROM chat_threads WHERE session_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }
    pub fn set_chat_model(&self, id: &str, model: Option<&str>) -> Result<()> {
        self.conn().execute("INSERT INTO chat_threads (session_id, model) VALUES (?1, ?2) ON CONFLICT(session_id) DO UPDATE SET model = excluded.model", params![id, model])?;
        Ok(())
    }

    pub fn chat_permission(&self, id: &str) -> Result<ChatPermissionMode> {
        let value: Option<String> = self
            .conn()
            .query_row(
                "SELECT permission FROM chat_threads WHERE session_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match value.as_deref() {
            Some("auto-accept-edits") => ChatPermissionMode::AutoAcceptEdits,
            Some("auto") => ChatPermissionMode::Auto,
            Some("plan") => ChatPermissionMode::Plan,
            Some("full-access") => ChatPermissionMode::FullAccess,
            _ => ChatPermissionMode::ApprovalRequired,
        })
    }

    pub fn set_chat_permission(&self, id: &str, permission: ChatPermissionMode) -> Result<()> {
        self.conn().execute(
            "INSERT INTO chat_threads (session_id, permission) VALUES (?1, ?2)
             ON CONFLICT(session_id) DO UPDATE SET permission = excluded.permission",
            params![id, permission.as_str()],
        )?;
        Ok(())
    }

    pub fn chat_items(&self, id: &str) -> Result<Vec<ChatItem>> {
        self.chat_item_tail(id, CHAT_ITEM_LIMIT)
    }

    /// The newest `limit` items in reading order. The inner query keeps the
    /// tail, the outer one restores the order the UI folds turns from.
    fn chat_item_tail(&self, id: &str, limit: i64) -> Result<Vec<ChatItem>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT data FROM (
                 SELECT data, ordinal FROM chat_items
                 WHERE session_id = ?1 ORDER BY ordinal DESC LIMIT ?2
             ) ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(params![id, limit], |row| {
            from_json::<ChatItem>(&row.get::<_, String>(0)?)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn chat_plans(&self, session_id: &str) -> Result<Vec<PlanReview>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT data FROM chat_plans WHERE session_id = ?1
             ORDER BY plan_id, revision",
        )?;
        let rows = stmt.query_map([session_id], |row| {
            from_json::<PlanReview>(&row.get::<_, String>(0)?)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn chat_plan(
        &self,
        session_id: &str,
        plan_id: &str,
        revision: i64,
    ) -> Result<Option<PlanReview>> {
        let text: Option<String> = self
            .conn()
            .query_row(
                "SELECT data FROM chat_plans
                 WHERE session_id = ?1 AND plan_id = ?2 AND revision = ?3",
                params![session_id, plan_id, revision],
                |row| row.get(0),
            )
            .optional()?;
        Ok(text.map(|text| serde_json::from_str(&text)).transpose()?)
    }

    /// Draft rows are previews only. One `plan_id` has at most one draft row
    /// (revision 0); a fresh plan cycle replaces it rather than growing an
    /// unbounded pile of incomplete previews.
    pub fn put_plan_draft(&self, session_id: &str, mut plan: PlanReview) -> Result<PlanReview> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let newer_open: Option<i64> = tx
            .query_row(
                "SELECT revision FROM chat_plans
                 WHERE session_id = ?1 AND plan_id = ?2 AND revision > 0
                   AND status IN ('awaiting_review', 'submitting', 'deferred')
                 ORDER BY revision DESC LIMIT 1",
                params![session_id, plan.plan_id],
                |row| row.get(0),
            )
            .optional()?;
        if newer_open.is_some() {
            return Err(Error::Conflict(
                "A complete revision of this plan is already under review".into(),
            ));
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT data FROM chat_plans
                 WHERE session_id = ?1 AND plan_id = ?2 AND revision = 0",
                params![session_id, plan.plan_id],
                |row| row.get(0),
            )
            .optional()?;
        let now = now_millis();
        let created_at = existing
            .as_deref()
            .and_then(|text| serde_json::from_str::<PlanReview>(text).ok())
            .map_or(now, |old| old.created_at);
        plan.session_id = session_id.into();
        plan.revision = 0;
        plan.status = PlanReviewStatus::Draft;
        plan.execution = PlanExecutionStatus::NotStarted;
        plan.complete = false;
        plan.decision = None;
        plan.feedback = None;
        plan.decision_intent_at = None;
        plan.applied_at = None;
        plan.superseded_by = None;
        plan.created_at = created_at;
        plan.updated_at = now;
        tx.execute(
            "INSERT INTO chat_plans
                (session_id, plan_id, revision, status, execution, kind, data)
             VALUES (?1, ?2, 0, ?3, ?4, 'draft', ?5)
             ON CONFLICT(session_id, plan_id, revision) DO UPDATE SET
                status = excluded.status, execution = excluded.execution,
                kind = excluded.kind, data = excluded.data",
            params![
                session_id,
                plan.plan_id,
                plan.status.as_str(),
                plan.execution.as_str(),
                json(&plan)?,
            ],
        )?;
        tx.commit()?;
        Ok(plan)
    }

    /// Add the next complete revision of a plan and supersede every older
    /// open revision in the same transaction.
    pub fn record_plan_review(&self, session_id: &str, mut plan: PlanReview) -> Result<PlanReview> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let next: i64 = tx.query_row(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM chat_plans
             WHERE session_id = ?1 AND plan_id = ?2",
            params![session_id, plan.plan_id],
            |row| row.get(0),
        )?;
        let now = now_millis();
        let mut stmt = tx.prepare(
            "SELECT revision, data FROM chat_plans
             WHERE session_id = ?1 AND plan_id = ?2",
        )?;
        let rows = stmt.query_map(params![session_id, plan.plan_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut supersede = Vec::new();
        for row in rows {
            let (revision, data) = row?;
            if let Ok(mut old) = serde_json::from_str::<PlanReview>(&data)
                && old.status.is_open()
            {
                old.status = PlanReviewStatus::Superseded;
                old.superseded_by = Some(next);
                old.updated_at = now;
                supersede.push((revision, old));
            }
        }
        drop(stmt);
        for (revision, old) in supersede {
            tx.execute(
                "UPDATE chat_plans SET status = ?1, data = ?2
                 WHERE session_id = ?3 AND plan_id = ?4 AND revision = ?5",
                params![
                    old.status.as_str(),
                    json(&old)?,
                    session_id,
                    plan.plan_id,
                    revision,
                ],
            )?;
        }
        plan.session_id = session_id.into();
        plan.revision = next;
        plan.created_at = now;
        plan.updated_at = now;
        plan.superseded_by = None;
        tx.execute(
            "INSERT INTO chat_plans
                (session_id, plan_id, revision, status, execution, kind, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session_id,
                plan.plan_id,
                plan.revision,
                plan.status.as_str(),
                plan.execution.as_str(),
                plan.review_kind.as_str(),
                json(&plan)?,
            ],
        )?;
        tx.commit()?;
        Ok(plan)
    }

    /// Atomically claim one revision for a decision. A second window/double
    /// click sees the current row as a conflict instead of applying twice.
    pub fn claim_plan_decision(
        &self,
        session_id: &str,
        plan_id: &str,
        expected_revision: i64,
        decision: PlanDecision,
        feedback: Option<&str>,
    ) -> Result<PlanClaim> {
        if decision == PlanDecision::RequestChanges
            && feedback.is_none_or(|feedback| feedback.trim().is_empty())
        {
            return Err(Error::invalid("Requesting changes requires feedback"));
        }
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let text: Option<String> = tx
            .query_row(
                "SELECT data FROM chat_plans
                 WHERE session_id = ?1 AND plan_id = ?2 AND revision = ?3",
                params![session_id, plan_id, expected_revision],
                |row| row.get(0),
            )
            .optional()?;
        let Some(text) = text else {
            return Err(Error::NotFound("plan review".into()));
        };
        let mut review: PlanReview = serde_json::from_str(&text)?;
        if !review.complete {
            return Err(Error::Conflict(
                "This plan is still a draft and cannot be approved".into(),
            ));
        }
        if !review.status.accepts_decision() {
            return Ok(PlanClaim::Conflict(review));
        }
        let previous = review.status;
        review.status = PlanReviewStatus::Submitting;
        review.decision = Some(decision);
        review.feedback = feedback
            .map(str::trim)
            .filter(|feedback| !feedback.is_empty())
            .map(str::to_string);
        review.decision_intent_at = Some(now_millis());
        review.updated_at = now_millis();
        tx.execute(
            "UPDATE chat_plans SET status = ?1, data = ?2
             WHERE session_id = ?3 AND plan_id = ?4 AND revision = ?5",
            params![
                review.status.as_str(),
                json(&review)?,
                session_id,
                plan_id,
                expected_revision,
            ],
        )?;
        tx.commit()?;
        Ok(PlanClaim::Claimed { review, previous })
    }

    pub fn finish_plan_decision(
        &self,
        session_id: &str,
        plan_id: &str,
        revision: i64,
        decision: PlanDecision,
        execution: PlanExecutionStatus,
    ) -> Result<Option<PlanReview>> {
        self.update_plan(session_id, plan_id, revision, |review| {
            if review.status != PlanReviewStatus::Submitting {
                return;
            }
            review.status = decision.settled_status();
            if !matches!(
                review.execution,
                PlanExecutionStatus::Completed | PlanExecutionStatus::Failed
            ) {
                review.execution = execution;
            }
            review.applied_at = Some(now_millis());
            review.updated_at = now_millis();
        })
    }

    pub fn revert_plan_submission(
        &self,
        session_id: &str,
        plan_id: &str,
        revision: i64,
        previous: PlanReviewStatus,
    ) -> Result<Option<PlanReview>> {
        self.update_plan(session_id, plan_id, revision, |review| {
            if review.status != PlanReviewStatus::Submitting {
                return;
            }
            review.status = previous;
            review.updated_at = now_millis();
        })
    }

    pub fn set_plan_execution(
        &self,
        session_id: &str,
        plan_id: &str,
        revision: i64,
        execution: PlanExecutionStatus,
    ) -> Result<Option<PlanReview>> {
        self.update_plan(session_id, plan_id, revision, |review| {
            review.execution = execution;
            review.updated_at = now_millis();
        })
    }

    pub fn expire_open_plans(&self, session_id: &str) -> Result<Vec<PlanReview>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT revision, data FROM chat_plans
             WHERE session_id = ?1 ORDER BY plan_id, revision",
        )?;
        let rows = stmt.query_map([session_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut expired = Vec::new();
        let mut updates = Vec::new();
        for row in rows {
            let (revision, text) = row?;
            let Ok(mut review) = serde_json::from_str::<PlanReview>(&text) else {
                continue;
            };
            if !matches!(
                review.status,
                PlanReviewStatus::AwaitingReview
                    | PlanReviewStatus::Submitting
                    | PlanReviewStatus::Deferred
            ) {
                continue;
            }
            review.status = PlanReviewStatus::Expired;
            if review.execution == PlanExecutionStatus::Starting
                || review.execution == PlanExecutionStatus::Running
            {
                review.execution = PlanExecutionStatus::Unknown;
            }
            review.updated_at = now_millis();
            updates.push((
                review.plan_id.clone(),
                revision,
                review.status.as_str().to_string(),
                json(&review)?,
            ));
            expired.push(review);
        }
        drop(stmt);
        for (plan_id, revision, status, data) in updates {
            tx.execute(
                "UPDATE chat_plans SET status = ?1, data = ?2
                 WHERE session_id = ?3 AND plan_id = ?4 AND revision = ?5",
                params![status, data, session_id, plan_id, revision],
            )?;
        }
        tx.commit()?;
        Ok(expired)
    }

    fn update_plan(
        &self,
        session_id: &str,
        plan_id: &str,
        revision: i64,
        update: impl FnOnce(&mut PlanReview),
    ) -> Result<Option<PlanReview>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let text: Option<String> = tx
            .query_row(
                "SELECT data FROM chat_plans
                 WHERE session_id = ?1 AND plan_id = ?2 AND revision = ?3",
                params![session_id, plan_id, revision],
                |row| row.get(0),
            )
            .optional()?;
        let Some(text) = text else {
            return Ok(None);
        };
        let mut review: PlanReview = serde_json::from_str(&text)?;
        update(&mut review);
        tx.execute(
            "UPDATE chat_plans
             SET status = ?1, execution = ?2, data = ?3
             WHERE session_id = ?4 AND plan_id = ?5 AND revision = ?6",
            params![
                review.status.as_str(),
                review.execution.as_str(),
                json(&review)?,
                session_id,
                plan_id,
                revision,
            ],
        )?;
        tx.commit()?;
        Ok(Some(review))
    }

    pub fn chat_subagents(&self, session_id: &str) -> Result<Vec<SubagentTask>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT data FROM chat_subagents
             WHERE session_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map([session_id], |row| {
            from_json::<SubagentTask>(&row.get::<_, String>(0)?)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn put_chat_subagent(
        &self,
        session_id: &str,
        mut task: SubagentTask,
    ) -> Result<SubagentTask> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let old: Option<(i64, String)> = tx
            .query_row(
                "SELECT ordinal, data FROM chat_subagents
                 WHERE session_id = ?1 AND id = ?2",
                params![session_id, task.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let now = now_millis();
        let (ordinal, started_at) = match old {
            Some((ordinal, data)) => {
                let started = serde_json::from_str::<SubagentTask>(&data)
                    .ok()
                    .map(|old| old.started_at)
                    .filter(|started| *started > 0)
                    .unwrap_or(now);
                (ordinal, started)
            }
            None => (
                tx.query_row(
                    "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM chat_subagents
                     WHERE session_id = ?1",
                    [session_id],
                    |row| row.get(0),
                )?,
                now,
            ),
        };
        task.started_at = started_at;
        task.updated_at = now;
        tx.execute(
            "INSERT INTO chat_subagents (session_id, id, ordinal, data)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session_id, id) DO UPDATE SET data = excluded.data",
            params![session_id, task.id, ordinal, json(&task)?],
        )?;
        tx.commit()?;
        Ok(task)
    }

    pub fn settle_active_subagents(&self, session_id: &str) -> Result<Vec<SubagentTask>> {
        let mut out = Vec::new();
        for mut task in self.chat_subagents(session_id)? {
            if task.state == crate::model::SubagentState::Active {
                task.state = crate::model::SubagentState::Stopped;
                out.push(self.put_chat_subagent(session_id, task)?);
            }
        }
        Ok(out)
    }

    /// Persist a complete item snapshot before broadcasting it. Revisions let
    /// clients merge a history read with concurrent live updates safely.
    /// Replace one turn's usage (bridges report absolute per-turn totals).
    pub fn put_chat_usage(&self, session_id: &str, turn: &ChatTurnUsage) -> Result<()> {
        self.conn().execute(
            "INSERT INTO chat_usage (session_id, turn_id, seq, data) VALUES (?1, ?2, (SELECT COALESCE(MAX(seq), 0) + 1 FROM chat_usage WHERE session_id = ?1), ?3)
             ON CONFLICT(session_id, turn_id) DO UPDATE SET data = excluded.data",
            params![session_id, turn.turn_id, json(&turn.usage)?],
        )?;
        Ok(())
    }

    pub fn chat_usage(&self, session_id: &str) -> Result<Vec<ChatTurnUsage>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT turn_id, data FROM chat_usage WHERE session_id = ?1 ORDER BY seq")?;
        let rows = stmt.query_map([session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (turn_id, data) = row?;
            // A row we cannot read is skipped rather than failing the chat.
            if let Ok(usage) = serde_json::from_str(&data) {
                out.push(ChatTurnUsage { turn_id, usage });
            }
        }
        Ok(out)
    }

    pub fn put_chat_item(&self, session_id: &str, mut item: ChatItem) -> Result<ChatItem> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let old: Option<(u32, u32, Option<i64>)> = tx.query_row("SELECT ordinal, revision, json_extract(data, '$.startedAt') FROM chat_items WHERE session_id = ?1 AND id = ?2", params![session_id, item.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        // The store owns timing: the first write starts an item, every write updates it.
        let now = crate::util::now_millis();
        item.updated_at = now;
        item.started_at = old
            .and_then(|(_, _, started)| started)
            .filter(|started| *started > 0)
            .unwrap_or(now);
        let (ordinal, revision) = match old {
            Some((ordinal, revision, _)) => (ordinal, revision + 1),
            None => (
                tx.query_row(
                    "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM chat_items WHERE session_id = ?1",
                    [session_id],
                    |r| r.get(0),
                )?,
                1,
            ),
        };
        item.ordinal = ordinal;
        item.revision = revision;
        tx.execute("INSERT INTO chat_items (session_id, id, ordinal, revision, data) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(session_id, id) DO UPDATE SET revision = excluded.revision, data = excluded.data", params![session_id, item.id, ordinal, revision, json(&item)?])?;
        tx.commit()?;
        Ok(item)
    }

    // ------------------------------------------------------------ operations

    pub fn save_operation(
        &self,
        record: &OperationRecord,
        key: Option<&str>,
        request: Option<&serde_json::Value>,
    ) -> Result<()> {
        let request = request.map(json).transpose()?;
        self.conn().execute(
            "INSERT INTO operations (id, kind, title, state, record, idempotency_key, request, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET state = excluded.state, record = excluded.record,
             updated_at = excluded.updated_at",
            params![
                record.id,
                record.kind,
                record.title,
                record.state.as_str(),
                json(record)?,
                key,
                request,
                record.created_at,
                record.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn operations(&self, limit: usize) -> Result<Vec<OperationRecord>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT record FROM operations ORDER BY created_at DESC LIMIT ?1")?;
        let rows = stmt.query_map([limit as i64], |row| {
            let text: String = row.get(0)?;
            from_json::<OperationRecord>(&text)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn operation_request(&self, id: &str) -> Result<(String, Option<serde_json::Value>)> {
        let (kind, request): (String, Option<String>) = self
            .conn()
            .query_row(
                "SELECT kind, request FROM operations WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| Error::NotFound("operation".into()))?;
        let request = request
            .map(|text| serde_json::from_str(&text))
            .transpose()?;
        Ok((kind, request))
    }

    /// Mark operations that were running when the app exited as failed.
    pub fn mark_interrupted(&self) -> Result<usize> {
        let unfinished: Vec<OperationRecord> = {
            let conn = self.conn();
            let mut stmt = conn
                .prepare("SELECT record FROM operations WHERE state IN ('pending', 'running')")?;
            let rows = stmt.query_map([], |row| {
                let text: String = row.get(0)?;
                from_json::<OperationRecord>(&text)
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for mut record in unfinished.iter().cloned() {
            record.state = OperationState::Failed;
            record.error = Some("Interrupted: the app exited while this operation was running. Check the steps below, then retry.".into());
            record.updated_at = now_millis();
            self.save_operation(&record, None, None)?;
        }
        Ok(unfinished.len())
    }

    // --------------------------------------------------------- proxy sources

    pub fn proxy_sources(&self) -> Result<Vec<StoredSource>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, url, nodes, updated_at FROM proxy_sources ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(StoredSource {
                id: row.get(0)?,
                name: row.get(1)?,
                url: row.get(2)?,
                nodes_json: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn upsert_proxy_source(&self, source: &StoredSource) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO proxy_sources (id, name, url, nodes, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, url = excluded.url,
                 nodes = excluded.nodes, updated_at = excluded.updated_at",
                params![
                    source.id,
                    source.name,
                    source.url,
                    source.nodes_json,
                    source.updated_at
                ],
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(code, _)
                    if code.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    Error::Conflict("A subscription with this name already exists".into())
                }
                other => other.into(),
            })?;
        Ok(())
    }

    pub fn delete_proxy_source(&self, id: &str) -> Result<()> {
        let changed = self
            .conn()
            .execute("DELETE FROM proxy_sources WHERE id = ?1", [id])?;
        if changed == 0 {
            return Err(Error::NotFound("subscription".into()));
        }
        Ok(())
    }

    // ------------------------------------------------------- network history

    pub fn append_network_history(
        &self,
        kind: &str,
        message: &str,
        failed_checks: u32,
        report: Option<&NetworkCheckReport>,
    ) -> Result<()> {
        let report = report.map(json).transpose()?;
        let mut conn = self.conn();
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT INTO network_history (at, kind, message, failed_checks, report)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![now_millis(), kind, message, failed_checks, report],
        )?;
        transaction.execute(
            "DELETE FROM network_history WHERE id <= (
                SELECT id FROM network_history ORDER BY id DESC LIMIT 1 OFFSET ?1
            ) - 1",
            [NETWORK_HISTORY_LIMIT - 1],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn network_history(
        &self,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Vec<NetworkHistoryEntry>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT id, at, kind, message, failed_checks, report FROM network_history
             WHERE id < ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(
            params![
                before.unwrap_or(i64::MAX),
                limit.clamp(1, NETWORK_HISTORY_LIMIT)
            ],
            |row| {
                let report: Option<String> = row.get(5)?;
                Ok(NetworkHistoryEntry {
                    id: row.get(0)?,
                    at: row.get(1)?,
                    kind: row.get(2)?,
                    message: row.get(3)?,
                    failed_checks: row.get(4)?,
                    report: report.as_deref().map(from_json).transpose()?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -------------------------------------------------------------- settings

    pub fn setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let text: Option<String> = self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(text.map(|text| serde_json::from_str(&text)).transpose()?)
    }

    pub fn set_setting<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, json(value)?],
        )?;
        Ok(())
    }

    pub fn remove_setting(&self, key: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: &str) -> Workspace {
        Workspace {
            id: id.into(),
            name: "api".into(),
            path: "/tmp".into(),
            model_overrides: vec![],
            archived: false,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn plan(status: PlanReviewStatus) -> PlanReview {
        PlanReview {
            plan_id: "plan-1".into(),
            agent: AgentKind::Codex,
            session_id: "s1".into(),
            revision: 0,
            title: "Plan".into(),
            content: "# Plan\n\n1. Do the work".into(),
            content_hash: "hash".into(),
            complete: status != PlanReviewStatus::Draft,
            review_kind: crate::model::PlanReviewKind::NextTurn,
            native_plan_id: Some("native-plan".into()),
            exec_permission: ChatPermissionMode::Auto,
            status,
            execution: PlanExecutionStatus::NotStarted,
            decision: None,
            feedback: None,
            decision_intent_at: None,
            applied_at: None,
            created_at: 0,
            updated_at: 0,
            superseded_by: None,
        }
    }

    fn subagent() -> SubagentTask {
        SubagentTask {
            id: "agent-1".into(),
            label: "Explore the store".into(),
            subagent_type: Some("Explore".into()),
            detail: "Find the persistence path".into(),
            state: crate::model::SubagentState::Active,
            origin: "claude".into(),
            started_at: 0,
            updated_at: 0,
            items: vec![],
        }
    }

    fn insert_chat(store: &Store) -> Result<()> {
        store.insert_workspace(&workspace("w1"))?;
        store.insert_session(&Session {
            id: "s1".into(),
            workspace_id: "w1".into(),
            worktree: None,
            name: "chat".into(),
            agent: Some(AgentKind::Codex),
            mode: SessionMode::Ui,
            archived: false,
            starred: false,
            created_at: 1,
            updated_at: 1,
            last_started_at: None,
            status: SessionStatus::Stopped,
        })
    }

    fn chat_item(id: &str) -> ChatItem {
        ChatItem {
            id: id.into(),
            ordinal: 0,
            revision: 0,
            turn_id: String::new(),
            kind: crate::model::ChatItemKind::AssistantMessage,
            status: crate::model::ChatItemStatus::Completed,
            title: String::new(),
            text: id.into(),
            detail: String::new(),
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

    #[test]
    fn chat_history_loads_the_newest_items_in_order() -> Result<()> {
        let store = Store::in_memory()?;
        insert_chat(&store)?;
        for index in 1..=5 {
            store.put_chat_item("s1", chat_item(&format!("i{index}")))?;
        }
        let tail: Vec<String> = store
            .chat_item_tail("s1", 3)?
            .into_iter()
            .map(|item| item.id)
            .collect();
        assert_eq!(tail, ["i3", "i4", "i5"]);
        assert_eq!(store.chat_items("s1")?.len(), 5);
        Ok(())
    }

    #[test]
    fn worktree_metadata_survives_reopen() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite3");
        let store = Store::open(&path)?;
        insert_chat(&store)?;
        let tree = crate::model::SessionWorktree {
            path: "/data/worktrees/s/project".into(),
            root: "/data/worktrees/s".into(),
            repository: "/repo".into(),
            base: "refs/heads/main".into(),
        };
        store.set_session_worktree("s1", Some(&tree))?;
        drop(store);
        let store = Store::open(&path)?;
        assert_eq!(store.session("s1")?.worktree, Some(tree));
        store.set_session_worktree("s1", None)?;
        assert_eq!(store.session("s1")?.worktree, None);
        Ok(())
    }

    #[test]
    fn deleting_a_workspace_deletes_its_sessions() -> Result<()> {
        let store = Store::in_memory()?;
        store.insert_workspace(&workspace("w1"))?;
        store.insert_session(&Session {
            id: "s1".into(),
            workspace_id: "w1".into(),
            worktree: None,
            name: "one".into(),
            agent: Some(AgentKind::Claude),
            mode: SessionMode::Terminal,
            archived: false,
            starred: false,
            created_at: 1,
            updated_at: 1,
            last_started_at: None,
            status: SessionStatus::Stopped,
        })?;
        assert_eq!(store.sessions()?.len(), 1);
        store.delete_workspace("w1")?;
        assert!(store.sessions()?.is_empty());
        Ok(())
    }

    #[test]
    fn chat_history_and_native_identity_survive_reopen() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.sqlite3");
        let store = Store::open(&path)?;
        store.insert_workspace(&workspace("w1"))?;
        store.insert_session(&Session {
            id: "s1".into(),
            workspace_id: "w1".into(),
            worktree: None,
            name: "chat".into(),
            agent: Some(AgentKind::Codex),
            mode: SessionMode::Ui,
            archived: false,
            starred: false,
            created_at: 1,
            updated_at: 1,
            last_started_at: None,
            status: SessionStatus::Stopped,
        })?;
        store.set_chat_model("s1", Some("gpt-test"))?;
        store.set_chat_thread("s1", "native-thread")?;
        store.set_chat_baseline_model("s1", "provider/default")?;
        store.set_chat_baseline_model("s1", "provider/other")?;
        assert_eq!(
            store.chat_permission("s1")?,
            ChatPermissionMode::ApprovalRequired
        );
        store.set_chat_permission("s1", ChatPermissionMode::Auto)?;
        let mut item = ChatItem {
            id: "a".into(),
            ordinal: 0,
            revision: 0,
            turn_id: String::new(),
            kind: crate::model::ChatItemKind::AssistantMessage,
            status: crate::model::ChatItemStatus::Running,
            title: String::new(),
            text: "hel".into(),
            detail: String::new(),
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
        };
        let first = store.put_chat_item("s1", item.clone())?;
        item.text = "hello".into();
        let second = store.put_chat_item("s1", item)?;
        assert_eq!(
            (
                first.ordinal,
                first.revision,
                second.ordinal,
                second.revision
            ),
            (1, 1, 1, 2)
        );
        drop(store);
        let store = Store::open(&path)?;
        assert_eq!(store.session("s1")?.mode, SessionMode::Ui);
        assert_eq!(store.chat_thread("s1")?.as_deref(), Some("native-thread"));
        assert_eq!(store.chat_model("s1")?.as_deref(), Some("gpt-test"));
        assert_eq!(
            store.chat_baseline_model("s1")?.as_deref(),
            Some("provider/default")
        );
        assert_eq!(store.chat_permission("s1")?, ChatPermissionMode::Auto);
        store.set_chat_model("s1", None)?;
        assert_eq!(store.chat_model("s1")?, None);
        assert_eq!(store.chat_items("s1")?, vec![second.clone()]);
        assert!(
            second.started_at > 0 && second.updated_at >= second.started_at,
            "the store stamps timing"
        );
        // Usage is replaced per turn and listed in the order turns started.
        let usage = |turn: &str, output: u64| ChatTurnUsage {
            turn_id: turn.into(),
            usage: crate::model::ChatUsage {
                output,
                ..Default::default()
            },
        };
        store.put_chat_usage("s1", &usage("t1", 5))?;
        store.put_chat_usage("s1", &usage("t2", 7))?;
        store.put_chat_usage("s1", &usage("t1", 9))?;
        assert_eq!(
            store.chat_usage("s1")?,
            vec![usage("t1", 9), usage("t2", 7)]
        );
        store.delete_workspace("w1")?;
        assert!(store.chat_items("s1")?.is_empty());
        assert!(store.chat_usage("s1")?.is_empty());
        Ok(())
    }

    #[test]
    fn plan_reviews_are_persisted_superseded_and_cas_guarded() -> Result<()> {
        let store = Store::in_memory()?;
        insert_chat(&store)?;
        let draft = store.put_plan_draft("s1", plan(PlanReviewStatus::Draft))?;
        assert_eq!(
            (draft.revision, draft.status, draft.complete),
            (0, PlanReviewStatus::Draft, false)
        );
        assert!(matches!(
            store.claim_plan_decision("s1", "plan-1", 0, PlanDecision::Approve, None),
            Err(Error::Conflict(_))
        ));

        let first = store.record_plan_review("s1", plan(PlanReviewStatus::AwaitingReview))?;
        assert_eq!(
            (first.revision, first.status),
            (1, PlanReviewStatus::AwaitingReview)
        );
        let second = store.record_plan_review("s1", plan(PlanReviewStatus::AwaitingReview))?;
        assert!(matches!(
            store.put_plan_draft("s1", plan(PlanReviewStatus::Draft)),
            Err(Error::Conflict(_))
        ));
        let persisted = store.chat_plans("s1")?;
        let old = persisted
            .iter()
            .find(|record| record.revision == first.revision)
            .unwrap();
        assert_eq!(old.status, PlanReviewStatus::Superseded);
        assert_eq!(old.superseded_by, Some(second.revision));

        drop(persisted);
        let claim = store.claim_plan_decision(
            "s1",
            "plan-1",
            second.revision,
            PlanDecision::Approve,
            None,
        )?;
        let PlanClaim::Claimed { review, previous } = claim else {
            panic!("the first decision must claim the revision");
        };
        assert_eq!(
            (review.status, previous),
            (
                PlanReviewStatus::Submitting,
                PlanReviewStatus::AwaitingReview
            )
        );
        let duplicate = store.claim_plan_decision(
            "s1",
            "plan-1",
            second.revision,
            PlanDecision::Approve,
            None,
        )?;
        assert!(matches!(
            duplicate,
            PlanClaim::Conflict(PlanReview {
                status: PlanReviewStatus::Submitting,
                ..
            })
        ));
        let settled = store
            .finish_plan_decision(
                "s1",
                "plan-1",
                second.revision,
                PlanDecision::Approve,
                PlanExecutionStatus::Starting,
            )?
            .unwrap();
        assert_eq!(
            (settled.status, settled.execution),
            (PlanReviewStatus::Approved, PlanExecutionStatus::Starting)
        );
        assert!(matches!(
            store.claim_plan_decision(
                "s1",
                "plan-1",
                second.revision,
                PlanDecision::Approve,
                None
            )?,
            PlanClaim::Conflict(PlanReview {
                status: PlanReviewStatus::Approved,
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn failed_plan_submission_reverts_and_changes_need_feedback() -> Result<()> {
        let store = Store::in_memory()?;
        insert_chat(&store)?;
        let review = store.record_plan_review("s1", plan(PlanReviewStatus::AwaitingReview))?;
        assert!(matches!(
            store.claim_plan_decision(
                "s1",
                "plan-1",
                review.revision,
                PlanDecision::RequestChanges,
                Some("  ")
            ),
            Err(Error::Invalid(_))
        ));
        let claim = store.claim_plan_decision(
            "s1",
            "plan-1",
            review.revision,
            PlanDecision::Approve,
            None,
        )?;
        let PlanClaim::Claimed { previous, .. } = claim else {
            panic!("the decision should claim");
        };
        let reverted = store
            .revert_plan_submission("s1", "plan-1", review.revision, previous)?
            .unwrap();
        assert_eq!(reverted.status, PlanReviewStatus::AwaitingReview);
        let expired = store.expire_open_plans("s1")?;
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].status, PlanReviewStatus::Expired);
        Ok(())
    }

    #[test]
    fn finishing_a_plan_decision_does_not_revive_a_settled_execution() -> Result<()> {
        let store = Store::in_memory()?;
        insert_chat(&store)?;
        let review = store.record_plan_review("s1", plan(PlanReviewStatus::AwaitingReview))?;
        assert!(matches!(
            store.claim_plan_decision(
                "s1",
                "plan-1",
                review.revision,
                PlanDecision::Approve,
                None
            )?,
            PlanClaim::Claimed { .. }
        ));
        store.set_plan_execution(
            "s1",
            "plan-1",
            review.revision,
            PlanExecutionStatus::Completed,
        )?;
        let settled = store
            .finish_plan_decision(
                "s1",
                "plan-1",
                review.revision,
                PlanDecision::Approve,
                PlanExecutionStatus::Starting,
            )?
            .unwrap();
        assert_eq!(settled.status, PlanReviewStatus::Approved);
        assert_eq!(settled.execution, PlanExecutionStatus::Completed);
        Ok(())
    }

    #[test]
    fn inactive_chats_expire_open_plans_but_keep_terminal_history() -> Result<()> {
        let store = Store::in_memory()?;
        insert_chat(&store)?;
        let approved = store.record_plan_review("s1", plan(PlanReviewStatus::AwaitingReview))?;
        let claimed = store.claim_plan_decision(
            "s1",
            "plan-1",
            approved.revision,
            PlanDecision::Approve,
            None,
        )?;
        assert!(matches!(claimed, PlanClaim::Claimed { .. }));
        store.finish_plan_decision(
            "s1",
            "plan-1",
            approved.revision,
            PlanDecision::Approve,
            PlanExecutionStatus::Completed,
        )?;
        assert!(store.expire_open_plans("s1")?.is_empty());
        assert_eq!(
            store
                .chat_plan("s1", "plan-1", approved.revision)?
                .unwrap()
                .status,
            PlanReviewStatus::Approved
        );
        Ok(())
    }

    #[test]
    fn subagent_history_survives_updates_and_settles_with_the_chat() -> Result<()> {
        let store = Store::in_memory()?;
        insert_chat(&store)?;
        let first = store.put_chat_subagent("s1", subagent())?;
        let mut update = first.clone();
        update.state = crate::model::SubagentState::Completed;
        update.label = "Explored the store".into();
        let second = store.put_chat_subagent("s1", update)?;
        assert_eq!(second.started_at, first.started_at);
        assert_eq!(store.chat_subagents("s1")?, vec![second.clone()]);

        let mut active = subagent();
        active.id = "agent-2".into();
        store.put_chat_subagent("s1", active)?;
        let stopped = store.settle_active_subagents("s1")?;
        assert_eq!(stopped.len(), 1);
        assert_eq!(stopped[0].id, "agent-2");
        assert_eq!(stopped[0].state, crate::model::SubagentState::Stopped);
        assert_eq!(store.chat_subagents("s1")?.len(), 2);
        Ok(())
    }

    #[test]
    fn settings_round_trip() -> Result<()> {
        let store = Store::in_memory()?;
        assert_eq!(store.setting::<String>("x")?, None);
        store.set_setting("x", &"y")?;
        assert_eq!(store.setting::<String>("x")?.as_deref(), Some("y"));
        Ok(())
    }

    #[test]
    fn network_history_is_bounded_and_pages_by_id() -> Result<()> {
        let store = Store::in_memory()?;
        for i in 0..505 {
            store.append_network_history("check", &format!("attempt {i}"), 1, None)?;
        }
        let latest = store.network_history(None, 100)?;
        assert_eq!(latest.len(), 100);
        assert_eq!(latest[0].message, "attempt 504");
        let previous = store.network_history(Some(latest[99].id), 100)?;
        assert_eq!(previous[0].message, "attempt 404");
        let all_ids: Vec<_> = std::iter::successors(Some(latest), |page| {
            let before = page.last()?.id;
            let next = store.network_history(Some(before), 100).unwrap();
            (!next.is_empty()).then_some(next)
        })
        .flatten()
        .map(|entry| entry.id)
        .collect();
        assert_eq!(
            store.network_history(None, NETWORK_HISTORY_LIMIT)?.len(),
            all_ids.len()
        );
        assert_eq!(all_ids.len(), NETWORK_HISTORY_LIMIT as usize);
        assert_eq!(*all_ids.last().unwrap(), 6);
        Ok(())
    }

    #[test]
    fn duplicate_subscription_name_is_a_conflict() -> Result<()> {
        let store = Store::in_memory()?;
        let source = |id: &str| StoredSource {
            id: id.into(),
            name: "same".into(),
            url: None,
            nodes_json: "[]".into(),
            updated_at: 1,
        };
        store.upsert_proxy_source(&source("a"))?;
        assert!(matches!(
            store.upsert_proxy_source(&source("b")),
            Err(Error::Conflict(_))
        ));
        Ok(())
    }
}
