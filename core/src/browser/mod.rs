//! Native guest-side Chrome controller.
//!
//! This module owns the loopback CDP connection, the Chrome process, the
//! controller lock and status file. All host commands use the same JSON status
//! format as the former Python controller so existing machines can migrate.

mod cdp;
mod process;
pub(crate) use process::chrome_log_path;
mod profile;

use std::fs;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use thiserror::Error as ThisError;

use self::cdp::{Cdp, http_json, ws_url};
use self::process::{Chrome, Status, lock, start, status, status_path};
use self::profile::Profile;

#[derive(Debug, ThisError)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Url(#[from] url::ParseError),
}

impl From<Box<ureq::Error>> for Error {
    fn from(error: Box<ureq::Error>) -> Self {
        Self::Message(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn error(message: impl Into<String>) -> Error {
    Error::Message(message.into())
}

const CDP_TIMEOUT: Duration = Duration::from_secs(8);
const EVENT_TIMEOUT: Duration = Duration::from_millis(200);

/// Dispatch `cabind browser <args>`. Returns the intended process exit
/// code; errors are printed by the binary and exit with code 1.
pub fn main(args: &[String]) -> std::result::Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let (options, rest) = parse(args)?;
    match rest[0].as_str() {
        "run" => Ok(run(&options)?),
        "verify" => verify(&options),
        "audit" => audit(&options),
        "navigate" => {
            let url = rest.get(1).ok_or_else(|| error("navigate needs a URL"))?;
            navigate(&options, url)?;
            Ok(0)
        }
        "paste" | "paste-base64" => {
            let encoded = rest
                .get(1)
                .ok_or_else(|| error("paste needs base64 text"))?;
            paste(&options, encoded)?;
            Ok(0)
        }
        "close" => close(&options),
        "status" => {
            println!("{}", serde_json::to_string(&status())?);
            Ok(0)
        }
        "log-tail" => {
            let log = cabin_diagnostics::tail(&chrome_log_path(), 8 * 1024, 0);
            print!("{log}");
            Ok(0)
        }
        "ready" => Ok(if status().get("ready") == Some(&json!(true)) {
            0
        } else {
            1
        }),
        _ => Err("usage: browser run|verify|audit|navigate|paste|close|status|ready|log-tail".into()),
    }
}

#[derive(Clone)]
struct Options {
    config: PathBuf,
    inject: PathBuf,
    explicit_inject: bool,
}

fn parse(args: &[String]) -> Result<(Options, Vec<String>)> {
    let mut options = Options {
        config: profile::default_config(),
        inject: profile::default_inject(),
        explicit_inject: false,
    };
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--config" => {
                options.config = PathBuf::from(
                    args.get(index + 1)
                        .ok_or_else(|| error("--config needs a path"))?,
                );
                index += 2;
            }
            "--inject" => {
                options.inject = PathBuf::from(
                    args.get(index + 1)
                        .ok_or_else(|| error("--inject needs a path"))?,
                );
                options.explicit_inject = true;
                index += 2;
            }
            other => {
                rest.push(other.to_owned());
                index += 1;
            }
        }
    }
    if rest.is_empty() {
        return Err(error("browser command is missing"));
    }
    Ok((options, rest))
}

fn load(options: &Options) -> Result<Profile> {
    Profile::load(&options.config, &options.inject, options.explicit_inject)
}

fn run(options: &Options) -> Result<i32> {
    let Some(_lock) = lock()? else { return Ok(75) };
    let profile = load(options)?;
    let restore = std::env::var("CABIN_CHROME_RESTORE") == Ok("1".into());
    let chrome = start(&profile, false, restore)?;
    let mut supervisor = Browser::new(profile, chrome)?;
    let mut status = Status::new(&supervisor.profile.data);
    status.update(json!({"ready": false, "stage": "starting", "pid": supervisor.chrome.child.id(), "port": supervisor.chrome.port}));
    let result = supervisor.serve(&mut status);
    status.update(json!({"ready": false, "stage": "stopped", "error": result.as_ref().err().map(ToString::to_string)}));
    result?;
    Ok(0)
}

struct Browser {
    profile: Profile,
    chrome: Chrome,
    cdp: Cdp,
    sessions: Vec<String>,
}

impl Browser {
    fn new(profile: Profile, chrome: Chrome) -> Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut address = None;
        while Instant::now() < deadline {
            if let Ok(info) = http_json(chrome.port, "/json/version") {
                address = ws_url(&info).ok().map(str::to_owned);
                if address.is_some() {
                    break;
                }
            }
            sleep(Duration::from_millis(100));
        }
        let address =
            address.ok_or_else(|| error("Chrome did not expose its DevTools endpoint"))?;
        let cdp = Cdp::connect(&address, CDP_TIMEOUT)?;
        Ok(Self {
            profile,
            chrome,
            cdp,
            sessions: Vec::new(),
        })
    }

    fn serve(&mut self, status: &mut Status) -> Result<()> {
        self.cdp.call(
            "Target.setDiscoverTargets",
            json!({"discover": true}),
            None,
            CDP_TIMEOUT,
        )?;
        self.cdp.call(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
            None,
            CDP_TIMEOUT,
        )?;
        status.update(json!({"ready": true, "stage": "ready", "profile_fingerprint": self.profile.data.get("fingerprint")}));
        while self.chrome.child.try_wait()?.is_none() {
            if let Some(event) = self.cdp.event(EVENT_TIMEOUT)? {
                self.event(event)?;
            }
        }
        Ok(())
    }

    fn event(&mut self, event: Value) -> Result<()> {
        if event.get("method").and_then(Value::as_str) != Some("Target.attachedToTarget") {
            return Ok(());
        }
        let Some(session) = event.pointer("/params/sessionId").and_then(Value::as_str) else {
            return Ok(());
        };
        let kind = event
            .pointer("/params/targetInfo/type")
            .and_then(Value::as_str);
        if kind != Some("page") || self.sessions.iter().any(|known| known == session) {
            let _ = self.cdp.call(
                "Runtime.runIfWaitingForDebugger",
                json!({}),
                Some(session),
                CDP_TIMEOUT,
            );
            return Ok(());
        }
        let session = session.to_owned();
        let configured = configure(&mut self.cdp, &self.profile, Some(&session), None);
        // A tab may close or navigate while being configured. Its next attach
        // configures it again, and it must never stall other tabs.
        let _ = configured;
        let _ = self.cdp.call(
            "Runtime.runIfWaitingForDebugger",
            json!({}),
            Some(&session),
            CDP_TIMEOUT,
        );
        self.sessions.push(session);
        Ok(())
    }
}

/// Apply the profile to a page session, or directly to a connected page. This
/// is the same sequence the managed controller uses for every new tab.
fn configure(
    cdp: &mut Cdp,
    profile: &Profile,
    session: Option<&str>,
    navigate: Option<&str>,
) -> Result<()> {
    let data = &profile.data;
    let navigator = data.get("navigator").cloned().unwrap_or_else(|| json!({}));
    let languages = navigator
        .get("languages")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    let user_agent = navigator
        .get("user_agent")
        .and_then(Value::as_str)
        .unwrap_or("");
    let platform = navigator
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("");
    cdp.call("Page.enable", json!({}), session, CDP_TIMEOUT)?;
    cdp.call(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": profile.source, "runImmediately": true}),
        session,
        CDP_TIMEOUT,
    )?;
    cdp.call(
        "Emulation.setUserAgentOverride",
        json!({
            "userAgent": user_agent,
            "acceptLanguage": languages,
            "platform": platform,
            "userAgentMetadata": profile::metadata(data),
        }),
        session,
        CDP_TIMEOUT,
    )?;
    if let Some(timezone) = data
        .get("timezone")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        cdp.call(
            "Emulation.setTimezoneOverride",
            json!({"timezoneId": timezone}),
            session,
            CDP_TIMEOUT,
        )?;
    }
    if let Some(locale) = data
        .get("locale")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        cdp.call(
            "Emulation.setLocaleOverride",
            json!({"locale": locale}),
            session,
            CDP_TIMEOUT,
        )?;
    }
    if let Some(points) = navigator
        .get("max_touch_points")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
    {
        cdp.call(
            "Emulation.setTouchEmulationEnabled",
            json!({"enabled": true, "maxTouchPoints": points}),
            session,
            CDP_TIMEOUT,
        )?;
    }
    let screen = data.get("screen").cloned().unwrap_or_else(|| json!({}));
    if data.get("emulate_screen").and_then(Value::as_bool) == Some(true)
        && !profile.native_display()
        && screen.as_object().is_some_and(|value| !value.is_empty())
    {
        let number =
            |key: &str, default: f64| screen.get(key).and_then(Value::as_f64).unwrap_or(default);
        let width = number("width", 1600.0);
        let height = number("height", 900.0);
        cdp.call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": number("device_pixel_ratio", 1.0),
                "mobile": false,
                "screenWidth": width,
                "screenHeight": height,
            }),
            session,
            CDP_TIMEOUT,
        )?;
    }
    if let Some(url) = navigate {
        // Chrome can commit a navigation while withholding the CDP response;
        // do not block the configuration sequence on that reply.
        cdp.send("Page.navigate", json!({"url": url}), session)?;
    }
    Ok(())
}

fn verify(options: &Options) -> std::result::Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let profile = load(options)?;
    let mut chrome = start(&profile, true, false)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let page = loop {
        if chrome.child.try_wait()?.is_some() {
            return Err("Chrome exited before opening a page".into());
        }
        if let Ok(pages) = http_json(chrome.port, "/json")
            && let Some(page) = pages.as_array().and_then(|pages| {
                pages
                    .iter()
                    .find(|page| page.get("type").and_then(Value::as_str) == Some("page"))
            })
        {
            break page.clone();
        }
        if Instant::now() >= deadline {
            return Err("Chrome page target is unavailable".into());
        }
        sleep(Duration::from_millis(100));
    };
    let address = ws_url(&page)?;
    let mut cdp = Cdp::connect(address, CDP_TIMEOUT)?;
    configure(
        &mut cdp,
        &profile,
        None,
        Some("data:text/html,<body>vmctl-fp-verify</body>"),
    )?;
    sleep(Duration::from_secs(1));
    let result = cdp.call(
        "Runtime.evaluate",
        json!({
            "expression": r#"JSON.stringify({
              hc: navigator.hardwareConcurrency,
              dm: navigator.deviceMemory,
              platform: navigator.platform,
              languages: navigator.languages,
              tz: Intl.DateTimeFormat().resolvedOptions().timeZone,
              dpr: devicePixelRatio,
              screen: [screen.width, screen.height, screen.colorDepth],
              webgl: (() => { try { const c = document.createElement('canvas').getContext('webgl'); if (!c) return null; const d = c.getExtension('WEBGL_debug_renderer_info'); return d ? { vendor: c.getParameter(d.UNMASKED_VENDOR_WEBGL), renderer: c.getParameter(d.UNMASKED_RENDERER_WEBGL) } : null; } catch (e) { return null; } })(),
              canvas: (() => { try { const c = document.createElement('canvas'); c.width = 32; c.height = 16; const x = c.getContext('2d'); x.fillText('vmctl', 1, 10); return c.toDataURL().slice(0, 96); } catch (e) { return null; } })()
            })"#,
            "returnByValue": true,
        }),
        None,
        CDP_TIMEOUT,
    )?;
    let text = result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let observed: Value = serde_json::from_str(text).unwrap_or_else(|_| json!({}));
    let navigator = profile
        .data
        .get("navigator")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let screen = profile
        .data
        .get("screen")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let expected = json!({
        "hc": navigator.get("hardware_concurrency"),
        "dm": navigator.get("device_memory"),
        "platform": navigator.get("platform"),
        "languages": navigator.get("languages"),
        "tz": profile.data.get("timezone"),
        "dpr": screen.get("device_pixel_ratio").and_then(Value::as_f64).unwrap_or(1.0),
        "screen": [
            screen.get("width").and_then(Value::as_i64).unwrap_or(0),
            screen.get("height").and_then(Value::as_i64).unwrap_or(0),
            screen.get("color_depth").and_then(Value::as_i64).unwrap_or(0)
        ],
        "webgl_vendor": profile.data.pointer("/webgl/vendor"),
        "webgl_renderer": profile.data.pointer("/webgl/renderer"),
    });
    let mut problems = Vec::new();
    for key in ["hc", "dm"] {
        let observed_value = observed.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        let expected_value = expected.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        if observed_value != expected_value {
            problems.push(format!("{key} {observed_value} != {expected_value}"));
        }
    }
    for key in ["platform", "languages", "tz", "screen"] {
        if observed.get(key) != expected.get(key) {
            problems.push(format!(
                "{key} {:?} != {:?}",
                observed.get(key),
                expected.get(key)
            ));
        }
    }
    let observed_dpr = observed.get("dpr").and_then(Value::as_f64).unwrap_or(0.0);
    let expected_dpr = expected.get("dpr").and_then(Value::as_f64).unwrap_or(1.0);
    if observed_dpr != expected_dpr {
        problems.push(format!("dpr {observed_dpr} != {expected_dpr}"));
    }
    let webgl = observed.get("webgl").cloned().unwrap_or_else(|| json!({}));
    for key in ["webgl_vendor", "webgl_renderer"] {
        if webgl.get(key.trim_start_matches("webgl_")) != expected.get(key) {
            problems.push(format!(
                "{key} {:?} != {:?}",
                webgl.get(key.trim_start_matches("webgl_")),
                expected.get(key)
            ));
        }
    }
    if observed
        .get("canvas")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        problems.push("canvas toDataURL returned no data".into());
    }
    let ok = problems.is_empty();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "ok": ok,
            "observed": observed,
            "expected": expected,
            "problems": problems,
        }))?
    );
    Ok(if ok { 0 } else { 2 })
}

/// Total time the live part of `audit` may take. Sleeping or frozen tabs
/// must not make the audit (and the network check waiting on it) hang.
const LIVE_BUDGET: Duration = Duration::from_secs(4);
const LIVE_PAGE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Report the installed browser settings (always, from the profile file) and,
/// separately, what the live Chrome says. The live part is an observation
/// only: Chrome not running, no web page, or pages asleep are states, not
/// errors, and comparing values is left to the Mac.
///
/// Output: `{enabled, installed, live, observed, liveDetail}` where `live` is
/// `observed` | `notRunning` | `idle` | `unavailable`.
fn audit(options: &Options) -> std::result::Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let profile = load(options)?;
    let navigator = profile
        .data
        .get("navigator")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let languages = navigator
        .get("languages")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let installed = json!({
        "timezone": profile.data.get("timezone"),
        "language": navigator.get("language").or_else(|| languages.get(0)),
        "languages": languages,
        "platform": navigator.get("platform"),
    });
    let enabled = profile.data.get("enabled") == Some(&json!(true));
    let mut result = json!({
        "enabled": enabled,
        "installed": installed,
        "live": "notRunning",
        "observed": Value::Null,
        "liveDetail": Value::Null,
    });
    if enabled {
        let (live, observed, detail) = match profile.port() {
            Ok(port) => observe_live(port),
            Err(error) => ("unavailable", None, Some(error.to_string())),
        };
        result["live"] = json!(live);
        result["observed"] = observed.unwrap_or(Value::Null);
        result["liveDetail"] = detail.map(Value::from).unwrap_or(Value::Null);
    }
    println!("{}", serde_json::to_string(&result)?);
    Ok(0)
}

fn observe_live(port: u16) -> (&'static str, Option<Value>, Option<String>) {
    let deadline = Instant::now() + LIVE_BUDGET;
    let pages = match http_json(port, "/json") {
        Ok(pages) => pages,
        Err(_) => return ("notRunning", None, Some("Chrome is not running".into())),
    };
    let pages: Vec<Value> = pages
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|page| page.get("type").and_then(Value::as_str) == Some("page"))
        .filter(|page| {
            let url = page.get("url").and_then(Value::as_str).unwrap_or("");
            !url.is_empty()
                && ![
                    "chrome://",
                    "chrome-error://",
                    "chrome-untrusted://",
                    "devtools://",
                    "about:",
                ]
                .iter()
                .any(|prefix| url.starts_with(prefix))
        })
        .collect();
    if pages.is_empty() {
        return ("idle", None, Some("No web page is open".into()));
    }
    let expression = "JSON.stringify({timezone:Intl.DateTimeFormat().resolvedOptions().timeZone,language:navigator.language,languages:Array.from(navigator.languages),platform:navigator.platform})";
    let mut observations = Vec::new();
    let mut asleep = 0;
    for page in pages.iter().take(3) {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let timeout = left.min(LIVE_PAGE_TIMEOUT);
        let observed = (|| -> Result<Value> {
            let mut cdp = Cdp::connect(ws_url(page)?, timeout)?;
            let value = cdp.call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true}),
                None,
                timeout,
            )?;
            let raw = value
                .pointer("/result/value")
                .and_then(Value::as_str)
                .ok_or_else(|| error("Chrome returned no environment values"))?;
            Ok(serde_json::from_str::<Value>(raw)?)
        })();
        match observed {
            Ok(mut value) => {
                if value.get("languages").and_then(Value::as_array).is_none() {
                    value["languages"] = json!([]);
                }
                observations.push(value);
            }
            // A discarded or frozen tab does not answer: it is asleep.
            Err(_) => asleep += 1,
        }
    }
    let Some(first) = observations.first().cloned() else {
        let detail = if asleep > 0 {
            "Chrome's pages are asleep"
        } else {
            "No page answered in time"
        };
        return ("idle", None, Some(detail.into()));
    };
    let signature = |value: &Value| serde_json::to_string(value).unwrap_or_default();
    if observations
        .iter()
        .any(|value| signature(value) != signature(&first))
    {
        return (
            "unavailable",
            None,
            Some("Chrome's pages report different environment values".into()),
        );
    }
    ("observed", Some(first), None)
}

fn navigate(_options: &Options, url: &str) -> Result<()> {
    let port = Profile::load(
        &profile::default_config(),
        &profile::default_inject(),
        false,
    )?
    .port()?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let page = loop {
        let pages = http_json(port, "/json/list")?;
        if let Some(page) = pages.as_array().and_then(|pages| {
            pages
                .iter()
                .find(|page| page.get("type").and_then(Value::as_str) == Some("page"))
        }) {
            break page.clone();
        }
        if Instant::now() >= deadline {
            return Err(error("Chrome page target is unavailable"));
        }
        sleep(Duration::from_millis(250));
    };
    let target = page
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| error("Chrome page target has no id"))?
        .to_owned();
    let initial_url = page
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let address = ws_url(&page)?;
    let mut cdp = Cdp::connect(address, CDP_TIMEOUT)?;
    // Chrome can commit a successful navigation without delivering the
    // matching CDP response. Fall back to the live target list.
    let mut last = "Page.navigate did not return".to_string();
    for attempt in 0..6 {
        let send_error = cdp
            .send("Page.navigate", json!({"url": url}), None)
            .err()
            .map(|error| error.to_string());
        let waited = Instant::now();
        while waited.elapsed() < Duration::from_secs(3) {
            let pages = http_json(port, "/json/list").unwrap_or_else(|_| json!([]));
            let current = pages.as_array().and_then(|pages| {
                pages
                    .iter()
                    .find(|page| page.get("id").and_then(Value::as_str) == Some(target.as_str()))
            });
            if let Some(current) = current {
                let current_url = current.get("url").and_then(Value::as_str).unwrap_or("");
                if current_url == url
                    || (current_url != initial_url && !current_url.starts_with("chrome-error://"))
                {
                    return Ok(());
                }
            }
            sleep(Duration::from_millis(100));
        }
        last = send_error.unwrap_or_else(|| "Page.navigate timed out".to_string());
        if attempt < 5 {
            sleep(Duration::from_millis((200u64 << attempt).min(1000)));
        }
    }
    Err(error(format!("Page.navigate: {last}")))
}

fn paste(_options: &Options, encoded: &str) -> Result<()> {
    use base64::Engine;
    let text = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| error(format!("Invalid paste text: {e}")))?;
    let text =
        String::from_utf8(text).map_err(|e| error(format!("Paste text is not UTF-8: {e}")))?;
    let port = Profile::load(
        &profile::default_config(),
        &profile::default_inject(),
        false,
    )?
    .port()?;
    let mut last = "Chrome paste target is unavailable".to_string();
    for attempt in 0..5 {
        match paste_once(port, &text) {
            Ok(()) => return Ok(()),
            Err(error) => last = error.to_string(),
        }
        if attempt < 4 {
            sleep(Duration::from_millis(250 * (attempt as u64 + 1)));
        }
    }
    Err(error(last))
}

fn focused_page(port: u16) -> Result<Value> {
    let pages = http_json(port, "/json")?;
    let pages: Vec<Value> = pages
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|page| page.get("type").and_then(Value::as_str) == Some("page"))
        .collect();
    let mut visible = None;
    for page in &pages {
        let Some(address) = page.get("webSocketDebuggerUrl").and_then(Value::as_str) else {
            continue;
        };
        let Ok(mut cdp) = Cdp::connect(address, Duration::from_secs(2)) else {
            continue;
        };
        let Ok(result) = cdp.call(
            "Runtime.evaluate",
            json!({"expression": "[document.hasFocus(), document.visibilityState]", "returnByValue": true}),
            None,
            Duration::from_secs(2),
        ) else {
            continue;
        };
        let values = result
            .pointer("/result/value")
            .cloned()
            .unwrap_or_else(|| json!([]));
        if values.get(0).and_then(Value::as_bool) == Some(true) {
            return Ok(page.clone());
        }
        if values.get(1).and_then(Value::as_str) == Some("visible") && visible.is_none() {
            visible = Some(page.clone());
        }
    }
    visible
        .or_else(|| pages.first().cloned())
        .ok_or_else(|| error("No Chrome page is available"))
}

fn paste_once(port: u16, text: &str) -> Result<()> {
    let page = focused_page(port)?;
    let address = ws_url(&page)?;
    let mut cdp = Cdp::connect(address, CDP_TIMEOUT)?;
    let editable = cdp.call(
        "Runtime.evaluate",
        json!({
            "expression": r#"(() => {
                let element = document.activeElement;
                while (element?.shadowRoot?.activeElement) {
                    element = element.shadowRoot.activeElement;
                }
                return !!element && (
                    element.isContentEditable ||
                    /^(INPUT|TEXTAREA|IFRAME)$/.test(element.tagName)
                );
            })()"#,
            "returnByValue": true,
        }),
        None,
        CDP_TIMEOUT,
    )?;
    if editable.pointer("/result/value").and_then(Value::as_bool) != Some(true) {
        return Err(error("focused Chrome element is not editable"));
    }
    cdp.call("Input.insertText", json!({"text": text}), None, CDP_TIMEOUT)?;
    Ok(())
}

fn close(_options: &Options) -> std::result::Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let path = status_path();
    let status = status();
    if let Some(pid) = status.get("pid").and_then(Value::as_u64) {
        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    }
    let _ = fs::remove_file(path);
    println!("{}", serde_json::to_string(&json!({"closed": true}))?);
    Ok(0)
}
