//! Address classification using the public services in FlClash v0.9.4.

use std::net::IpAddr;
use std::time::Duration;

use serde_json::Value;

use crate::mapping::{asn, text};
use crate::{Detector, IpType, race, read_body};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityService {
    IdentMe,
    IpApiCom,
    IpQuery,
    IpLocate,
    IpApiIs,
    ProxyCheck,
}

impl QualityService {
    pub const ALL: [Self; 6] = [
        Self::IdentMe,
        Self::IpApiCom,
        Self::IpQuery,
        Self::IpLocate,
        Self::IpApiIs,
        Self::ProxyCheck,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::IdentMe => "ident.me",
            Self::IpApiCom => "ip-api.com",
            Self::IpQuery => "IPQuery",
            Self::IpLocate => "IPLocate",
            Self::IpApiIs => "ipapi.is",
            Self::ProxyCheck => "ProxyCheck",
        }
    }

    pub fn url(self, ip: IpAddr) -> String {
        match self {
            Self::IdentMe => "https://ident.me/json".into(),
            Self::IpApiCom => format!(
                "http://ip-api.com/json/{ip}?fields=status,query,isp,org,as,mobile,proxy,hosting"
            ),
            Self::IpQuery => format!("https://api.ipquery.io/{ip}?format=json"),
            Self::IpLocate => format!("https://iplocate.io/api/lookup/{ip}"),
            Self::IpApiIs => format!("https://api.ipapi.is/?q={ip}"),
            Self::ProxyCheck => format!("https://proxycheck.io/v3/{ip}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub source: QualityService,
    pub ip_type: IpType,
    pub inferred: bool,
    pub organization: Option<String>,
    pub asn: Option<u32>,
}

fn declared(value: &Value) -> Option<IpType> {
    match value.as_str()? {
        "isp" => Some(IpType::Residential),
        "business" | "education" | "government" | "banking" => Some(IpType::Commercial),
        "hosting" => Some(IpType::Server),
        _ => None,
    }
}

fn pick(hosting: bool, mobile: bool, declared: Option<IpType>) -> Option<IpType> {
    if hosting || declared == Some(IpType::Server) {
        Some(IpType::Server)
    } else if mobile {
        Some(IpType::Mobile)
    } else {
        declared
    }
}

fn flag(value: &Value) -> bool {
    value.as_bool() == Some(true)
}

pub(super) fn parse(source: QualityService, ip: IpAddr, data: &Value) -> Option<Classification> {
    use QualityService::*;
    if !data.is_object()
        || data
            .get("error")
            .is_some_and(|error| !error.is_null() && error != false)
    {
        return None;
    }
    if ["ip", "query"]
        .iter()
        .filter_map(|key| data.get(key))
        .any(|reply| reply.as_str().and_then(|text| text.parse().ok()) != Some(ip))
    {
        return None;
    }
    let (detected, organization, asn) = match source {
        IdentMe => {
            data.get("ip")?;
            (
                declared(&data["type"]),
                text(data, &["aso"]),
                asn(data, &["asn"]),
            )
        }
        IpApiCom => {
            if data["status"] != "success" {
                return None;
            }
            (
                pick(flag(&data["hosting"]), flag(&data["mobile"]), None),
                text(data, &["org"]).or_else(|| text(data, &["isp"])),
                asn(data, &["as"]),
            )
        }
        IpQuery => {
            let risk = &data["risk"];
            if !risk.is_object() {
                return None;
            }
            (
                pick(flag(&risk["is_datacenter"]), flag(&risk["is_mobile"]), None),
                text(data, &["isp", "org"]).or_else(|| text(data, &["isp", "isp"])),
                asn(data, &["isp", "asn"]),
            )
        }
        IpLocate => (
            pick(
                flag(&data["privacy"]["is_hosting"]),
                false,
                declared(&data["company"]["type"]).or_else(|| declared(&data["asn"]["type"])),
            ),
            text(data, &["company", "name"]).or_else(|| text(data, &["asn", "name"])),
            asn(data, &["asn", "asn"]),
        ),
        IpApiIs => (
            pick(
                flag(&data["is_datacenter"]),
                flag(&data["is_mobile"]),
                declared(&data["company"]["type"]).or_else(|| declared(&data["asn"]["type"])),
            ),
            text(data, &["company", "name"]).or_else(|| text(data, &["asn", "org"])),
            asn(data, &["asn", "asn"]),
        ),
        ProxyCheck => {
            if !matches!(data["status"].as_str(), Some("ok" | "warning")) {
                return None;
            }
            let result = &data[ip.to_string()];
            let network = &result["network"];
            let kind = network["type"].as_str();
            let declared = match kind {
                Some("Residential") => Some(IpType::Residential),
                Some("Business") => Some(IpType::Commercial),
                _ => None,
            };
            (
                pick(
                    flag(&result["detections"]["hosting"]) || kind == Some("Hosting"),
                    kind == Some("Wireless"),
                    declared,
                ),
                text(network, &["organisation"]),
                asn(network, &["asn"]),
            )
        }
    };
    let inferred = detected.is_none() && matches!(source, IpApiCom | IpQuery);
    let ip_type = detected.or(inferred.then_some(IpType::Residential))?;
    Some(Classification {
        source,
        ip_type,
        inferred,
        organization,
        asn,
    })
}

impl Detector {
    pub fn classify(&self, ip: IpAddr) -> Option<Classification> {
        if !crate::public_ip(ip) || self.ipv4_only && !ip.is_ipv4() {
            return None;
        }
        let agent = self.agent.clone();
        let outcomes = race::race(
            QualityService::ALL,
            race::Schedule::quality(),
            move |source, timeout| query(&agent, source, ip, timeout),
            |answer| answer.as_ref().is_some_and(|answer| !answer.inferred),
        );
        let mut answers = outcomes.into_iter().flatten();
        let first = answers.next()?;
        if !first.inferred {
            return Some(first);
        }
        answers.find(|answer| !answer.inferred).or(Some(first))
    }
}

fn query(
    agent: &ureq::Agent,
    source: QualityService,
    ip: IpAddr,
    timeout: Duration,
) -> Option<Classification> {
    let response = agent.get(&source.url(ip)).timeout(timeout).call().ok()?;
    if response.status() != 200 {
        return None;
    }
    let body = read_body(response).ok()?;
    parse(source, ip, &serde_json::from_slice(&body).ok()?)
}
