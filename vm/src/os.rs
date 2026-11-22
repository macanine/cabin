//! The guest operating systems the installer offers (Ubuntu LTS releases),
//! and what differs between them inside the VM.
//!
//! Every release publishes dated build folders, each with a `SHA256SUMS`
//! list. An install resolves the newest complete build and records that
//! exact URL and checksum in the machine's configuration, so the image never
//! changes under an installed machine. The moving `release/` folder is never
//! recorded.

use regex::Regex;

use crate::Result;
use crate::download::Checksum;
use crate::util::message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Os {
    /// Stable identifier stored in the machine's configuration.
    pub id: &'static str,
    pub name: &'static str,
    pub codename: &'static str,
    /// A development release: images are daily builds and change often.
    pub testing: bool,
    /// Folder that lists the dated builds.
    builds: &'static str,
    /// A dated build folder in that listing (captured without the slash).
    build_dir: &'static str,
    /// The release version, e.g. `24.04`.
    pub version: &'static str,
}

/// The default for new machines.
pub const DEFAULT: &str = "ubuntu-24.04";

pub const RELEASES: [Os; 2] = [
    Os {
        id: "ubuntu-24.04",
        name: "Ubuntu 24.04 LTS",
        codename: "noble",
        testing: false,
        builds: "https://cloud-images.ubuntu.com/minimal/releases/noble/",
        build_dir: r"release-\d{8}(?:\.\d+)?",
        version: "24.04",
    },
    Os {
        id: "ubuntu-26.04",
        name: "Ubuntu 26.04 LTS",
        codename: "resolute",
        testing: false,
        builds: "https://cloud-images.ubuntu.com/minimal/releases/resolute/",
        build_dir: r"release-\d{8}(?:\.\d+)?",
        version: "26.04",
    },
];

impl Os {
    /// The cloud image for the host's guest architecture (`aarch64` on the
    /// macOS product, the native architecture elsewhere).
    pub fn image(&self) -> String {
        let arch = if crate::platform::guest_arch() == "x86_64" {
            "amd64"
        } else {
            "arm64"
        };
        format!(
            r"ubuntu-{}-minimal-cloudimg-{}\.img",
            self.version.replace('.', r"\."),
            arch
        )
    }
}

pub fn find(id: &str) -> Option<&'static Os> {
    RELEASES.iter().find(|os| os.id == id)
}

pub fn get(id: &str) -> Result<&'static Os> {
    find(id).ok_or_else(|| {
        crate::Error::Message(if id.starts_with("debian-") {
            // Machines installed by older builds: say what to do, not just "unknown".
            format!(
                "this machine runs {id}, and Debian is no longer supported. Remove the machine (Settings → Machine) and install it again with Ubuntu"
            )
        } else {
            format!(
                "unknown operating system {id:?}; choose one of: {}",
                RELEASES.map(|os| os.id).join(", ")
            )
        })
    })
}

/// One build of an image, pinned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImage {
    pub url: String,
    pub checksum: Checksum,
    /// The dated build folder, e.g. `release-20261105`.
    pub build: String,
}

impl Os {
    /// The newest build that has this image in its checksum list. Builds
    /// still being published (no list yet, or no arm64 entry) are skipped.
    pub fn resolve(&self) -> Result<ResolvedImage> {
        let listing = fetch_text(self.builds)?;
        let builds = dated_builds(&listing, self.build_dir);
        if builds.is_empty() {
            return message(format!(
                "no {} builds are listed at {}",
                self.name, self.builds
            ));
        }
        let mut last_error = None;
        for build in builds.iter().rev().take(4) {
            let folder = format!("{}{build}/", self.builds);
            let sums = match fetch_text(&format!("{folder}SHA256SUMS")) {
                Ok(text) => text,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            if let Some((file, checksum)) = self.find_image(&sums) {
                return Ok(ResolvedImage {
                    url: format!("{folder}{file}"),
                    checksum,
                    build: build.clone(),
                });
            }
        }
        match last_error {
            Some(error) => message(format!("could not find a {} image: {error}", self.name)),
            None => message(format!(
                "no recent {} build lists an image for this architecture",
                self.name
            )),
        }
    }

    /// The folder listing this release's builds.
    pub fn index_url(&self) -> &'static str {
        self.builds
    }

    /// The image's file name and checksum in a `SHA256SUMS` list
    /// (`<hex> *<file>` or `<hex>  <file>`).
    fn find_image(&self, sums: &str) -> Option<(String, Checksum)> {
        let image = Regex::new(&format!("^{}$", self.image())).ok()?;
        sums.lines().find_map(|line| {
            let mut parts = line.split_whitespace();
            let hex = parts.next()?.to_ascii_lowercase();
            let file = parts.next()?.trim_start_matches('*');
            if !image.is_match(file) {
                return None;
            }
            let checksum = Checksum::Sha256(hex);
            checksum
                .is_well_formed()
                .then(|| (file.to_string(), checksum))
        })
    }

    /// The kernel metapackage that carries every driver the VM needs (9p
    /// shared folders, DRM), and the minimal image's slimmer metapackages it
    /// replaces (`linux-virtual` lacks both).
    pub fn kernel(&self) -> KernelSwitch {
        KernelSwitch {
            install: "linux-image-generic",
            replaces: &["linux-virtual", "linux-image-virtual"],
        }
    }

    /// Kernel modules beyond the base package, if the release splits them out.
    pub fn extra_modules_package(&self) -> Option<&'static str> {
        (self.id == "ubuntu-24.04").then_some("linux-modules-extra-$(uname -r)")
    }

    /// The archive the image uses, then mirrors commonly faster in Asia.
    pub fn mirrors(&self) -> &'static [&'static str] {
        if crate::platform::guest_arch() == "x86_64" {
            &[
                "http://archive.ubuntu.com/ubuntu",
                "https://ftp.kaist.ac.kr/ubuntu",
                "https://mirrors.nju.edu.cn/ubuntu",
            ]
        } else {
            &[
                "http://ports.ubuntu.com/ubuntu-ports",
                "https://ftp.kaist.ac.kr/ubuntu-ports",
                "https://mirrors.nju.edu.cn/ubuntu-ports",
            ]
        }
    }

    /// The suite whose package index a mirror test downloads.
    pub fn mirror_index_suite(&self) -> String {
        format!("{}-updates", self.codename)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelSwitch {
    pub install: &'static str,
    pub replaces: &'static [&'static str],
}

impl KernelSwitch {
    /// Run once by cloud-init: keep the full kernel and drop the cloud
    /// image's metapackages only after the replacement is installed (dpkg
    /// refuses removal while something still depends on them).
    pub fn script(&self) -> String {
        format!(
            r#"set -eu
status() {{ dpkg-query -W -f='${{db:Status-Status}}' "$1" 2>/dev/null || true; }}
if [ "$(status {install})" != installed ]; then
    echo '{install} is not installed; keeping the existing kernel metapackage' >&2
    exit 1
fi
apt-mark manual {install}
for package in {replaces}; do
    if [ "$(status "$package")" = installed ]; then
        dpkg --purge "$package"
    fi
done
"#,
            install = self.install,
            replaces = self.replaces.join(" "),
        )
    }
}

/// Dated build folders in an index page, oldest first.
fn dated_builds(listing: &str, pattern: &str) -> Vec<String> {
    let Ok(href) = Regex::new(&format!(r#"href="({pattern})/""#)) else {
        return Vec::new();
    };
    let mut builds: Vec<String> = href
        .captures_iter(listing)
        .map(|c| c[1].to_string())
        .collect();
    builds.sort();
    builds.dedup();
    builds
}

fn fetch_text(url: &str) -> Result<String> {
    match crate::download::agent().get(url).call() {
        Ok(response) => response
            .into_string()
            .map_err(|error| crate::Error::Message(format!("could not read {url}: {error}"))),
        Err(ureq::Error::Status(code, _)) => message(format!("HTTP {code} for {url}")),
        Err(error) => message(format!("could not reach {url}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_the_default_exists() {
        let mut ids: Vec<_> = RELEASES.iter().map(|os| os.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), RELEASES.len());
        assert!(find(DEFAULT).is_some());
        assert!(get("windows-11").is_err());
    }

    #[test]
    fn a_former_debian_machine_is_told_to_reinstall() {
        let error = get("debian-13").unwrap_err().to_string();
        assert!(error.contains("no longer supported"), "{error}");
        assert!(error.contains("Ubuntu"), "{error}");
    }

    #[test]
    fn picks_dated_builds_never_the_moving_folders() {
        let ubuntu = r#"<a href="release-20261027/">x</a><a href="release-20261105/">x</a>
<a href="release-20261103/">x</a><a href="release/">release/</a><a href="current/">c</a>"#;
        assert_eq!(
            dated_builds(ubuntu, RELEASES[0].build_dir),
            ["release-20261027", "release-20261103", "release-20261105"]
        );
    }

    #[test]
    fn finds_the_guest_image_in_the_checksum_list() {
        let ubuntu = find("ubuntu-24.04").unwrap();
        let sums = format!(
            "{a} *ubuntu-24.04-minimal-cloudimg-arm64.manifest\n{b} *ubuntu-24.04-minimal-cloudimg-arm64.img\n{c} *ubuntu-24.04-minimal-cloudimg-amd64.img\n",
            a = "1".repeat(64),
            b = "AB".repeat(32),
            c = "3".repeat(64)
        );
        let (file, checksum) = ubuntu.find_image(&sums).unwrap();
        let (arch, digest) = if crate::platform::guest_arch() == "x86_64" {
            ("amd64", "3".repeat(64))
        } else {
            ("arm64", "ab".repeat(32))
        };
        assert_eq!(file, format!("ubuntu-24.04-minimal-cloudimg-{arch}.img"));
        assert_eq!(checksum, Checksum::Sha256(digest));
        // A short or malformed digest is not trusted.
        assert!(
            ubuntu
                .find_image("abc  ubuntu-24.04-minimal-cloudimg-arm64.img\n")
                .is_none()
        );
    }

    #[test]
    fn guest_facts() {
        let noble = find("ubuntu-24.04").unwrap();
        let resolute = find("ubuntu-26.04").unwrap();
        assert_eq!(noble.mirror_index_suite(), "noble-updates");
        assert!(noble.extra_modules_package().is_some());
        assert!(resolute.extra_modules_package().is_none());
        let script = noble.kernel().script();
        assert!(script.contains("apt-mark manual linux-image-generic"));
        assert!(script.contains("for package in linux-virtual linux-image-virtual; do"));
        assert!(script.contains("${db:Status-Status}"));
    }

    #[test]
    #[ignore = "requires outbound network"]
    fn resolves_every_release_upstream() {
        for os in RELEASES {
            let image = os.resolve().unwrap_or_else(|e| panic!("{}: {e}", os.id));
            assert!(image.url.contains(&image.build), "{image:?}");
            assert!(!image.url.contains("/release/"));
            assert!(image.checksum.is_well_formed());
        }
    }
}
