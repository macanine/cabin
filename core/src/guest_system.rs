//! Small guest-side system queries used during browser setup.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

use crate::{Error, Result};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Environment {
    pub locale: String,
    pub timezone: String,
}

fn locale_from_file(contents: &str) -> String {
    contents
        .lines()
        .find_map(|line| line.strip_prefix("LANG="))
        .map(|value| value.trim().trim_matches(['\'', '"']).to_string())
        .unwrap_or_else(|| "C.UTF-8".into())
}

pub fn environment() -> Result<Environment> {
    let contents = match std::fs::read_to_string("/etc/default/locale") {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(Error::io("read guest locale", error)),
    };
    let output = Command::new("timedatectl")
        .args(["show", "-p", "Timezone", "--value"])
        .output()
        .map_err(|error| Error::io("read guest timezone", error))?;
    if !output.status.success() {
        return Err(Error::Command {
            message: "Could not read the guest timezone".into(),
            output: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let timezone = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if timezone.is_empty() {
        return Err(Error::Unavailable(
            "The guest did not report its timezone".into(),
        ));
    }
    Ok(Environment {
        locale: locale_from_file(&contents),
        timezone,
    })
}

pub fn validate_json(path: &Path) -> Result<()> {
    let file = std::fs::File::open(path).map_err(|error| Error::path("read", path, error))?;
    let _: serde_json::Value = serde_json::from_reader(file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_reads_the_first_lang_and_preserves_the_default() {
        assert_eq!(
            locale_from_file("LC_ALL=C\nLANG=\"en_US.UTF-8\"\n"),
            "en_US.UTF-8"
        );
        assert_eq!(
            locale_from_file("LANG='zh_CN.UTF-8'\nLANG=en_US\n"),
            "zh_CN.UTF-8"
        );
        assert_eq!(locale_from_file("LC_ALL=C\n"), "C.UTF-8");
    }

    #[test]
    fn json_validation_reports_invalid_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.json");
        std::fs::write(&file, "{\"enabled\":true}").unwrap();
        validate_json(&file).unwrap();
        std::fs::write(&file, "{broken").unwrap();
        assert!(validate_json(&file).is_err());
        assert!(validate_json(&dir.path().join("missing")).is_err());
    }
}
