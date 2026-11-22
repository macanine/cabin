use std::sync::Mutex;

use super::session::Chrome;
use super::*;

const BASELINE: &[u8] = b"{\"chrome\":{\"window_size\":\"1600,872\"},\"keep\":true,\"navigator\":{\"languages\":[\"en-US\"]}}\n";
const SWAY: &str = "# existing settings\noutput HEADLESS-1 scale 1\n";

struct Live {
    output: Output,
    chrome: bool,
    wayland: bool,
    restarts: Vec<bool>,
    resize_checks: Vec<(u32, u32)>,
    viewer_failures: usize,
    chrome_failures: usize,
}

struct Test {
    _temp: tempfile::TempDir,
    files: Files,
    live: Arc<Mutex<Live>>,
    ops: OperationManager,
}

impl Test {
    fn new() -> Result<Self> {
        let temp = tempfile::tempdir()?;
        let files = Files {
            state: temp.path().join("state"),
            sway: temp.path().join("sway/config"),
            browser: temp.path().join("browser"),
        };
        fsx::ensure_dir(&files.state)?;
        fsx::atomic_write(&files.sway, SWAY.as_bytes(), None)?;
        fsx::atomic_write(&files.browser.join("config.json"), BASELINE, None)?;
        let old_inject = injection(&profile(BASELINE, false)?)?;
        fsx::atomic_write(&files.browser.join("inject.js"), &old_inject, None)?;
        let live = Arc::new(Mutex::new(Live {
            output: Output {
                name: "HEADLESS-1".into(),
                width: 1600,
                height: 900,
                scale: 1.0,
            },
            chrome: true,
            wayland: false,
            restarts: Vec::new(),
            resize_checks: Vec::new(),
            viewer_failures: 0,
            chrome_failures: 0,
        }));
        Ok(Self {
            _temp: temp,
            files,
            live,
            ops: OperationManager::new(Arc::new(Store::in_memory()?)),
        })
    }

    fn request(&self, request: Request) -> Result<Value> {
        self.files.change(self.live.clone(), &self.ops, request)
    }

    fn apply(&self, width: u32, height: u32, zoom: u32) -> Result<Value> {
        self.request(Request::Apply {
            width,
            height,
            scale: 2.0,
            zoom,
        })
    }

    fn read(&self, kind: FileKind) -> Vec<u8> {
        self.files.read(kind).unwrap().unwrap()
    }
    fn write(&self, kind: FileKind, bytes: &[u8]) -> Result<()> {
        self.files.write(self.live.as_ref(), kind, Some(bytes))
    }
    fn config(&self) -> Value {
        profile(&self.read(FileKind::Config), false).unwrap()
    }

    fn environment() -> Request {
        serde_json::from_value(json!({"action":"sync_environment", "environment":{
            "timezone":"Asia/Tokyo", "locale":"ja-JP", "language":"ja-JP", "languages":["ja-JP","ja"], "chrome_language":"ja-JP"
        }})).unwrap()
    }
}

impl Session for Mutex<Live> {
    fn output(&self) -> Result<Output> {
        Ok(self.lock().unwrap().output.clone())
    }
    fn set_output(&self, _: &Output, target: &Output) -> Result<()> {
        self.lock().unwrap().output = target.clone();
        Ok(())
    }
    fn verify_viewer(&self, output: &Output) -> Result<()> {
        let mut live = self.lock().unwrap();
        live.resize_checks.push((output.width, output.height));
        if live.viewer_failures > 0 {
            live.viewer_failures -= 1;
            return Err(Error::Unavailable("VNC failed to resize".into()));
        }
        Ok(())
    }
    fn chrome(&self) -> Chrome {
        let live = self.lock().unwrap();
        let mut chrome = Chrome::default();
        chrome.pid = live.chrome.then_some(1);
        chrome.ready = live.chrome;
        chrome.wayland = live.wayland;
        chrome
    }
    fn restart_chrome(&self, wayland: bool) -> Result<()> {
        let mut live = self.lock().unwrap();
        live.restarts.push(wayland);
        if live.chrome_failures > 0 {
            live.chrome_failures -= 1;
            return Err(Error::Unavailable("Chrome did not restart".into()));
        }
        live.wayland = wayland;
        Ok(())
    }
    fn install(&self, path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
        fsx::atomic_write(path, bytes, Some(mode))
    }
    fn remove(&self, path: &Path) -> Result<()> {
        fsx::restore(path, None)
    }
}

#[test]
fn reuses_wayland_and_preserves_restore_baseline_across_live_resizes() -> Result<()> {
    let test = Test::new()?;
    let first = test.apply(1920, 1080, 100)?;
    assert_eq!(first["logicalWidth"], 960);
    assert_eq!(first["logicalHeight"], 540);
    assert_eq!(first["chromeWayland"], true);
    assert_eq!(test.config()["chrome"]["window_size"], "960,512");
    assert_eq!(test.apply(2560, 1440, 150)?["zoom"], 150);
    assert_eq!(test.apply(2560, 1440, 175)?["zoom"], 175);
    assert_eq!(test.config()["chrome"]["window_size"], "1280,692");
    assert_eq!(test.live.lock().unwrap().restarts, [true]);
    assert_eq!(test.request(Request::Restore)?["enabled"], false);
    assert_eq!(test.read(FileKind::Config), BASELINE);
    assert_eq!(test.read(FileKind::Sway), SWAY.as_bytes());
    assert!(!test.files.pending().exists());
    assert!(Saved::load(&test.files.state)?.is_none());
    let live = test.live.lock().unwrap();
    assert_eq!(live.restarts, [true, false]);
    assert_eq!(live.output.width, 1600);
    assert!(live.resize_checks.contains(&(2560, 1440)));
    Ok(())
}

#[test]
fn failures_roll_back_live_output_and_all_affected_files() -> Result<()> {
    let test = Test::new()?;
    test.write(FileKind::Controller, b"old controller")?;
    test.write(FileKind::Inject, b"old injection")?;
    test.live.lock().unwrap().viewer_failures = 1;
    let error = test.apply(1920, 1080, 100).unwrap_err().to_string();
    assert!(error.contains("VNC failed to resize"));
    assert_eq!(test.read(FileKind::Config), BASELINE);
    assert_eq!(test.read(FileKind::Sway), SWAY.as_bytes());
    assert_eq!(test.read(FileKind::Controller), b"old controller");
    assert_eq!(test.read(FileKind::Inject), b"old injection");
    assert_eq!(test.live.lock().unwrap().output.width, 1600);
    assert!(!test.files.pending().exists());
    assert_eq!(test.ops.recent(1)?[0].state, OperationState::RolledBack);

    test.apply(1920, 1080, 100)?;
    let state = test.read(FileKind::State);
    let config = test.read(FileKind::Config);
    test.live.lock().unwrap().viewer_failures = 1;
    assert!(test.apply(2560, 1440, 150).is_err());
    assert_eq!(test.read(FileKind::State), state);
    assert_eq!(test.read(FileKind::Config), config);
    let live = test.live.lock().unwrap();
    assert_eq!(live.output.width, 1920);
    assert!(live.wayland);
    Ok(())
}

#[test]
fn recovers_legacy_pending_journal_and_refuses_other_mutations_until_recovery() -> Result<()> {
    let test = Test::new()?;
    let before = test.live.output()?;
    let backup = test.files.state.join("transaction-old");
    fs::create_dir(&backup)?;
    fs::write(backup.join("sway"), SWAY)?;
    fs::write(backup.join("config"), BASELINE)?;
    fs::write(
        test.files.pending(),
        json_bytes(&json!({"backup":"transaction-old", "output": before, "transport":"spice"}))?,
    )?;
    test.write(FileKind::Sway, b"partial change")?;
    test.write(
        FileKind::Config,
        br#"{"chrome":{"window_size":"1,1"},"native_display":{"enabled":true}}"#,
    )?;
    test.live.lock().unwrap().output.width = 1920;
    test.live.lock().unwrap().wayland = true;
    assert_eq!(
        test.files.status(test.live.as_ref())?["recoveryRequired"],
        true
    );
    assert!(
        test.request(Test::environment())
            .unwrap_err()
            .to_string()
            .contains("recovery first")
    );
    assert!(test.apply(1920, 1080, 100).is_err());
    assert_eq!(test.request(Request::Restore)?["recoveryRequired"], false);
    assert_eq!(test.read(FileKind::Config), BASELINE);
    assert_eq!(test.read(FileKind::Sway), SWAY.as_bytes());
    assert_eq!(test.live.output()?, before);
    assert_eq!(test.live.lock().unwrap().restarts, [false]);
    assert!(!backup.exists());
    Ok(())
}

#[test]
fn recovers_controller_snapshots_from_old_pending_journals() -> Result<()> {
    for previous in [Some(b"previous controller".as_slice()), None] {
        let test = Test::new()?;
        let backup = test.files.state.join("transaction-controller");
        fs::create_dir(&backup)?;
        if let Some(bytes) = previous {
            fs::write(backup.join("controller"), bytes)?;
        }
        test.write(FileKind::Controller, b"partially installed controller")?;
        fs::write(
            test.files.pending(),
            json_bytes(&json!({
                "backup":"transaction-controller",
                "files":["controller"],
                "restart_chrome": false
            }))?,
        )?;
        assert_eq!(test.request(Request::Restore)?["recoveryRequired"], false);
        assert_eq!(test.files.read(FileKind::Controller)?.as_deref(), previous);
        assert_eq!(test.live.lock().unwrap().restarts, [false]);
        assert!(!backup.exists());
    }
    Ok(())
}

#[test]
fn display_changes_do_not_install_or_replace_legacy_controller() -> Result<()> {
    for existing in [Some(b"old controller".as_slice()), None] {
        let test = Test::new()?;
        if let Some(bytes) = existing {
            test.write(FileKind::Controller, bytes)?;
        }
        test.apply(1920, 1080, 100)?;
        test.request(Test::environment())?;
        test.request(Request::Restore)?;
        assert_eq!(test.files.read(FileKind::Controller)?.as_deref(), existing);
    }
    Ok(())
}

#[test]
fn restores_v2_without_managed_fields_using_sway_and_keeps_external_edits() -> Result<()> {
    let test = Test::new()?;
    test.apply(1920, 1080, 100)?;
    let mut old: Value = serde_json::from_slice(&test.read(FileKind::State))?;
    old.as_object_mut().unwrap().shift_remove("managed_display");
    test.write(FileKind::State, &json_bytes(&old)?)?;
    let mut config = test.config();
    config["chrome"]["lang"] = json!("ja-JP");
    config["custom"] = json!({"added": true});
    test.write(FileKind::Config, &json_bytes(&config)?)?;
    test.apply(2560, 1440, 150)?;
    assert_eq!(test.config()["chrome"]["lang"], "ja-JP");
    assert_eq!(test.config()["custom"]["added"], true);
    assert_eq!(test.live.lock().unwrap().restarts, [true, true]);
    test.request(Request::Restore)?;
    assert_eq!(test.config()["chrome"]["window_size"], "1600,872");
    assert_eq!(test.config()["chrome"]["lang"], "ja-JP");
    assert_eq!(test.config()["custom"]["added"], true);
    Ok(())
}

#[test]
fn validates_display_fields_independently_of_unrelated_json() -> Result<()> {
    for original in [
        json!({"chrome": {"lang":"en-US"}}),
        json!({"chrome":{"window_size":null}, "native_display":null}),
        json!({"chrome":{"window_size":"1600,872"}, "native_display":{"enabled":false,"note":"original"}}),
    ] {
        let test = Test::new()?;
        test.write(FileKind::Config, &json_bytes(&original)?)?;
        test.apply(1920, 1080, 100)?;
        let mut current = test.config();
        current["keep"] = json!(3);
        current["native_display"]["enabled"] = json!(1);
        let conflicting = json_bytes(&current)?;
        test.write(FileKind::Config, &conflicting)?;
        assert!(
            test.request(Request::Restore)
                .unwrap_err()
                .to_string()
                .contains("settings changed")
        );
        assert!(test.apply(2560, 1440, 150).is_err());
        assert_eq!(test.read(FileKind::Config), conflicting);
        current["native_display"]["enabled"] = json!(true);
        test.write(FileKind::Config, &json_bytes(&current)?)?;
        test.request(Request::Restore)?;
        let mut expected = original;
        expected["keep"] = json!(3);
        assert_eq!(test.config(), expected);
    }
    Ok(())
}

#[test]
fn migrates_unversioned_restore_state_and_refuses_an_ambiguous_edit() -> Result<()> {
    let test = Test::new()?;
    let before = test.live.output()?;
    test.apply(1920, 1080, 100)?;
    let managed = test.read(FileKind::Config);
    test.write(
        FileKind::State,
        &json_bytes(&json!({"baseline":before,"zoom":100,"transport":"vnc"}))?,
    )?;
    fs::write(test.files.state.join("original-config"), BASELINE)?;
    let mut config = test.config();
    config["chrome"]["lang"] = json!("de-DE");
    test.write(FileKind::Config, &json_bytes(&config)?)?;
    test.write(FileKind::Sway, SWAY.as_bytes())?;
    assert!(test.request(Request::Restore).is_err());
    fs::write(test.files.state.join("managed-config"), managed)?;
    test.apply(2560, 1440, 150)?;
    let state: Value = serde_json::from_slice(&test.read(FileKind::State))?;
    assert_eq!(state["version"], 2);
    assert!(!test.files.state.join("original-config").exists());
    test.request(Request::Restore)?;
    assert_eq!(test.config()["chrome"]["window_size"], "1600,872");
    assert_eq!(test.config()["chrome"]["lang"], "de-DE");
    Ok(())
}

#[test]
fn environment_sync_is_transactional_and_survives_display_restore() -> Result<()> {
    let test = Test::new()?;
    test.apply(1920, 1080, 100)?;
    let old_state = test.read(FileKind::State);
    let old_config = test.read(FileKind::Config);
    let old_inject = test.read(FileKind::Inject);
    test.live.lock().unwrap().chrome_failures = 1;
    assert!(
        test.request(Test::environment())
            .unwrap_err()
            .to_string()
            .contains("Chrome did not restart")
    );
    assert_eq!(test.read(FileKind::Config), old_config);
    assert_eq!(test.read(FileKind::State), old_state);
    assert_eq!(test.read(FileKind::Inject), old_inject);
    assert_eq!(test.request(Test::environment())?["changed"], true);
    assert_eq!(test.request(Test::environment())?["changed"], false);
    assert_eq!(test.config()["chrome"]["window_size"], "960,512");
    test.request(Request::Restore)?;
    assert_eq!(test.config()["chrome"]["window_size"], "1600,872");
    assert_eq!(test.config()["timezone"], "Asia/Tokyo");
    assert_eq!(test.config()["locale"], "ja-JP");
    assert_eq!(
        test.config()["navigator"]["languages"],
        json!(["ja-JP", "ja"])
    );
    assert_eq!(test.config()["chrome"]["lang"], "ja-JP");
    assert!(!test.files.pending().exists());
    Ok(())
}

#[test]
fn a_stopped_browser_is_not_opened_by_environment_sync() -> Result<()> {
    let test = Test::new()?;
    test.live.lock().unwrap().chrome = false;
    assert_eq!(test.request(Test::environment())?["changed"], true);
    assert!(test.live.lock().unwrap().restarts.is_empty());
    assert!(test.live.lock().unwrap().resize_checks.is_empty());
    Ok(())
}
