//! Helpers shared by the global network tests.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::PathBuf;

use agent_core::model::{NetworkCheckCategory, NetworkCheckItem, NetworkCheckReport, NetworkCheckStatus, NetworkRoute};
use agent_core::util::now_millis;
use vm_core::network_interfaces::InterfacePath;

use super::GlobalNetwork;
use crate::network_gate::route_revision;

pub(super) const TWO: &str = "proxies:\n  - {name: entry, type: socks5, server: 127.0.0.1, port: 1080}\n  - {name: exit, type: socks5, server: 127.0.0.2, port: 1081}\n";

pub(super) fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cabin-network-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

pub(super) fn passing_report(revision: &str) -> NetworkCheckReport {
    NetworkCheckReport {
        id: "test-check".into(),
        revision: revision.into(),
        started_at: now_millis(),
        finished_at: now_millis(),
        checks: vec![NetworkCheckItem {
            id: "test".into(),
            title: "Test".into(),
            category: NetworkCheckCategory::Route,
            status: NetworkCheckStatus::Pass,
            required: true,
            summary: "passed".into(),
            evidence: Vec::new(),
        }],
        passed_required: 1,
        total_required: 1,
        can_open: true,
    }
}

pub(super) fn open_gate(network: &GlobalNetwork, route: &NetworkRoute) {
    let revision = route_revision(route);
    let token = network.begin_check(revision.clone(), false).unwrap();
    assert!(
        network
            .finish_check(&token, passing_report(&revision), None, &[], None, None)
            .unwrap()
    );
}

pub(super) fn serve_once(body: &str) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let body = body.to_string();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0u8; 4096];
        let _ = stream.read(&mut request);
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    (format!("http://{address}/source"), handle)
}

pub(super) fn test_path(last: u8) -> BTreeMap<String, InterfacePath> {
    BTreeMap::from([(
        "en-test".to_string(),
        InterfacePath { index: 4, ipv4: vec![(Ipv4Addr::new(10, 196, 103, last), Some(Ipv4Addr::new(255, 255, 0, 0)))] },
    )])
}

pub(super) fn bound_network(name: &str) -> (std::path::PathBuf, GlobalNetwork, NetworkRoute) {
    let dir = temp(name);
    let network = GlobalNetwork::open(&dir).unwrap();
    network.start(&dir.join("gateway.sock"), 0).unwrap();
    let route = NetworkRoute { bind_interface: Some("en-test".into()), ..NetworkRoute::default() };
    network.save(&route).unwrap();
    network.runtime().active_route = Some(route.clone());
    open_gate(&network, &route);
    (dir, network, route)
}
