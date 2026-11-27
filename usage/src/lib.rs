//! Usage imported from the agents' own guest-side session stores.
//!
//! Session-log layouts are informed by cc-switch. The Antigravity importer
//! follows cc-switch v4.0.38 (MIT, Copyright 2025 Jason Young). See
//! CC-SWITCH-LICENSE.txt.
//! Prices come from models.dev. Input excludes cache reads/writes throughout
//! this crate.

pub mod pricing;
mod sources;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{Offset, TimeZone};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsagePrice"))]
#[serde(rename_all = "camelCase")]
pub struct Price {
    pub model: String,
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsageFilter"))]
#[serde(rename_all = "camelCase")]
pub struct Filter {
    #[cfg_attr(feature = "bindings", ts(type = "number | null"))]
    pub from_ms: Option<i64>,
    #[cfg_attr(feature = "bindings", ts(type = "number | null"))]
    pub to_ms: Option<i64>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub limit: u32,
    pub offset: u32,
    #[serde(default)]
    pub host_zone: Option<String>,
    #[serde(default)]
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub clock_offset: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsageCounts"))]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub requests: u64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub input: u64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub output: u64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub cache_read: u64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub cache_write: u64,
    pub cost: f64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub unpriced_requests: u64,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsageLog"))]
#[serde(rename_all = "camelCase")]
pub struct Log {
    pub id: String,
    pub agent: String,
    pub session_id: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    /// Unix milliseconds.
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub timestamp: i64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub input: u64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub output: u64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub cache_read: u64,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub cache_write: u64,
    pub cost: Option<f64>,
    #[cfg_attr(feature = "bindings", ts(type = "number | null"))]
    pub duration_ms: Option<i64>,
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsageDaily"))]
pub struct Daily {
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub timestamp: i64,
    #[serde(flatten)]
    pub counts: Counts,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsageModel"))]
pub struct Model {
    pub model: String,
    #[serde(flatten)]
    pub counts: Counts,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsageProvider"))]
pub struct Provider {
    pub provider: String,
    #[serde(flatten)]
    pub counts: Counts,
}

/// One agent's share of a period (the agents hub lists every agent with it).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsageAgent"))]
pub struct AgentUsage {
    pub agent: String,
    #[serde(flatten)]
    pub counts: Counts,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsageDashboard"))]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub summary: Counts,
    pub daily: Vec<Daily>,
    pub models: Vec<Model>,
    pub providers: Vec<Provider>,
    /// Per agent, under the same filter.
    pub agents: Vec<AgentUsage>,
    pub logs: Vec<Log>,
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub total: u64,
    pub prices: Vec<Price>,
    pub price_sources: Vec<pricing::SourceStatus>,
}

#[derive(Debug, Default, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "UsageSyncResult"))]
pub struct SyncResult {
    #[cfg_attr(feature = "bindings", ts(type = "number"))]
    pub imported: u64,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Record {
    pub id: String,
    pub agent: &'static str,
    pub session_id: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    pub timestamp: i64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reported_cost: Option<f64>,
    pub duration_ms: Option<i64>,
    pub status: Option<String>,
    pub complete: bool,
}

pub struct Usage {
    conn: Connection,
}

impl Usage {
    pub fn open(path: &Path) -> Result<Self> {
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS usage_records (
               id TEXT PRIMARY KEY, agent TEXT NOT NULL, session_id TEXT, model TEXT NOT NULL,
               provider TEXT, timestamp INTEGER NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL,
               cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL, reported_cost REAL,
               duration_ms INTEGER, status TEXT, complete INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS usage_time ON usage_records(timestamp DESC);
             CREATE TABLE IF NOT EXISTS source_revisions (path TEXT PRIMARY KEY, revision TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS antigravity_selections (
               id TEXT PRIMARY KEY REFERENCES usage_records(id) ON DELETE CASCADE,
               model TEXT NOT NULL
             );"
        )?;
        pricing::schema(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn sync_prices<F>(
        &mut self,
        now: i64,
        force: bool,
        fetch: F,
    ) -> Result<Vec<pricing::SourceStatus>>
    where
        F: FnMut(&str) -> std::result::Result<String, String>,
    {
        pricing::sync(&mut self.conn, now, force, fetch)
    }

    pub fn save_price(&self, price: &Price) -> Result<()> {
        if price.model.trim().is_empty()
            || price.model.len() > 512
            || [
                price.input,
                price.output,
                price.cache_read,
                price.cache_write,
            ]
            .iter()
            .any(|v| !v.is_finite() || *v < 0.0)
        {
            return Err("Model and non-negative finite prices are required".into());
        }
        self.conn.execute("INSERT INTO price_manual VALUES (?1,?2,?3,?4,?5) ON CONFLICT(model) DO UPDATE SET input=excluded.input,output=excluded.output,cache_read=excluded.cache_read,cache_write=excluded.cache_write", params![price.model.trim(), price.input, price.output, price.cache_read, price.cache_write])?;
        Ok(())
    }

    pub fn sync(&mut self, home: &Path) -> Result<SyncResult> {
        sources::sync(&mut self.conn, home)
    }

    pub fn dashboard(&self, filter: &Filter) -> Result<Dashboard> {
        let catalog = pricing::Catalog::load(&self.conn)?;
        let prices = catalog.display_prices();
        let mut summary = Counts::default();
        let mut daily = BTreeMap::<i64, Counts>::new();
        let zone = filter
            .host_zone
            .as_deref()
            .and_then(|zone| zone.parse::<chrono_tz::Tz>().ok())
            .unwrap_or(chrono_tz::UTC);
        let mut models = BTreeMap::<String, Counts>::new();
        let mut providers = BTreeMap::<String, Counts>::new();
        let mut agents = BTreeMap::<String, Counts>::new();
        let mut logs = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT id,agent,session_id,model,provider,timestamp,input,output,cache_read,cache_write,reported_cost,duration_ms,status
             FROM usage_records WHERE (?1 IS NULL OR timestamp >= ?1) AND (?2 IS NULL OR timestamp <= ?2)
             AND (?3 IS NULL OR agent = ?3) AND (?4 IS NULL OR model = ?4) ORDER BY timestamp DESC,id DESC")?;
        let mut rows = stmt.query(params![
            filter.from_ms,
            filter.to_ms,
            filter.agent,
            filter.model
        ])?;
        while let Some(row) = rows.next()? {
            let reported = row
                .get::<_, Option<f64>>(10)?
                .filter(|cost| cost.is_finite() && *cost >= 0.0);
            let mut log = Log {
                id: row.get(0)?,
                agent: row.get(1)?,
                session_id: row.get(2)?,
                model: row.get(3)?,
                provider: row.get(4)?,
                timestamp: row.get(5)?,
                input: row.get(6)?,
                output: row.get(7)?,
                cache_read: row.get(8)?,
                cache_write: row.get(9)?,
                cost: None,
                duration_ms: row.get(11)?,
                status: row.get(12)?,
            };
            // Provider-reported cost wins. Keep unknown prices absent from the total.
            let tokens = pricing::Tokens {
                input: log.input,
                output: log.output,
                cache_read: log.cache_read,
                cache_write: log.cache_write,
            };
            log.cost = reported.or_else(|| {
                catalog.cost(
                    &log.agent,
                    &log.model,
                    log.provider.as_deref(),
                    &tokens,
                    log.timestamp,
                )
            });
            let host_ms = log.timestamp.saturating_sub(filter.clock_offset);
            let day = chrono::DateTime::from_timestamp_millis(host_ms)
                .and_then(|instant| {
                    let local = instant.with_timezone(&zone);
                    let date = instant
                        .naive_utc()
                        .checked_add_offset(local.offset().fix())?
                        .date();
                    (0..24)
                        .find_map(|hour| {
                            date.and_hms_opt(hour, 0, 0)
                                .and_then(|local| zone.from_local_datetime(&local).earliest())
                        })
                        .map(|start| start.timestamp_millis().saturating_add(filter.clock_offset))
                })
                .unwrap_or(log.timestamp);
            accumulate(&mut summary, &log);
            accumulate(daily.entry(day).or_default(), &log);
            accumulate(models.entry(log.model.clone()).or_default(), &log);
            accumulate(agents.entry(log.agent.clone()).or_default(), &log);
            accumulate(
                providers
                    .entry(log.provider.clone().unwrap_or_else(|| "Unknown".into()))
                    .or_default(),
                &log,
            );
            let position = summary.requests - 1;
            if position >= filter.offset as u64 && logs.len() < filter.limit.clamp(1, 200) as usize
            {
                logs.push(log);
            }
        }
        Ok(Dashboard {
            total: summary.requests,
            summary,
            daily: daily
                .into_iter()
                .map(|(timestamp, counts)| Daily { timestamp, counts })
                .collect(),
            models: models
                .into_iter()
                .map(|(model, counts)| Model { model, counts })
                .collect(),
            providers: providers
                .into_iter()
                .map(|(provider, counts)| Provider { provider, counts })
                .collect(),
            agents: agents
                .into_iter()
                .map(|(agent, counts)| AgentUsage { agent, counts })
                .collect(),
            logs,
            prices,
            price_sources: vec![pricing::status(&self.conn)?],
        })
    }
}

fn accumulate(counts: &mut Counts, log: &Log) {
    counts.requests += 1;
    counts.input = counts.input.saturating_add(log.input);
    counts.output = counts.output.saturating_add(log.output);
    counts.cache_read = counts.cache_read.saturating_add(log.cache_read);
    counts.cache_write = counts.cache_write.saturating_add(log.cache_write);
    if let Some(cost) = log.cost {
        counts.cost = (counts.cost + cost).min(f64::MAX);
    } else {
        counts.unpriced_requests += 1;
    }
}

pub(crate) fn insert(conn: &Connection, record: &Record) -> Result<bool> {
    let exists: bool = conn
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM usage_records WHERE id=?1)")?
        .query_row([&record.id], |r| r.get(0))?;
    conn.prepare_cached("INSERT INTO usage_records VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
      ON CONFLICT(id) DO UPDATE SET session_id=excluded.session_id,model=excluded.model,provider=excluded.provider,
      timestamp=excluded.timestamp,input=excluded.input,output=excluded.output,cache_read=excluded.cache_read,
      cache_write=excluded.cache_write,reported_cost=excluded.reported_cost,duration_ms=excluded.duration_ms,
      status=excluded.status,complete=excluded.complete
      WHERE excluded.complete > usage_records.complete OR (excluded.complete = usage_records.complete AND excluded.output >= usage_records.output)")?
      .execute(params![record.id, record.agent, record.session_id, record.model, record.provider, record.timestamp,
        record.input, record.output, record.cache_read, record.cache_write, record.reported_cost,
        record.duration_ms, record.status, record.complete])?;
    Ok(!exists)
}

pub(crate) fn revision(conn: &Connection, path: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT revision FROM source_revisions WHERE path=?1",
            [path],
            |r| r.get(0),
        )
        .optional()?)
}
