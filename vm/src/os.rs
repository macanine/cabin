//! The guest operating systems the installer offers (Ubuntu LTS releases),
//! and what differs between them inside the VM.
//!
//! Every release publishes dated build folders, each with a `SHA256SUMS`
//! list. An install resolves the newest complete build and records that
//! exact URL and checksum in the machine's configuration, so the image never
//! changes under an installed machine. The moving `release/` folder is never
//! recorded.

use std::io::Read;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::Result;
use crate::download::{Checksum, HttpOptions, http_error, request};
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
        let arch = crate::arch::GuestArch::current().cloud_image_suffix();
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
    /// The checksum list comes from the official host when it is reachable,
    /// otherwise from a mirror.
    pub fn resolve(&self) -> Result<ResolvedImage> {
        self.resolve_from(&image_candidates(self.builds), fetch_text)
    }

    fn resolve_from(
        &self,
        sources: &[String],
        fetch: impl Fn(&str) -> Result<String>,
    ) -> Result<ResolvedImage> {
        let mut failures = Vec::new();
        let mut checked = std::collections::HashSet::new();
        for base in sources {
            let listing = match fetch(base) {
                Ok(listing) => listing,
                Err(error) => {
                    failures.push(error.to_string());
                    continue;
                }
            };
            let builds = dated_builds(&listing, self.build_dir);
            if builds.is_empty() {
                failures.push(format!("no {} builds are listed at {base}", self.name));
            }
            for build in builds.into_iter().rev().take(4) {
                if !checked.insert(build.clone()) {
                    continue;
                }
                // Even if only a mirror's index was reachable, ask the official
                // host for the checksum of this exact build first.
                for source in sources {
                    let folder = format!("{source}{build}/");
                    let url = format!("{folder}SHA256SUMS");
                    match fetch(&url) {
                        Ok(sums) => {
                            if let Some((file, checksum)) = self.find_image(&sums) {
                                return Ok(ResolvedImage {
                                    url: format!("{folder}{file}"),
                                    checksum,
                                    build,
                                });
                            }
                            failures.push(format!("no valid image checksum in {url}"));
                        }
                        Err(error) => failures.push(error.to_string()),
                    }
                }
            }
        }
        message(format!(
            "could not find a {} image: {}",
            self.name,
            failures.join(" | ")
        ))
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
            if parts.next().is_some() || !image.is_match(file) {
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
        crate::arch::GuestArch::current().apt_mirrors()
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
    let Ok(href) = Regex::new(&format!(r#"href="(?:[^"]*/)?({pattern})/""#)) else {
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

/// Where Ubuntu publishes cloud images, then mirrors that serve the same
/// `minimal/releases/<codename>/<build>/` layout (verified: USTC; Tsinghua
/// only carries `daily/`, so it is not listed).
const OFFICIAL_IMAGES: &str = "https://cloud-images.ubuntu.com/";
const IMAGE_MIRRORS: &[&str] = &["https://mirrors.ustc.edu.cn/ubuntu-cloud-images/"];

/// `url` on the official host and on each mirror, official first. A URL that
/// is on none of them is returned alone.
pub fn image_candidates(url: &str) -> Vec<String> {
    let all = std::iter::once(OFFICIAL_IMAGES).chain(IMAGE_MIRRORS.iter().copied());
    let Some(rest) = all.clone().find_map(|prefix| url.strip_prefix(prefix)) else {
        return vec![url.to_string()];
    };
    all.map(|prefix| format!("{prefix}{rest}")).collect()
}

/// Order candidates fastest first. Unreachable ones go last in their original
/// order; probes run in parallel threads.
pub fn order_by_latency<F>(candidates: Vec<String>, probe: F) -> Vec<String>
where
    F: Fn(&str) -> Option<Duration> + Sync,
{
    let timings: Vec<Option<Duration>> = std::thread::scope(|scope| {
        let handles: Vec<_> = candidates
            .iter()
            .map(|url| scope.spawn(|| probe(url)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().ok().flatten())
            .collect()
    });
    let mut ranked: Vec<(usize, Option<Duration>, String)> = candidates
        .into_iter()
        .zip(timings)
        .enumerate()
        .map(|(index, (url, time))| (index, time, url))
        .collect();
    ranked.sort_by_key(|(index, time, _)| (time.is_none(), *time, *index));
    ranked.into_iter().map(|(_, _, url)| url).collect()
}

/// Time to the first response of a HEAD request (about 3 seconds at most).
pub fn probe_latency(url: &str) -> Option<Duration> {
    let timeout = Duration::from_secs(3);
    let start = Instant::now();
    request(
        "HEAD",
        url,
        None,
        &HttpOptions {
            read_timeout: timeout,
            timeout: Some(timeout),
        },
    )
    .ok()
    .filter(|response| response.status() == 200 && !crate::download::is_html(response))
    .map(|_| start.elapsed())
}

fn fetch_text(url: &str) -> Result<String> {
    const MAX_TEXT: u64 = 2 * 1024 * 1024;
    let response = request(
        "GET",
        url,
        None,
        &HttpOptions {
            read_timeout: Duration::from_secs(8),
            timeout: Some(Duration::from_secs(12)),
        },
    )
    .map_err(|error| {
        let detail = match error {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            error => http_error(&error),
        };
        crate::Error::Message(format!("could not download {url}: {detail}"))
    })?;
    let mut text = String::new();
    response
        .into_reader()
        .take(MAX_TEXT + 1)
        .read_to_string(&mut text)
        .map_err(|error| crate::Error::Message(format!("could not download {url}: {error}")))?;
    if text.len() as u64 > MAX_TEXT {
        return message(format!("could not download {url}: metadata exceeds 2 MiB"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_cover_the_official_host_and_the_mirror() {
        let url = "https://cloud-images.ubuntu.com/minimal/releases/noble/release-1/a.img";
        let list = image_candidates(url);
        assert_eq!(list[0], url);
        assert_eq!(
            list[1],
            "https://mirrors.ustc.edu.cn/ubuntu-cloud-images/minimal/releases/noble/release-1/a.img"
        );
        // Rebasing from a mirror URL gives the same set.
        assert_eq!(image_candidates(&list[1]), list);
        assert_eq!(
            image_candidates("http://x/y"),
            vec!["http://x/y".to_string()]
        );
    }

    #[test]
    fn latency_orders_fastest_first_and_unreachable_last() {
        let urls: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        let ordered = order_by_latency(urls, |url| match url {
            "a" => None,
            "b" => Some(Duration::from_millis(50)),
            "c" => Some(Duration::from_millis(10)),
            _ => None,
        });
        assert_eq!(ordered, ["c", "b", "a", "d"]);
    }

    #[test]
    fn listings_with_absolute_links_are_parsed() {
        let html = r#"<a href="/x/minimal/noble/release-20240423/">a</a><a href="release-20260101.1/">b</a>"#;
        assert_eq!(
            dated_builds(html, RELEASES[0].build_dir),
            ["release-20240423", "release-20260101.1"]
        );
    }

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
        let (arch, digest) = match crate::arch::GuestArch::current() {
            crate::arch::GuestArch::Amd64 => ("amd64", "3".repeat(64)),
            crate::arch::GuestArch::Arm64 => ("arm64", "ab".repeat(32)),
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
    fn official_checksum_wins_even_when_only_the_mirror_index_loads() {
        let os = &RELEASES[0];
        let sources = image_candidates(os.index_url());
        let build = "release-20261123";
        let name = format!(
            "ubuntu-24.04-minimal-cloudimg-{}.img",
            crate::arch::GuestArch::current().cloud_image_suffix()
        );
        let resolved = os
            .resolve_from(&sources, |url| {
                if url == sources[0] {
                    return message("official index unavailable");
                }
                if url == sources[1] {
                    return Ok(format!(r#"<a href="{build}/">build</a>"#));
                }
                let digest = if url.starts_with(&sources[0]) {
                    "1"
                } else {
                    "2"
                }
                .repeat(64);
                Ok(format!("{digest}  {name}"))
            })
            .unwrap();
        assert_eq!(resolved.url, format!("{}{build}/{name}", sources[0]));
        assert_eq!(resolved.checksum, Checksum::Sha256("1".repeat(64)));
    }

    #[test]
    fn tries_the_mirror_for_a_missing_checksum_of_the_same_dated_build() {
        let os = &RELEASES[0];
        let sources = image_candidates(os.index_url());
        let latest = "release-20261123";
        let older = "release-20261120";
        let name = format!(
            "ubuntu-24.04-minimal-cloudimg-{}.img",
            crate::arch::GuestArch::current().cloud_image_suffix()
        );
        let digest = "3".repeat(64);
        for mirror_has_latest in [false, true] {
            let resolved = os
                .resolve_from(&sources, |url| {
                    if url == sources[0] {
                        return Ok(format!(
                            r#"<a href="{older}/">old</a><a href="{latest}/">new</a>"#
                        ));
                    }
                    if url.contains(latest) && url.starts_with(&sources[0]) {
                        return message("HTTP 404 for official SHA256SUMS");
                    }
                    if url.contains(latest) && !mirror_has_latest {
                        return message("HTTP 404 for mirror SHA256SUMS");
                    }
                    if url.ends_with("SHA256SUMS") {
                        return Ok(format!("malformed  {name}\n{digest}  {name}"));
                    }
                    panic!("unexpected lookup {url}");
                })
                .unwrap();
            let (source, build) = if mirror_has_latest {
                (&sources[1], latest)
            } else {
                (&sources[0], older)
            };
            assert_eq!(resolved.url, format!("{source}{build}/{name}"));
            assert_eq!(resolved.checksum, Checksum::Sha256(digest.clone()));
        }
    }

    #[test]
    fn reports_unavailable_and_malformed_checksums_without_using_an_unpinned_image() {
        let os = &RELEASES[0];
        let sources = image_candidates(os.index_url());
        let error = os
            .resolve_from(&sources, |url| {
                if sources.iter().any(|base| base == url) {
                    Ok(r#"<a href="release-20261123/">build</a>"#.to_owned())
                } else if url.starts_with(&sources[0]) {
                    message("HTTP 503 for official SHA256SUMS")
                } else {
                    Ok("malformed checksum line".into())
                }
            })
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("HTTP 503") && error.contains("no valid image checksum"),
            "{error}"
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
