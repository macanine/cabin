//! Model prices from models.dev.
//!
//! The catalog carries the rules, not just four numbers: context-token bands,
//! UTC time-of-day/day/date multipliers, and service-tier multipliers. We
//! evaluate the conditions our session logs can answer and skip the rest rather
//! than guess (for example, a `priority` service tier is not recorded).
//!
//! Fetched with the guest's network. On any failure the previous catalog stays
//! in place; a malformed or truncated catalog never replaces it.

use std::collections::BTreeMap;

use chrono::{Datelike, Timelike};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Price, Result};

pub const MODELS_URL: &str = "https://models.dev/experimental/v2.0/api.json";
pub const INTERVAL_MS: i64 = 6 * 60 * 60 * 1000;
const RETRY_MS: i64 = 30 * 60 * 1000;
/// A genuine catalog has thousands of text-priced models. Below this the
/// response is a truncated page, an error document or a format change.
const MIN_MODELS: usize = 1000;

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[cfg_attr(feature = "bindings", ts(rename = "PriceSourceStatus"))]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub source: String,
    #[cfg_attr(feature = "bindings", ts(type = "number | null"))]
    pub last_success: Option<i64>,
    #[cfg_attr(feature = "bindings", ts(type = "number | null"))]
    pub last_attempt: Option<i64>,
    pub error: Option<String>,
    pub count: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Rates {
    fn cost(&self, tokens: &Tokens, factor: &Factor) -> Option<f64> {
        let cost = tokens.input as f64 / 1_000_000.0 * self.input * factor.input
            + tokens.output as f64 / 1_000_000.0 * self.output * factor.output
            + tokens.cache_read as f64 / 1_000_000.0 * self.cache_read * factor.cache_read
            + tokens.cache_write as f64 / 1_000_000.0 * self.cache_write * factor.cache_write;
        (cost.is_finite() && cost >= 0.0).then_some(cost)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct PartialRates {
    #[serde(default)]
    input: Option<f64>,
    #[serde(default)]
    output: Option<f64>,
    #[serde(default)]
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Option<f64>,
}

impl PartialRates {
    fn applied(&self, base: &Rates) -> Rates {
        Rates {
            input: self.input.unwrap_or(base.input),
            output: self.output.unwrap_or(base.output),
            cache_read: self.cache_read.unwrap_or(base.cache_read),
            cache_write: self.cache_write.unwrap_or(base.cache_write),
        }
    }
}

/// A condition from models.dev's `when` object. `Unsupported` means the
/// condition uses request facts our logs do not carry; it never matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
enum Condition {
    ContextGt(u64),
    ContextGte(u64),
    UtcHours {
        #[serde(default)]
        gte: Option<u16>,
        #[serde(default)]
        lt: Option<u16>,
    },
    UtcDays(u8),
    DateGte(String),
    DateLt(String),
    Not(Box<Condition>),
    Any(Vec<Condition>),
    All(Vec<Condition>),
    Unsupported,
}

#[derive(Debug, Clone, Copy)]
struct Facts {
    context: u128,
    utc_minute: u16,
    weekday: u8,
    date: (i32, u32, u32),
}

impl Condition {
    fn eval(&self, facts: &Facts) -> Option<bool> {
        match self {
            Self::ContextGt(threshold) => Some(facts.context > u128::from(*threshold)),
            Self::ContextGte(threshold) => Some(facts.context >= u128::from(*threshold)),
            Self::UtcHours { gte, lt } => {
                let minute = facts.utc_minute;
                match (*gte, *lt) {
                    (Some(start), Some(end)) if start > end => {
                        Some(minute >= start || minute < end)
                    }
                    (Some(start), Some(end)) => Some(minute >= start && minute < end),
                    (Some(start), None) => Some(minute >= start),
                    (None, Some(end)) => Some(minute < end),
                    (None, None) => None,
                }
            }
            Self::UtcDays(mask) => Some(mask & (1 << facts.weekday) != 0),
            Self::DateGte(date) => Some(
                format!(
                    "{:04}-{:02}-{:02}",
                    facts.date.0, facts.date.1, facts.date.2
                )
                .as_str()
                    >= date.as_str(),
            ),
            Self::DateLt(date) => Some(
                format!(
                    "{:04}-{:02}-{:02}",
                    facts.date.0, facts.date.1, facts.date.2
                )
                .as_str()
                    < date.as_str(),
            ),
            Self::Not(inner) => inner.eval(facts).map(|value| !value),
            Self::Any(parts) => {
                let mut unknown = false;
                for part in parts {
                    match part.eval(facts) {
                        Some(true) => return Some(true),
                        Some(false) => {}
                        None => unknown = true,
                    }
                }
                (!unknown).then_some(false)
            }
            Self::All(parts) => {
                let mut unknown = false;
                for part in parts {
                    match part.eval(facts) {
                        Some(false) => return Some(false),
                        Some(true) => {}
                        None => unknown = true,
                    }
                }
                (!unknown).then_some(true)
            }
            Self::Unsupported => None,
        }
    }

    fn context_threshold(&self) -> Option<u64> {
        match self {
            Self::ContextGt(value) => Some(*value),
            Self::ContextGte(value) => Some(*value),
            Self::All(parts) => parts.iter().filter_map(Self::context_threshold).max(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Band {
    condition: Condition,
    rates: PartialRates,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct Factor {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

impl Default for Factor {
    fn default() -> Self {
        Self {
            input: 1.0,
            output: 1.0,
            cache_read: 1.0,
            cache_write: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Multiplier {
    condition: Condition,
    factor: Factor,
}

/// The stored, evaluated form of one model's text prices.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Rule {
    base: Rates,
    bands: Vec<Band>,
    multipliers: Vec<Multiplier>,
}

impl Rule {
    #[cfg(test)]
    pub(crate) fn base(&self) -> Rates {
        self.base
    }

    /// Cost in USD for one request. `timestamp` is Unix ms; models.dev time
    /// conditions are UTC.
    pub(crate) fn cost(&self, tokens: &Tokens, timestamp: i64) -> Option<f64> {
        let instant = chrono::DateTime::from_timestamp_millis(timestamp)?;
        let facts = Facts {
            context: u128::from(tokens.input)
                + u128::from(tokens.cache_read)
                + u128::from(tokens.cache_write),
            utc_minute: (instant.hour() * 60 + instant.minute()) as u16,
            weekday: instant.weekday().num_days_from_monday() as u8,
            date: (instant.year(), instant.month(), instant.day()),
        };
        let mut rates = self.base;
        let mut selected: Option<(u64, &Band)> = None;
        for band in &self.bands {
            if band.condition.eval(&facts) != Some(true) {
                continue;
            }
            let threshold = band.condition.context_threshold().unwrap_or(0);
            if selected.is_none_or(|(current, _)| threshold >= current) {
                selected = Some((threshold, band));
            }
        }
        if let Some((_, band)) = selected {
            rates = band.rates.applied(&rates);
        }
        let mut factor = Factor::default();
        for multiplier in &self.multipliers {
            if multiplier.condition.eval(&facts) != Some(true) {
                continue;
            }
            factor.input *= multiplier.factor.input;
            factor.output *= multiplier.factor.output;
            factor.cache_read *= multiplier.factor.cache_read;
            factor.cache_write *= multiplier.factor.cache_write;
        }
        rates.cost(tokens, &factor)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && (0.0..=1_000_000.0).contains(v))
}

/// Cache writes may carry several TTLs. Session logs do not say which TTL was
/// used, so take the short (5m) price, then any listed price.
fn cache_write(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::Object(ttls)) => number(ttls.get("5m"))
            .or_else(|| number(ttls.get("1h")))
            .or_else(|| {
                ttls.values()
                    .filter_map(|value| number(Some(value)))
                    .min_by(f64::total_cmp)
            }),
        other => number(other),
    }
}

fn parse_hhmm(value: Option<&Value>) -> Option<u16> {
    let (hour, minute) = value?.as_str()?.split_once(':')?;
    let hour = hour.parse::<u16>().ok()?;
    let minute = minute.parse::<u16>().ok()?;
    if (hour < 24 && minute < 60) || (hour == 24 && minute == 0) {
        Some(hour * 60 + minute)
    } else {
        None
    }
}

fn parse_days(value: Option<&Value>) -> Option<u8> {
    let value = value?;
    let list = value
        .as_array()
        .or_else(|| value.get("in").and_then(Value::as_array))?;
    let mut mask = 0u8;
    for day in list {
        let index = match day.as_str()? {
            "mon" => 0,
            "tue" => 1,
            "wed" => 2,
            "thu" => 3,
            "fri" => 4,
            "sat" => 5,
            "sun" => 6,
            _ => return None,
        };
        mask |= 1 << index;
    }
    Some(mask)
}

fn parse_condition(value: &Value) -> Condition {
    let Some(object) = value.as_object() else {
        return Condition::Unsupported;
    };
    let mut parts = Vec::new();
    for (key, value) in object {
        let condition = match key.as_str() {
            "context" => {
                let mut bounds = Vec::new();
                if let Some(value) = value.get("gt").and_then(Value::as_u64) {
                    bounds.push(Condition::ContextGt(value));
                }
                if let Some(value) = value.get("gte").and_then(Value::as_u64) {
                    bounds.push(Condition::ContextGte(value));
                }
                match bounds.len() {
                    0 => None,
                    1 => bounds.pop(),
                    _ => Some(Condition::All(bounds)),
                }
            }
            "utc_hours" => {
                let gte = parse_hhmm(value.get("gte"));
                let lt = parse_hhmm(value.get("lt"));
                if (gte.is_none() && value.get("gte").is_some())
                    || (lt.is_none() && value.get("lt").is_some())
                {
                    None
                } else {
                    (gte.is_some() || lt.is_some()).then_some(Condition::UtcHours { gte, lt })
                }
            }
            "utc_days" => parse_days(Some(value)).map(Condition::UtcDays),
            "date" => {
                let mut bounds = Vec::new();
                if let Some(date) = value.get("gte").and_then(Value::as_str) {
                    bounds.push(Condition::DateGte(date.to_string()));
                }
                if let Some(date) = value.get("lt").and_then(Value::as_str) {
                    bounds.push(Condition::DateLt(date.to_string()));
                }
                match bounds.len() {
                    0 => None,
                    1 => bounds.pop(),
                    _ => Some(Condition::All(bounds)),
                }
            }
            "not" => Some(Condition::Not(Box::new(parse_condition(value)))),
            "any" => value
                .as_array()
                .map(|items| Condition::Any(items.iter().map(parse_condition).collect())),
            "all" => value
                .as_array()
                .map(|items| Condition::All(items.iter().map(parse_condition).collect())),
            _ => None,
        };
        parts.push(condition.unwrap_or(Condition::Unsupported));
    }
    match parts.len() {
        0 => Condition::Unsupported,
        1 => parts.pop().unwrap_or(Condition::Unsupported),
        _ => Condition::All(parts),
    }
}

fn parse_factor(value: &Value) -> Factor {
    let mut factor = Factor::default();
    match value {
        Value::Number(_) => {
            let value = number(Some(value)).unwrap_or(1.0);
            factor = Factor {
                input: value,
                output: value,
                cache_read: value,
                cache_write: value,
            };
        }
        Value::Object(object) => {
            let Some(text) = object.get("text") else {
                return factor;
            };
            match text {
                Value::Number(_) => {
                    let value = number(Some(text)).unwrap_or(1.0);
                    factor = Factor {
                        input: value,
                        output: value,
                        cache_read: value,
                        cache_write: value,
                    };
                }
                Value::Object(text) => {
                    if let Some(input) = text.get("input") {
                        match input {
                            Value::Number(_) => {
                                factor.input = number(Some(input)).unwrap_or(1.0);
                            }
                            Value::Object(input) => {
                                if let Some(value) = number(input.get("price")) {
                                    factor.input = value;
                                }
                                if let Some(cache) = input.get("cache") {
                                    if let Some(value) = number(cache.get("read")) {
                                        factor.cache_read = value;
                                    }
                                    if let Some(value) = cache_write(Some(&cache["write"])) {
                                        factor.cache_write = value;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    match text.get("output") {
                        Some(output @ Value::Number(_)) => {
                            factor.output = number(Some(output)).unwrap_or(1.0);
                        }
                        Some(Value::Object(output)) => {
                            if let Some(value) = number(output.get("price")) {
                                factor.output = value;
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
    factor
}

impl Rule {
    fn from_cost(cost: &Value) -> Option<Self> {
        let text = cost.get("text")?.as_object()?;
        let input = text.get("input").filter(|v| v.is_object());
        let output = text.get("output").filter(|v| v.is_object());
        // Per-character or per-image text prices cannot use token counts.
        if input
            .and_then(|v| v.get("unit"))
            .is_some_and(|unit| unit != "token")
            || output
                .and_then(|v| v.get("unit"))
                .is_some_and(|unit| unit != "token")
        {
            return None;
        }
        let base = Rates {
            input: number(input.and_then(|v| v.get("price"))).unwrap_or(0.0),
            output: number(output.and_then(|v| v.get("price"))).unwrap_or(0.0),
            cache_read: number(
                input
                    .and_then(|v| v.get("cache"))
                    .and_then(|v| v.get("read")),
            )
            .unwrap_or(0.0),
            cache_write: cache_write(
                input
                    .and_then(|v| v.get("cache"))
                    .and_then(|v| v.get("write")),
            )
            .unwrap_or(0.0),
        };
        if base == Rates::default() {
            return None;
        }
        let mut bands: BTreeMap<String, Band> = BTreeMap::new();
        for (side, source) in [("input", input), ("output", output)] {
            let Some(overrides) = source
                .and_then(|v| v.get("overrides"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for entry in overrides {
                let condition = parse_condition(entry.get("when").unwrap_or(&Value::Null));
                if condition.context_threshold().is_none() {
                    continue;
                }
                let key = serde_json::to_string(&condition).ok()?;
                let band = bands.entry(key).or_insert_with(|| Band {
                    condition: condition.clone(),
                    rates: PartialRates::default(),
                });
                if side == "input" {
                    band.rates.input = number(entry.get("price"));
                    band.rates.cache_read = number(entry.get("cache").and_then(|v| v.get("read")));
                    band.rates.cache_write =
                        cache_write(entry.get("cache").and_then(|v| v.get("write")));
                } else {
                    band.rates.output = number(entry.get("price"));
                }
            }
        }
        let mut multipliers = Vec::new();
        for entry in cost
            .get("multipliers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let condition = parse_condition(entry.get("when").unwrap_or(&Value::Null));
            let factor = parse_factor(entry.get("factor").unwrap_or(&Value::Null));
            let default = Factor::default();
            if (
                factor.input,
                factor.output,
                factor.cache_read,
                factor.cache_write,
            ) == (
                default.input,
                default.output,
                default.cache_read,
                default.cache_write,
            ) {
                continue;
            }
            multipliers.push(Multiplier { condition, factor });
        }
        Some(Self {
            base,
            bands: bands.into_values().collect(),
            multipliers,
        })
    }
}

fn parse_models(text: &str) -> Result<Vec<((String, String), Rule)>> {
    let root: Value = serde_json::from_str(text)?;
    let providers = root
        .as_object()
        .ok_or("models.dev catalog is not an object")?;
    let mut result = Vec::new();
    for (provider, data) in providers {
        let Some(models) = data.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (id, model) in models {
            let Some(rule) = model.get("cost").and_then(Rule::from_cost) else {
                continue;
            };
            let model_id = model.get("id").and_then(Value::as_str).unwrap_or(id);
            if model_id.is_empty()
                || model_id.len() > 512
                || provider.is_empty()
                || provider.len() > 512
            {
                continue;
            }
            result.push(((provider.clone(), model_id.to_string()), rule));
        }
    }
    if result.len() < MIN_MODELS {
        return Err(format!(
            "models.dev catalog has only {} text-priced models; refusing a truncated or changed response",
            result.len()
        )
        .into());
    }
    Ok(result)
}

pub(crate) fn schema(conn: &mut Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS price_manual (
           model TEXT PRIMARY KEY, input REAL NOT NULL, output REAL NOT NULL,
           cache_read REAL NOT NULL, cache_write REAL NOT NULL
         );
         CREATE TABLE IF NOT EXISTS model_prices (
           provider TEXT NOT NULL, model TEXT NOT NULL, rule TEXT NOT NULL,
           PRIMARY KEY (provider, model)
         );
         CREATE TABLE IF NOT EXISTS price_sync (
           source TEXT PRIMARY KEY, last_success INTEGER, last_attempt INTEGER,
           error TEXT, count INTEGER NOT NULL DEFAULT 0
         );",
    )?;
    Ok(())
}

pub(crate) fn status(conn: &Connection) -> Result<SourceStatus> {
    Ok(conn
        .query_row(
            "SELECT last_success,last_attempt,error,count FROM price_sync WHERE source='models.dev'",
            [],
            |r| {
                Ok(SourceStatus {
                    source: "models.dev".into(),
                    last_success: r.get(0)?,
                    last_attempt: r.get(1)?,
                    error: r.get(2)?,
                    count: r.get(3)?,
                })
            },
        )
        .optional()?
        .unwrap_or(SourceStatus {
            source: "models.dev".into(),
            last_success: None,
            last_attempt: None,
            error: None,
            count: 0,
        }))
}

pub(crate) fn sync<F>(
    conn: &mut Connection,
    now: i64,
    force: bool,
    mut fetch: F,
) -> Result<Vec<SourceStatus>>
where
    F: FnMut(&str) -> std::result::Result<String, String>,
{
    let prior = status(conn)?;
    let delay = if prior.error.is_some() {
        RETRY_MS
    } else {
        INTERVAL_MS
    };
    if !force
        && prior
            .last_attempt
            .is_some_and(|at| now >= at && now.saturating_sub(at) < delay)
    {
        return Ok(vec![prior]);
    }
    match fetch(MODELS_URL)
        .map_err(|error| error.to_string())
        .and_then(|text| parse_models(&text).map_err(|error| error.to_string()))
    {
        Ok(rules) => {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM model_prices", [])?;
            {
                let mut insert = tx.prepare(
                    "INSERT INTO model_prices (provider, model, rule) VALUES (?1, ?2, ?3)",
                )?;
                for ((provider, model), rule) in &rules {
                    insert.execute(params![provider, model, serde_json::to_string(rule)?])?;
                }
            }
            tx.execute(
                "INSERT INTO price_sync VALUES ('models.dev',?1,?1,NULL,?2)
                 ON CONFLICT(source) DO UPDATE SET last_success=?1,last_attempt=?1,error=NULL,count=?2",
                params![now, rules.len() as u32],
            )?;
            tx.commit()?;
        }
        Err(error) => {
            conn.execute(
                "INSERT INTO price_sync(source,last_attempt,error,count) VALUES ('models.dev',?1,?2,0)
                 ON CONFLICT(source) DO UPDATE SET last_attempt=?1,error=?2",
                params![now, error.chars().take(500).collect::<String>()],
            )?;
        }
    }
    Ok(vec![status(conn)?])
}

pub(crate) struct Catalog {
    manual: BTreeMap<String, Rates>,
    rules: BTreeMap<(String, String), Rule>,
    unique: BTreeMap<String, Option<(String, String)>>,
}

impl Catalog {
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut manual = BTreeMap::new();
        let mut query =
            conn.prepare("SELECT model,input,output,cache_read,cache_write FROM price_manual")?;
        for row in query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Rates {
                    input: r.get(1)?,
                    output: r.get(2)?,
                    cache_read: r.get(3)?,
                    cache_write: r.get(4)?,
                },
            ))
        })? {
            let (model, rates) = row?;
            manual.insert(model, rates);
        }
        let mut rules = BTreeMap::new();
        let mut unique = BTreeMap::new();
        let mut query = conn.prepare("SELECT provider,model,rule FROM model_prices")?;
        for row in query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (provider, model, text) = row?;
            let Ok(rule) = serde_json::from_str::<Rule>(&text) else {
                continue;
            };
            let key = (provider, model.clone());
            rules.insert(key.clone(), rule);
            unique
                .entry(model)
                .and_modify(|entry: &mut Option<(String, String)>| {
                    if entry.as_ref() != Some(&key) {
                        *entry = None;
                    }
                })
                .or_insert(Some(key));
        }
        Ok(Self {
            manual,
            rules,
            unique,
        })
    }

    pub fn display_prices(&self) -> Vec<Price> {
        self.manual
            .iter()
            .map(|(model, rates)| Price {
                model: model.clone(),
                input: rates.input,
                output: rates.output,
                cache_read: rates.cache_read,
                cache_write: rates.cache_write,
            })
            .collect()
    }

    /// Cost for one request. Manual overrides win; otherwise the provider
    /// (from the log, the model prefix, or the agent's official API) selects
    /// the models.dev entry, falling back to a model only one provider serves.
    pub fn cost(
        &self,
        agent: &str,
        model: &str,
        provider: Option<&str>,
        tokens: &Tokens,
        timestamp: i64,
    ) -> Option<f64> {
        let (prefix, short) = model
            .rsplit_once('/')
            .map_or((None, model), |(provider, model)| (Some(provider), model));
        let alias = undated(short);
        let agy_aliases = if agent == "antigravity" {
            antigravity_price_aliases(alias)
        } else {
            Vec::new()
        };
        let names = [Some(short), (alias != short).then_some(alias)]
            .into_iter()
            .flatten()
            .chain(agy_aliases.iter().map(String::as_str));
        for name in names.clone() {
            if let Some(rates) = self.manual.get(name).or_else(|| {
                provider.and_then(|provider| self.manual.get(&format!("{provider}/{name}")))
            }) {
                return rates.cost(tokens, &Factor::default());
            }
        }
        let providers = [provider, prefix, default_provider(agent, short)];
        for provider in providers.into_iter().flatten() {
            for name in names.clone() {
                let key = (normalize_provider(provider), name.to_string());
                if let Some(rule) = self.rules.get(&key) {
                    return rule.cost(tokens, timestamp);
                }
            }
        }
        names.into_iter().find_map(|name| {
            self.unique
                .get(name)
                .and_then(|entry| entry.as_ref())
                .and_then(|key| self.rules.get(key))
                .and_then(|rule| rule.cost(tokens, timestamp))
        })
    }
}

fn normalize_provider(provider: &str) -> String {
    let provider = provider.trim().to_ascii_lowercase();
    match provider.as_str() {
        "openai-codex" | "chatgpt" => "openai".into(),
        _ => provider,
    }
}

/// Claude logs name dated snapshots (`…-20250929`); the catalog usually has the
/// undated model. Strip only a trailing eight-digit date.
fn undated(model: &str) -> &str {
    match model.rsplit_once('-') {
        Some((base, suffix))
            if suffix.len() == 8 && suffix.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            base
        }
        _ => model,
    }
}

// Antigravity keeps the generation ID in the log. Look up variants using the
// exact base APIs observed by cc-switch v4.0.38, with catalog rules at request time.
fn antigravity_price_aliases(model: &str) -> Vec<String> {
    let lower = model.to_ascii_lowercase();
    let mut aliases = Vec::new();
    if lower != model {
        aliases.push(lower.clone());
    }
    if !lower.starts_with("gemini-") && !lower.starts_with("claude-") {
        return aliases;
    }
    let mut base = lower.as_str();
    if let Some((undated, date)) = lower
        .len()
        .checked_sub(11)
        .and_then(|index| lower.split_at_checked(index))
        && let Some(date) = date.strip_prefix('-')
        && chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
    {
        base = undated;
        aliases.push(base.into());
    }
    if let Some(untiered) = ["-minimal", "-low", "-medium", "-high", "-xhigh"]
        .into_iter()
        .find_map(|suffix| base.strip_suffix(suffix))
    {
        base = untiered;
        aliases.push(base.into());
    }
    if base.starts_with("claude-") {
        let normalized = base.replace('.', "-");
        if normalized != base {
            aliases.push(normalized.clone());
        }
        if let Some(thinking) = normalized.strip_suffix("-thinking") {
            aliases.push(thinking.into());
        }
    } else {
        let api = match base {
            "gemini-3.1-pro" => Some("gemini-3.1-pro-preview"),
            "gemini-4-hydrogen-tiered"
            | "gemini-4-hydrogen-orchestrator"
            | "gemini-flash-worker-agy" => Some("gemini-4-hydrogen"),
            _ => {
                let flash_version = base
                    .strip_prefix("gemini-")
                    .and_then(|id| id.strip_suffix("-flash-tiered"));
                flash_version
                    .filter(|version| {
                        version.split('.').all(|part| {
                            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
                        })
                    })
                    .and_then(|_| base.strip_suffix("-tiered"))
            }
        };
        aliases.extend(api.map(str::to_owned));
    }
    aliases
}

fn default_provider(agent: &str, model: &str) -> Option<&'static str> {
    match agent {
        "claude" => Some("anthropic"),
        "codex" => Some("openai"),
        "antigravity" if model.starts_with("claude-") => Some("anthropic"),
        "antigravity" if model.starts_with("gemini-") => Some("google"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tokens(input: u64) -> Tokens {
        Tokens {
            input,
            output: 0,
            cache_read: 0,
            cache_write: 0,
        }
    }

    fn rule(value: Value) -> Rule {
        Rule::from_cost(&value).unwrap()
    }

    #[test]
    fn context_bands_replace_rates_once_the_prompt_crosses_the_threshold() {
        let rule = rule(json!({
            "text": {
                "input": {"price": 2.5, "cache": {"read": 0.25},
                    "overrides": [{"when": {"context": {"gt": 272000}}, "price": 5, "cache": {"read": 0.5}}]},
                "output": {"price": 15,
                    "overrides": [{"when": {"context": {"gt": 272000}}, "price": 22.5}]}
            }
        }));
        assert_eq!(rule.cost(&tokens(272000), 0), Some(272000.0 * 2.5 / 1e6));
        assert_eq!(rule.cost(&tokens(272001), 0), Some(272001.0 * 5.0 / 1e6));
        // Cache reads are part of the prompt size the band checks.
        let cached = Tokens {
            input: 100000,
            output: 0,
            cache_read: 200000,
            cache_write: 0,
        };
        let expected = 100000.0 * 5.0 / 1e6 + 200000.0 * 0.5 / 1e6;
        assert!((rule.cost(&cached, 0).unwrap() - expected).abs() < 1e-12);
        let inclusive = Rule::from_cost(&json!({
            "text": {"input": {
                "price": 1,
                "overrides": [{"when": {"context": {"gte": 100}}, "price": 2}]
            }}
        }))
        .unwrap();
        assert_eq!(inclusive.cost(&tokens(99), 0), Some(99.0 / 1e6));
        assert_eq!(inclusive.cost(&tokens(100), 0), Some(200.0 / 1e6));
    }

    #[test]
    fn utc_hours_days_and_dates_multiply_prices() {
        let rule = rule(json!({
            "text": {"input": {"price": 2}, "output": {"price": 10}},
            "multipliers": [
                {"when": {"utc_hours": {"gte": "14:00"}}, "factor": {"text": 0.5}},
                {"when": {"utc_days": {"in": ["mon", "tue", "wed", "thu", "fri"]}}, "factor": 0.9},
                {"when": {"date": {"gte": "2027-01-01"}}, "factor": 2}
            ]
        }));
        // 2026-07-10 is a Friday; 15:00 UTC is in the 50% window.
        let friday = chrono::DateTime::parse_from_rfc3339("2026-07-10T15:00:00Z")
            .unwrap()
            .timestamp_millis();
        let morning = chrono::DateTime::parse_from_rfc3339("2026-07-10T10:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert!((rule.cost(&tokens(1_000_000), friday).unwrap() - 0.9).abs() < 1e-12);
        assert!((rule.cost(&tokens(1_000_000), morning).unwrap() - 1.8).abs() < 1e-12);
        // Saturday has no weekday discount; 2027 also doubles.
        let saturday = chrono::DateTime::parse_from_rfc3339("2027-01-02T10:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert!((rule.cost(&tokens(1_000_000), saturday).unwrap() - 4.0).abs() < 1e-12);
    }

    #[test]
    fn unknown_request_facts_never_apply_a_multiplier() {
        let rule = rule(json!({
            "text": {"input": {"price": 2}},
            "multipliers": [{"when": {"service_tier": {"equals": "priority"}}, "factor": 2}]
        }));
        assert_eq!(rule.cost(&tokens(1_000_000), 0), Some(2.0));
    }

    #[test]
    fn malformed_price_fields_do_not_apply_to_otherwise_valid_prices() {
        let rule = rule(json!({
            "text": {"input": {"price": 2, "cache": {"write": {"5m": -4, "1h": 3}}}},
            "multipliers": [
                {"when": {"utc_hours": {"gte": "65535:00", "lt": "24:00"}}, "factor": 2},
                {"when": {"utc_hours": {"gte": "00:00"}}, "factor": -2}
            ]
        }));
        assert_eq!(rule.cost(&tokens(1_000_000), 0), Some(2.0));
        assert_eq!(rule.base.cache_write, 3.0);
    }

    #[test]
    fn context_bands_use_the_full_sum_of_native_token_counters() {
        let rule = rule(json!({"text": {"input": {"price": 1, "overrides": [
            {"when": {"context": {"gt": u64::MAX}}, "price": 2}
        ]}}}));
        let tokens = Tokens {
            input: i64::MAX as u64,
            cache_read: i64::MAX as u64,
            cache_write: i64::MAX as u64,
            output: 0,
        };
        assert_eq!(rule.cost(&tokens, 0), Some(tokens.input as f64 / 1e6 * 2.0));
    }

    fn v2_catalog() -> String {
        let mut openai = serde_json::Map::new();
        openai.insert(
            "gpt-5.4".into(),
            json!({"cost": {"text": {
                "input": {"price": 2.5, "cache": {"read": 0.25},
                    "overrides": [{"when": {"context": {"gt": 272000}}, "price": 5, "cache": {"read": 0.5}}]},
                "output": {"price": 15,
                    "overrides": [{"when": {"context": {"gt": 272000}}, "price": 22.5}]}
            }}}),
        );
        openai.insert(
            "shared".into(),
            json!({"cost": {"text": {"input": {"price": 1}, "output": {"price": 1}}}}),
        );
        for index in 0..1200 {
            openai.insert(
                format!("filler-{index}"),
                json!({"cost": {"text": {"input": {"price": 1}, "output": {"price": 2}}}}),
            );
        }
        json!({
            "openai": {"models": openai},
            "anthropic": {"models": {
                "claude-opus-4-6": {"cost": {"text": {
                    "input": {"price": 5, "cache": {"read": 0.5, "write": {"5m": 6.25, "1h": 10}}},
                    "output": {"price": 25}
                }}},
                "shared": {"cost": {"text": {"input": {"price": 4}, "output": {"price": 4}}}}
            }},
            "alibaba-cn": {"models": {
                "off-peak": {"cost": {
                    "text": {"input": {"price": 2}, "output": {"price": 8}},
                    "multipliers": [{"when": {"utc_hours": {"gte": "14:00"}}, "factor": {"text": 0.5}}]
                }}
            }}
        })
        .to_string()
    }

    fn open_catalog(dir: &std::path::Path) -> crate::Usage {
        crate::Usage::open(&dir.join("usage.db")).unwrap()
    }

    #[test]
    fn sync_stores_rules_and_failure_keeps_the_previous_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let mut usage = open_catalog(dir.path());
        let catalog = v2_catalog();
        let mut calls = 0;
        let statuses = usage
            .sync_prices(1_000, false, |_| {
                calls += 1;
                Ok(catalog.clone())
            })
            .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].last_success, Some(1_000));
        assert!(statuses[0].count >= 1200);
        let loaded = Catalog::load(&usage.conn).unwrap();
        assert_eq!(
            loaded.cost("codex", "gpt-5.4", None, &tokens(10_000), 0),
            Some(0.025)
        );
        assert_eq!(
            loaded.cost("codex", "gpt-5.4", None, &tokens(300_000), 0),
            Some(1.5)
        );
        assert_eq!(
            loaded.cost("claude", "claude-opus-4-6", None, &tokens(1_000_000), 0),
            Some(5.0)
        );
        // A cache write uses the short TTL when the log does not say which one.
        let written = Tokens {
            input: 0,
            output: 0,
            cache_read: 0,
            cache_write: 1_000_000,
        };
        assert_eq!(
            loaded.cost("claude", "claude-opus-4-6", None, &written, 0),
            Some(6.25)
        );

        // Not yet six hours: no request.
        usage
            .sync_prices(2_000, false, |_| {
                calls += 1;
                Err("unexpected".into())
            })
            .unwrap();
        assert_eq!(calls, 1);
        // Forced failure records the error but keeps the last success and data.
        let failed = usage
            .sync_prices(3_000, true, |_| {
                calls += 1;
                Err("offline".into())
            })
            .unwrap();
        assert_eq!(calls, 2);
        assert!(
            failed[0]
                .error
                .as_deref()
                .is_some_and(|error| error.contains("offline"))
        );
        assert_eq!(failed[0].last_success, Some(1_000));
        assert_eq!(
            Catalog::load(&usage.conn)
                .unwrap()
                .cost("codex", "gpt-5.4", None, &tokens(10_000), 0),
            Some(0.025)
        );
    }

    #[test]
    fn provider_falls_back_from_log_to_agent_to_a_unique_model() {
        let dir = tempfile::tempdir().unwrap();
        let mut usage = open_catalog(dir.path());
        usage
            .sync_prices(1_000, true, |_| Ok(v2_catalog()))
            .unwrap();
        let catalog = Catalog::load(&usage.conn).unwrap();
        // Pi has no default provider; a model only one provider serves is priced.
        assert_eq!(
            catalog.cost("pi", "filler-1", Some("made-up"), &tokens(1_000_000), 0),
            Some(1.0)
        );
        // Two providers serve `shared`, so without a provider it is ambiguous.
        assert_eq!(
            catalog.cost("pi", "shared", None, &tokens(1_000_000), 0),
            None
        );
        assert_eq!(
            catalog.cost("pi", "shared", Some("anthropic"), &tokens(1_000_000), 0),
            Some(4.0)
        );
        assert_eq!(
            catalog.cost("pi", "shared", Some("openai"), &tokens(1_000_000), 0),
            Some(1.0)
        );
        // Dated Claude snapshots fall back to the undated catalog entry.
        assert_eq!(
            catalog.cost(
                "claude",
                "claude-opus-4-6-20260206",
                None,
                &tokens(1_000_000),
                0
            ),
            Some(5.0)
        );
    }

    #[test]
    fn antigravity_prices_verified_aliases_with_catalog_rules_and_exact_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let usage = open_catalog(dir.path());
        for (provider, model, input, output, read) in [
            ("google", "gemini-4-carbon", 2.0, 10.0, 0.1),
            ("google", "gemini-4-argon", 2.0, 10.0, 0.1),
            ("google", "gemini-4-neon", 3.0, 15.0, 0.15),
            ("google", "gemini-4-hydrogen", 1.5, 7.5, 0.075),
            ("google", "gemini-3.8-flash", 0.75, 3.75, 0.075),
            ("google", "gemini-3.7-flash", 0.75, 3.75, 0.075),
            ("google", "gemini-3.1-flash-lite", 0.25, 1.50, 0.025),
            ("google", "gemini-3.1-pro-preview", 2.0, 12.0, 0.2),
            ("anthropic", "claude-opus-4-6", 5.0, 25.0, 0.5),
            ("a-catalog-reseller", "gemini-4-hydrogen", 9.0, 9.0, 9.0),
        ] {
            let rule = rule(json!({"text": {
                "input": {"price": input, "cache": {"read": read}},
                "output": {"price": output}
            }}));
            usage
                .conn
                .execute(
                    "INSERT INTO model_prices VALUES (?1,?2,?3)",
                    params![provider, model, serde_json::to_string(&rule).unwrap()],
                )
                .unwrap();
        }
        let tokens = Tokens {
            input: 1_000_000,
            output: 100_000,
            cache_read: 1_000_000,
            cache_write: 0,
        };
        let catalog = Catalog::load(&usage.conn).unwrap();
        for (model, expected) in [
            ("gemini-4-carbon-high", 3.1),
            ("gemini-4-argon-medium", 3.1),
            ("gemini-4-neon", 4.65),
            ("gemini-4-hydrogen-orchestrator", 2.325),
            ("gemini-4-hydrogen-tiered-high", 2.325),
            ("gemini-flash-worker-agy", 2.325),
            ("gemini-3.8-flash-high", 1.2),
            ("gemini-3.8-flash-tiered-high", 1.2),
            ("gemini-3.8-flash-tiered-2026-09-02", 1.2),
            ("gemini-3.7-flash-tiered", 1.2),
            ("gemini-3.1-pro-high", 3.4),
            ("claude-opus-4.6-thinking", 8.0),
        ] {
            let actual = catalog
                .cost("antigravity", model, None, &tokens, 0)
                .unwrap();
            assert!(
                (actual - expected).abs() < 1e-12,
                "{model}: {actual} != {expected}"
            );
        }
        for unknown in [
            "unknown",
            "orchestrator-neon",
            "gemini-pro-orchestrator-g1-pro",
            "gemini-3.1-flash-tiered",
            "gemini-3.8-flash-tiered-preview",
        ] {
            assert_eq!(
                catalog.cost("antigravity", unknown, None, &tokens, 0),
                None,
                "{unknown}"
            );
        }
        assert_eq!(
            catalog.cost("pi", "gemini-3.8-flash-tiered", None, &tokens, 0),
            None
        );
        usage
            .save_price(&Price {
                model: "gemini-4-hydrogen-tiered-high".into(),
                input: 8.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            })
            .unwrap();
        let catalog = Catalog::load(&usage.conn).unwrap();
        assert_eq!(
            catalog.cost(
                "antigravity",
                "gemini-4-hydrogen-tiered-high",
                None,
                &tokens,
                0
            ),
            Some(8.0)
        );
    }

    #[test]
    fn manual_override_beats_fetched_rules() {
        let dir = tempfile::tempdir().unwrap();
        let mut usage = open_catalog(dir.path());
        usage
            .sync_prices(1_000, true, |_| Ok(v2_catalog()))
            .unwrap();
        usage
            .save_price(&Price {
                model: "gpt-5.4".into(),
                input: 9.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            })
            .unwrap();
        let catalog = Catalog::load(&usage.conn).unwrap();
        assert_eq!(
            catalog.cost("codex", "gpt-5.4", None, &tokens(1_000_000), 0),
            Some(9.0)
        );
    }

    #[test]
    fn real_catalog_parses_when_provided() {
        let Ok(path) = std::env::var("CABIN_TEST_MODELS_PRICES") else {
            return;
        };
        let rules = parse_models(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(rules.len() > 1000);
        let find = |provider: &str, model: &str| {
            rules
                .iter()
                .find(|((p, m), _)| p == provider && m == model)
                .map(|(_, rule)| rule)
        };
        assert_eq!(
            find("anthropic", "claude-opus-4-6").unwrap().base().input,
            5.0
        );
        let gpt = find("openai", "gpt-5.4").unwrap();
        assert_eq!(gpt.cost(&tokens(10000), 0), Some(0.025));
        assert_eq!(gpt.cost(&tokens(300000), 0), Some(1.5));
        let flash = find("alibaba-cn", "deepseek-v4.1-flash").unwrap();
        let off_peak = chrono::DateTime::parse_from_rfc3339("2026-07-10T15:00:00Z")
            .unwrap()
            .timestamp_millis();
        let peak = chrono::DateTime::parse_from_rfc3339("2026-07-10T10:00:00Z")
            .unwrap()
            .timestamp_millis();
        assert!(
            flash.cost(&tokens(1_000_000), off_peak).unwrap()
                < flash.cost(&tokens(1_000_000), peak).unwrap()
        );
    }
}
