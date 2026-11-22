//! Guest-side desktop application discovery, launch and Sway focus inspection.
//!
//! The caller supplies the guest user's home. `list_apps` and `launch_app` use
//! the same three application directories as the original desktop commands.
//! No application command or desktop-entry value is passed to a shell.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::system::{CommandSpec, HostSystem, System};
use crate::{Error, Result};

/// The presentation fields of a visible guest `.desktop` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopApp {
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
}

/// Application directories in increasing precedence order.
pub fn application_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        home.join(".local/share/applications"),
    ]
}

#[derive(Default)]
struct Entry {
    fields: HashMap<String, String>,
}

impl Entry {
    fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    fn is_true(&self, key: &str) -> bool {
        self.get(key)
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    }
}

// RawConfigParser's relevant behavior: case-insensitive keys, last duplicate
// wins, comments and unrelated sections ignored. Malformed files are skipped.
fn read_entry(path: &Path) -> Option<Entry> {
    let text = fs::read_to_string(path).ok()?;
    let mut entry = Entry::default();
    let mut section = false;
    let mut saw_section = false;
    let mut found = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            let end = line.find(']')?;
            let name = &line[1..end];
            section = name == "Desktop Entry";
            saw_section = true;
            found |= section;
            continue;
        }
        if !saw_section {
            return None;
        }
        let pos = line.find(['=', ':'])?;
        if section {
            entry.fields.insert(
                line[..pos].trim().to_ascii_lowercase(),
                line[pos + 1..].trim().to_owned(),
            );
        }
    }
    found.then_some(entry)
}

fn stem(id: &str) -> &str {
    id.strip_suffix(".desktop").unwrap_or(id)
}

/// Lists visible entries from the standard guest directories.
pub fn list_apps(home: &Path) -> Vec<DesktopApp> {
    list_apps_in(&application_dirs(home))
}

/// Lists entries from directories in increasing precedence order.
///
/// Like the former guest script, an ineligible higher-priority entry is
/// skipped rather than removing an eligible lower-priority entry.
pub fn list_apps_in(dirs: &[PathBuf]) -> Vec<DesktopApp> {
    let mut apps: Vec<DesktopApp> = Vec::new();
    let mut positions = HashMap::<String, usize>::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        paths.sort();
        for path in paths {
            let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !id.ends_with(".desktop") {
                continue;
            }
            let Some(entry) = read_entry(&path) else {
                continue;
            };
            if entry.get("type").unwrap_or("Application") != "Application"
                || entry.get("exec").is_none_or(str::is_empty)
                || entry.is_true("nodisplay")
                || entry.is_true("hidden")
                || (entry.is_true("terminal") && id != "foot.desktop")
            {
                continue;
            }
            let app = DesktopApp {
                id: id.to_owned(),
                name: entry.get("name").unwrap_or_else(|| stem(id)).to_owned(),
                comment: entry
                    .get("comment")
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
            };
            if let Some(&index) = positions.get(id) {
                apps[index] = app;
            } else {
                positions.insert(id.to_owned(), apps.len());
                apps.push(app);
            }
        }
    }
    apps.sort_by_cached_key(|app| app.name.to_lowercase());
    apps
}

/// Whether an ID is a plain desktop filename accepted by the launcher.
pub fn valid_app_id(id: &str) -> bool {
    id.ends_with(".desktop")
        && id.len() <= 200
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

/// Prepared direct process launch. `argv[0]` is the executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLaunch {
    pub workspace: String,
    pub argv: Vec<String>,
}

/// Resolve an entry using the last directory first and prepare its launch.
pub fn prepare_launch(id: &str, dirs: &[PathBuf]) -> Result<AppLaunch> {
    if !valid_app_id(id) {
        return Err(Error::invalid("Unknown app"));
    }
    let path = dirs
        .iter()
        .rev()
        .map(|dir| dir.join(id))
        .find(|path| path.is_file())
        .ok_or_else(|| Error::invalid("Unknown app"))?;
    let entry = read_entry(&path).ok_or_else(|| Error::invalid("Invalid desktop entry"))?;
    let exec = entry
        .get("exec")
        .filter(|exec| !exec.is_empty())
        .ok_or_else(|| Error::invalid("Desktop entry has no Exec"))?;
    let name = entry.get("name").unwrap_or_else(|| stem(id));
    let workspace: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-'))
        .take(40)
        .collect();
    let workspace = if workspace.is_empty() {
        stem(id).to_owned()
    } else {
        workspace
    };
    let argv = parse_exec(exec, name, entry.get("icon"), &path)?;
    Ok(AppLaunch { workspace, argv })
}

// Split like shlex without evaluating substitutions or invoking a shell. Both
// double and single quotes are accepted for compatibility with existing files.
fn split_exec(input: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = input.chars().peekable();
    let mut quote = None;
    let mut started = false;
    while let Some(ch) = chars.next() {
        if ch == '\0' {
            return Err(Error::invalid("NUL in desktop Exec"));
        }
        match (quote, ch) {
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, '\'' | '"') => {
                quote = Some(ch);
                started = true;
            }
            (Some(q), c) if q == c => quote = None,
            (Some('\''), c) => word.push(c),
            (_, '\\') => {
                let next = chars
                    .next()
                    .ok_or_else(|| Error::invalid("Trailing escape in desktop Exec"))?;
                if next == '\0' {
                    return Err(Error::invalid("NUL in desktop Exec"));
                }
                if quote == Some('"') && !matches!(next, '"' | '\\' | '$' | '`') {
                    word.push('\\');
                }
                word.push(next);
                started = true;
            }
            (_, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(Error::invalid("Unclosed quote in desktop Exec"));
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

/// Expand desktop field codes into argv with no file or URL arguments.
/// `%i` expands to `--icon` and the icon, `%c` to the name, `%k` to the
/// desktop file path, and `%%` to `%`. File/URL and deprecated codes are
/// removed. Unknown or malformed codes are rejected.
pub fn parse_exec(exec: &str, name: &str, icon: Option<&str>, path: &Path) -> Result<Vec<String>> {
    let mut argv = Vec::new();
    for word in split_exec(exec)? {
        if word == "%i" {
            if let Some(icon) = icon.filter(|icon| !icon.is_empty()) {
                argv.push("--icon".into());
                argv.push(icon.to_owned());
            }
            continue;
        }
        let mut expanded = String::new();
        let mut chars = word.chars();
        let mut removed = false;
        while let Some(ch) = chars.next() {
            if ch != '%' {
                expanded.push(ch);
                continue;
            }
            match chars.next() {
                Some('%') => expanded.push('%'),
                Some('c') => expanded.push_str(name),
                Some('k') => expanded.push_str(&path.to_string_lossy()),
                Some('f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'v' | 'm') => removed = true,
                Some('i') => {
                    return Err(Error::invalid(
                        "Desktop Exec %i must be a separate argument",
                    ));
                }
                _ => {
                    return Err(Error::invalid(
                        "Unknown or incomplete desktop Exec field code",
                    ));
                }
            }
        }
        if !expanded.is_empty() || !removed {
            argv.push(expanded);
        }
    }
    if argv.first().is_none_or(String::is_empty) || argv.iter().any(|arg| arg.contains('\0')) {
        return Err(Error::invalid("Desktop Exec has no valid executable"));
    }
    Ok(argv)
}

/// Launch an installed application on its own Sway workspace.
///
/// Finds the guest's Sway socket under `/run/user/<uid>` and starts the app
/// directly, without exposing its Exec arguments to a shell or Sway parser.
pub fn launch_app(home: &Path, id: &str) -> Result<()> {
    let launch = prepare_launch(id, &application_dirs(home))?;
    let uid = unsafe { libc::getuid() };
    launch_in(
        &launch,
        &PathBuf::from(format!("/run/user/{uid}")),
        "swaymsg",
    )
}

fn runtime_entries(runtime: &Path, prefix: &str, suffix: &str) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(runtime)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with(prefix) && name.ends_with(suffix)
        })
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    paths
}

fn launch_in(launch: &AppLaunch, runtime: &Path, swaymsg: &str) -> Result<()> {
    let socket = runtime_entries(runtime, "sway-ipc.", ".sock")
        .into_iter()
        .next()
        .ok_or_else(|| Error::Unavailable("The desktop is not running".into()))?;
    let runtime_str = runtime.to_string_lossy().into_owned();
    let socket_str = socket.to_string_lossy().into_owned();
    let env = vec![
        ("XDG_RUNTIME_DIR".into(), runtime_str.clone()),
        ("SWAYSOCK".into(), socket_str.clone()),
    ];
    // Workspace names are restricted to characters that cannot terminate the
    // Sway command or its quotes, including in the filename fallback.
    let workspace = format!("workspace \"{}\"", launch.workspace);
    let system = HostSystem {
        path: std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into()),
    };
    let output = system.check(
        &CommandSpec::new([swaymsg, "-r", &workspace])
            .env(env)
            .timeout(Duration::from_secs(10)),
    )?;
    let replies: Vec<Value> = serde_json::from_str(&output)?;
    if replies.is_empty()
        || replies
            .iter()
            .any(|reply| reply.get("success").and_then(Value::as_bool) != Some(true))
    {
        let message = replies
            .iter()
            .find_map(|reply| reply.get("error").and_then(Value::as_str))
            .unwrap_or("The desktop rejected the workspace");
        return Err(Error::Unavailable(message.to_owned()));
    }
    let mut command = Command::new(&launch.argv[0]);
    command
        .args(&launch.argv[1..])
        .env("XDG_RUNTIME_DIR", runtime_str)
        .env("SWAYSOCK", socket_str)
        .env("XDG_SESSION_TYPE", "wayland")
        .env("XDG_CURRENT_DESKTOP", "sway")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if std::env::var_os("DISPLAY").is_none() {
        command.env("DISPLAY", ":0");
    }
    if let Some(display) = runtime_entries(runtime, "wayland-", "")
        .into_iter()
        .find(|path| !path.to_string_lossy().ends_with(".lock"))
        && let Some(name) = display.file_name()
    {
        command.env("WAYLAND_DISPLAY", name);
    }
    let mut child = command
        .spawn()
        .map_err(|error| Error::io("Could not start the app", error))?;
    // Reap children without waiting for graphical applications to close.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The output selected by the browser setup probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwayOutput {
    pub name: String,
    pub width: u64,
    pub height: u64,
    pub scale: serde_json::Number,
}

impl SwayOutput {
    /// Render the `widthxheight@scale` value expected by browser setup.
    pub fn screen(&self) -> String {
        format!("{}x{}@{}", self.width, self.height, self.scale)
    }
}

/// Read the first active output, falling back to the first output or defaults.
/// Missing mode dimensions default to zero, name to empty, and scale to one,
/// matching the former browser setup probe. Invalid JSON returns an error.
pub fn selected_output(outputs_json: &str) -> Result<SwayOutput> {
    let outputs: Vec<Value> = serde_json::from_str(outputs_json)?;
    let output = outputs
        .iter()
        .find(|output| output.get("active").and_then(Value::as_bool) == Some(true))
        .or_else(|| outputs.first());
    let mode = output.and_then(|output| output.get("current_mode"));
    Ok(SwayOutput {
        name: output
            .and_then(|output| output.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        width: mode
            .and_then(|mode| mode.get("width"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        height: mode
            .and_then(|mode| mode.get("height"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        scale: output
            .and_then(|output| output.get("scale"))
            .and_then(Value::as_number)
            .cloned()
            .unwrap_or_else(|| 1.into()),
    })
}

/// Inspect the first focused node in a `swaymsg -t get_tree` JSON response.
pub fn chrome_focused(tree_json: &str) -> Result<bool> {
    let tree: Value = serde_json::from_str(tree_json)?;
    let mut pending = vec![&tree];
    while let Some(node) = pending.pop() {
        if node.get("focused").and_then(Value::as_bool) == Some(true) {
            let app_id = node
                .get("app_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let class = node
                .get("window_properties")
                .and_then(|p| p.get("class"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            let name = node
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            return Ok(app_id.contains("chrome")
                || class.contains("chrome")
                || name.ends_with("google chrome"));
        }
        // Stack is LIFO: ordinary nodes precede floating nodes, as in Python.
        for key in ["floating_nodes", "nodes"] {
            if let Some(children) = node.get(key).and_then(Value::as_array) {
                pending.extend(children.iter().rev());
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(dir: &Path, id: &str, text: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(id), text).unwrap();
    }

    #[test]
    fn listing_preserves_precedence_filtering_and_stable_sort() {
        let temp = tempfile::tempdir().unwrap();
        let dirs: Vec<_> = ["system", "local", "user"]
            .iter()
            .map(|name| temp.path().join(name))
            .collect();
        write(
            &dirs[0],
            "same.desktop",
            "[Desktop Entry]\nName=Zebra\nExec=system\nComment=system\n",
        );
        write(
            &dirs[1],
            "same.desktop",
            "[Desktop Entry]\nName=alpha\nExec=local\nComment=local\n",
        );
        write(
            &dirs[2],
            "same.desktop",
            "[Desktop Entry]\nName=ALPHA\nExec=user\nComment=\n",
        );
        write(
            &dirs[0],
            "another.desktop",
            "[Desktop Entry]\nName=Alpha\nExec=another\n",
        );
        write(
            &dirs[0],
            "masked.desktop",
            "[Desktop Entry]\nName=Beta\nExec=base\n",
        );
        write(
            &dirs[2],
            "masked.desktop",
            "[Desktop Entry]\nHidden=true\nExec=override\n",
        );
        write(
            &dirs[0],
            "terminal.desktop",
            "[Desktop Entry]\nExec=term\nTerminal=TrUe\n",
        );
        write(
            &dirs[0],
            "foot.desktop",
            "[Desktop Entry]\nExec=foot\nTerminal=true\n",
        );
        write(
            &dirs[0],
            "hidden.desktop",
            "[Desktop Entry]\nExec=hidden\nNoDisplay=true\n",
        );
        write(
            &dirs[0],
            "other.desktop",
            "[Desktop Entry]\nType=Link\nExec=link\n",
        );
        write(&dirs[0], "broken.desktop", "bad ini\n");
        write(&dirs[0], ".dot.desktop", "[Desktop Entry]\nExec=dot\n");
        let apps = list_apps_in(&dirs);
        assert_eq!(
            apps.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
            [
                ".dot.desktop",
                "another.desktop",
                "same.desktop",
                "masked.desktop",
                "foot.desktop"
            ]
        );
        assert_eq!(apps[2].name, "ALPHA");
        assert_eq!(apps[2].comment, None);
        assert_eq!(apps[3].name, "Beta");
    }

    #[test]
    fn launcher_resolves_highest_priority_and_validates_ids() {
        let temp = tempfile::tempdir().unwrap();
        let dirs = vec![temp.path().join("system"), temp.path().join("user")];
        write(
            &dirs[0],
            "app.desktop",
            "[Desktop Entry]\nName=Old\nExec=old\n",
        );
        write(
            &dirs[1],
            "app.desktop",
            "[Desktop Entry]\nName=New ; kill [x] Ω\nExec=\"/opt/my app\" \"hello world\" %U\n",
        );
        let launch = prepare_launch("app.desktop", &dirs).unwrap();
        assert_eq!(launch.workspace, "New  kill x ");
        assert_eq!(launch.argv, ["/opt/my app", "hello world"]);
        for id in [
            "../x.desktop",
            "a b.desktop",
            "x;rm.desktop",
            ".hidden.desktop",
            "$(id).desktop",
            "x",
        ] {
            assert!(prepare_launch(id, &dirs).is_err(), "{id}");
        }
        write(
            &dirs[1],
            "empty.desktop",
            "[Desktop Entry]\nName=Ω\nExec=app\n",
        );
        assert_eq!(
            prepare_launch("empty.desktop", &dirs).unwrap().workspace,
            "empty"
        );
        write(
            &dirs[1],
            "long.desktop",
            &format!("[Desktop Entry]\nName={}\nExec=app\n", "a".repeat(50)),
        );
        assert_eq!(
            prepare_launch("long.desktop", &dirs)
                .unwrap()
                .workspace
                .len(),
            40
        );
    }

    #[test]
    fn exec_expands_field_codes_without_shell_interpretation() {
        let argv = parse_exec(r#"app "two words" 'single quoted' --literal=$(touch /tmp/no) "x; y" %f %F %u %U %d %D %n %N %v %m %i %c %k %% --name=%c --file=%f"#, "My App", Some("app-icon"), Path::new("/tmp/a b.desktop")).unwrap();
        assert_eq!(
            argv,
            [
                "app",
                "two words",
                "single quoted",
                "--literal=$(touch",
                "/tmp/no)",
                "x; y",
                "--icon",
                "app-icon",
                "My App",
                "/tmp/a b.desktop",
                "%",
                "--name=My App",
                "--file="
            ]
        );
        assert_eq!(
            parse_exec("app %i %c %%", "Title", None, Path::new("a")).unwrap(),
            ["app", "Title", "%"]
        );
        assert_eq!(
            parse_exec(r#"app "" "a\\q" escaped\ space"#, "", None, Path::new("a")).unwrap(),
            ["app", "", "a\\q", "escaped space"]
        );
        for bad in [
            "",
            "%u",
            "app %z",
            "app %",
            "app a%i",
            "app 'unclosed",
            "app \\",
            "app \0",
        ] {
            assert!(
                parse_exec(bad, "", None, Path::new("a")).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn focus_searches_nodes_then_floating_nodes_and_stops_at_first_focus() {
        let json = r#"{"nodes":[{"nodes":[],"floating_nodes":[{"focused":true,"window_properties":{"class":"GOOGLE-CHROME"}}]}]}"#;
        assert!(chrome_focused(json).unwrap());
        assert!(chrome_focused(r#"{"focused":true,"app_id":"org.Chrome.App"}"#).unwrap());
        assert!(chrome_focused(r#"{"focused":true,"name":"Page - Google Chrome"}"#).unwrap());
        assert!(!chrome_focused(r#"{"nodes":[{"focused":true,"name":"Terminal"},{"focused":true,"app_id":"chrome"}]}"#).unwrap());
        assert!(!chrome_focused(r#"{"nodes":[],"floating_nodes":[]}"#).unwrap());
        assert!(chrome_focused("{").is_err());
    }

    #[test]
    fn output_probe_uses_active_then_first_and_defaults() {
        let output = selected_output(r#"[{"name":"old","active":false},{"name":"Virtual-1","active":true,"current_mode":{"width":1920,"height":1080},"scale":1.25}]"#).unwrap();
        assert_eq!(output.name, "Virtual-1");
        assert_eq!(output.screen(), "1920x1080@1.25");
        let output = selected_output(r#"[{"name":"fallback"}]"#).unwrap();
        assert_eq!(output.name, "fallback");
        assert_eq!(output.screen(), "0x0@1");
        assert_eq!(selected_output("[]").unwrap().name, "");
        assert!(selected_output("{}").is_err());
    }
}
