//! Node groups and isolated measurements for the Mac catalog.

use std::collections::{HashMap, HashSet};
use std::io::{Read, copy, sink};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{ProxyCatalog, ProxyGroup, ProxyNode, ProxyTestKind, ProxyTestResult};
use crate::store::Store;
use crate::util::{clean_name, new_id, now_millis, sha256_text};

use super::subscription::Node;
use super::{EmbeddedProxy, catalog, find_node, free_port, probe, render_listener};

const GROUPS: &str = "proxy_groups";
const REVISION_SALT: &str = "proxy_revision_salt";
const DOWNLOAD_BYTES: u32 = 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(8);
const DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down?bytes=1048576";

pub(super) fn revision_salt(store: &Store) -> Result<String> {
    if let Some(salt) = store.setting(REVISION_SALT)? {
        return Ok(salt);
    }
    let salt = new_id("salt");
    store.set_setting(REVISION_SALT, &salt)?;
    Ok(salt)
}

pub(super) fn config_revision(salt: &str, node: &Node) -> Result<String> {
    // Keep the credential-bearing configuration and its salt on the Mac.
    let input = serde_json::to_string(&(salt, &node.name, &node.protocol, &node.config))?;
    Ok(sha256_text(&input))
}

#[derive(Serialize, Deserialize)]
struct StoredGroup {
    id: String,
    name: String,
    members: Vec<StoredMember>,
}

#[derive(Serialize, Deserialize)]
struct StoredMember {
    source_id: String,
    name: String,
    protocol: String,
}

fn group_records(store: &Store) -> Result<Vec<StoredGroup>> {
    Ok(store.setting(GROUPS)?.unwrap_or_default())
}

pub(super) fn groups(store: &Store, nodes: &[ProxyNode]) -> Result<Vec<ProxyGroup>> {
    // The subscription parser enforces unique names within a source. Membership
    // follows that name across refreshes, where a positional route ID can change.
    let by_name: HashMap<_, _> = nodes
        .iter()
        .map(|node| {
            (
                (
                    node.source_id.as_str(),
                    node.name.as_str(),
                    node.protocol.as_str(),
                ),
                &node.id,
            )
        })
        .collect();
    Ok(group_records(store)?
        .into_iter()
        .map(|group| {
            let node_ids = group
                .members
                .iter()
                .filter_map(|member| {
                    by_name
                        .get(&(
                            member.source_id.as_str(),
                            member.name.as_str(),
                            member.protocol.as_str(),
                        ))
                        .map(|id| (*id).clone())
                })
                .collect();
            ProxyGroup {
                id: group.id,
                name: group.name,
                node_ids,
            }
        })
        .collect())
}

pub fn save_group(
    store: &Store,
    id: Option<&str>,
    name: &str,
    node_ids: &[String],
) -> Result<ProxyCatalog> {
    let name = clean_name(name, "group")?;
    let catalog = catalog(store)?;
    let by_id: HashMap<_, _> = catalog
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    let mut seen = HashSet::new();
    let members = node_ids
        .iter()
        .filter(|id| seen.insert(id.as_str()))
        .map(|id| {
            let node = by_id
                .get(id.as_str())
                .ok_or_else(|| Error::NotFound("proxy node".into()))?;
            Ok(StoredMember {
                source_id: node.source_id.clone(),
                name: node.name.clone(),
                protocol: node.protocol.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut records = group_records(store)?;
    if let Some(id) = id {
        let existing = records
            .iter_mut()
            .find(|group| group.id == id)
            .ok_or_else(|| Error::NotFound("proxy group".into()))?;
        existing.name = name;
        existing.members = members;
    } else {
        records.push(StoredGroup {
            id: new_id("group"),
            name,
            members,
        });
    }
    records.sort_by_cached_key(|group| group.name.to_lowercase());
    store.set_setting(GROUPS, &records)?;
    let mut catalog = catalog;
    catalog.groups = groups(store, &catalog.nodes)?;
    Ok(catalog)
}

pub fn remove_group(store: &Store, id: &str) -> Result<ProxyCatalog> {
    let mut records = group_records(store)?;
    records.retain(|group| group.id != id);
    store.set_setting(GROUPS, &records)?;
    catalog(store)
}

/// Capture one matching configuration under the store lock before any I/O.
/// Test results carry that revision even if the source is refreshed meanwhile.
pub fn test_node(store: &Store, id: &str, expected_revision: &str) -> Result<Node> {
    let node = find_node(store, id)?;
    if config_revision(&revision_salt(store)?, &node)? != expected_revision {
        return Err(Error::Conflict(
            "This proxy node changed. Select its new configuration and try again.".into(),
        ));
    }
    Ok(node)
}

pub fn measure(
    node: &Node,
    id: &str,
    revision: &str,
    kind: ProxyTestKind,
    latency_target: &str,
    interface: Option<&str>,
) -> ProxyTestResult {
    let mut result = ProxyTestResult {
        node_id: id.to_string(),
        config_revision: revision.to_string(),
        kind,
        tested_at: now_millis(),
        ok: false,
        latency_ms: None,
        bytes: None,
        elapsed_ms: None,
        detail: "The temporary node proxy could not start".into(),
    };
    if node.protocol == "direct" {
        result.detail = "Choose a node that uses an upstream proxy".into();
        return result;
    }
    let proxy = EmbeddedProxy::default();
    let url = free_port().and_then(|port| {
        let config = render_listener(std::slice::from_ref(&node.config), port, false, interface)?;
        proxy.start_on_interface(config, port, interface)?;
        Ok(format!("http://127.0.0.1:{port}"))
    });
    let Ok(url) = url else { return result };

    match kind {
        ProxyTestKind::Latency => {
            let sample = probe(latency_target, Some(&url));
            result.ok = sample.ok;
            result.latency_ms = sample.latency_ms;
            result.detail = sample.detail;
        }
        ProxyTestKind::Download => {
            let http_proxy = match ureq::Proxy::new(&url) {
                Ok(proxy) => proxy,
                Err(error) => {
                    result.detail = format!("Could not configure the temporary proxy: {error}");
                    return result;
                }
            };
            let agent = ureq::AgentBuilder::new()
                .resolver(crate::ipv4::resolve)
                .try_proxy_from_env(false)
                .proxy(http_proxy)
                .timeout_connect(DOWNLOAD_TIMEOUT)
                .timeout(DOWNLOAD_TIMEOUT)
                .redirects(0)
                .build();
            match download_sample(&agent, DOWNLOAD_URL) {
                Ok((bytes, elapsed_ms)) => {
                    result.ok = true;
                    result.bytes = Some(bytes);
                    result.elapsed_ms = Some(elapsed_ms);
                    result.detail =
                        format!("Downloaded {bytes} bytes from Cloudflare in {elapsed_ms} ms");
                }
                Err(detail) => result.detail = detail,
            }
        }
    }
    result.tested_at = now_millis();
    result
}

fn download_sample(agent: &ureq::Agent, url: &str) -> std::result::Result<(u32, u32), String> {
    let started = Instant::now();
    let response = agent
        .get(url)
        .set("Accept-Encoding", "identity")
        .call()
        .map_err(|error| match error {
            ureq::Error::Status(status, _) => format!("Download server returned HTTP {status}"),
            _ => "Could not connect to the download server through this node".into(),
        })?;
    if response.status() != 200 {
        return Err(format!(
            "Download server returned HTTP {}",
            response.status()
        ));
    }
    let mut reader = response.into_reader().take(u64::from(DOWNLOAD_BYTES));
    let received = copy(&mut reader, &mut sink())
        .map_err(|_| "The download did not finish within the sample timeout".to_string())?;
    if received != u64::from(DOWNLOAD_BYTES) {
        return Err(format!("The download ended after {received} bytes"));
    }
    let elapsed = started.elapsed().as_millis().clamp(1, u32::MAX as u128) as u32;
    Ok((DOWNLOAD_BYTES, elapsed))
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    use super::*;

    fn update_source(store: &Store, id: &str, hosts: &[(&str, &str)]) {
        let mut source = store
            .proxy_sources()
            .unwrap()
            .into_iter()
            .find(|source| source.id == id)
            .unwrap();
        let text = hosts
            .iter()
            .fold("proxies:\n".to_owned(), |mut text, (name, host)| {
                text.push_str(&format!(
                    "  - {{name: {name}, type: socks5, server: {host}, port: 1080}}\n"
                ));
                text
            });
        source.nodes_json =
            serde_json::to_string(&super::super::subscription::parse_nodes(&text).unwrap())
                .unwrap();
        store.upsert_proxy_source(&source).unwrap();
    }

    #[test]
    fn group_persists_and_follows_its_node_across_subscription_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("network.sqlite3");
        let store = Store::open(&path).unwrap();
        let initial = super::super::import(&store, "Provider", None, Some(
            "proxies:\n  - {name: Sydney, type: socks5, server: node.example, port: 1080}\n  - {name: Other, type: socks5, server: other.example, port: 1080}\n",
        ), None).unwrap();
        let node = &initial.nodes[0];
        let saved = save_group(&store, None, "Work", &[node.id.clone(), node.id.clone()]).unwrap();
        assert_eq!(saved.groups[0].node_ids, std::slice::from_ref(&node.id));
        drop(store);

        let store = Store::open(&path).unwrap();
        assert_eq!(catalog(&store).unwrap(), saved);
        update_source(
            &store,
            &node.source_id,
            &[("Other", "other.example"), ("Sydney", "new.example")],
        );
        let updated = catalog(&store).unwrap();
        assert_eq!(updated.groups[0].node_ids, [updated.nodes[1].id.clone()]);
        assert_ne!(updated.nodes[1].config_revision, node.config_revision);
        assert!(matches!(
            test_node(&store, &node.id, &node.config_revision),
            Err(Error::Conflict(_))
        ));
        assert!(
            remove_group(&store, &saved.groups[0].id)
                .unwrap()
                .groups
                .is_empty()
        );
    }

    fn download_from_loopback(content_length: u32) -> std::result::Result<(u32, u32), String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            assert_ne!(stream.read(&mut request).unwrap(), 0);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let _ = stream.write_all(&vec![0; content_length as usize]);
        });
        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .timeout(Duration::from_secs(2))
            .build();
        let result = download_sample(&agent, &format!("http://{address}/"));
        server.join().unwrap();
        result
    }

    #[test]
    fn download_caps_bytes_and_requires_a_complete_sample() {
        let (bytes, elapsed) = download_from_loopback(DOWNLOAD_BYTES + 1024).unwrap();
        assert_eq!(bytes, DOWNLOAD_BYTES);
        assert!(elapsed > 0);
        assert_eq!(
            download_from_loopback(16).unwrap_err(),
            "The download ended after 16 bytes"
        );
    }
}
