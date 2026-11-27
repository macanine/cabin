use serde_json::Value;

use crate::{IpInfo, Service, now_unix_ms};

fn field<'a>(data: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(data, |value, key| value.get(*key))
}

pub(super) fn text(data: &Value, path: &[&str]) -> Option<String> {
    field(data, path)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn number(data: &Value, path: &[&str]) -> Option<f64> {
    let value = field(data, path)?;
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|value: &f64| value.is_finite())
}

pub(super) fn asn(data: &Value, path: &[&str]) -> Option<u32> {
    let value = field(data, path)?;
    let parsed = value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .or_else(|| {
            let string = value.as_str()?.trim();
            let digits = string
                .split(|c: char| !c.is_ascii_digit())
                .find(|part| !part.is_empty())?;
            digits.parse().ok()
        });
    parsed.filter(|number| *number != 0)
}

pub(super) fn country_code(code: Option<String>) -> Option<String> {
    code.filter(|code| {
        code.len() == 2
            && code.bytes().all(|byte| byte.is_ascii_alphabetic())
            && !code.eq_ignore_ascii_case("XX")
    })
    .map(|code| code.to_ascii_uppercase())
}

pub(super) fn parse_body(service: Service, body: &[u8]) -> Result<IpInfo, String> {
    if service == Service::Cloudflare {
        let body = std::str::from_utf8(body).map_err(|_| "trace is not UTF-8")?;
        let field = |key: &str| {
            body.lines()
                .filter_map(|line| line.split_once('='))
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value)
        };
        return parse(
            service,
            &serde_json::json!({"ip": field("ip"), "country_code": field("loc")}),
        );
    }
    let data = serde_json::from_slice(body).map_err(|error| format!("invalid JSON: {error}"))?;
    parse(service, &data)
}

pub(crate) fn parse(service: Service, data: &Value) -> Result<IpInfo, String> {
    if data.get("error").and_then(Value::as_bool) == Some(true)
        || data.get("success").and_then(Value::as_bool) == Some(false)
    {
        let reason = text(data, &["reason"])
            .or_else(|| text(data, &["message"]))
            .unwrap_or_else(|| "error".into());
        return Err(format!("the service declined: {reason}"));
    }
    let ip = text(data, &["ip"])
        .and_then(|ip| ip.parse().ok())
        .ok_or_else(|| "missing or invalid IP address".to_owned())?;

    let mut info = IpInfo {
        ip,
        country_code: country_code(text(data, &["country_code"])),
        continent_code: text(data, &["continent_code"])
            .map(|code| code.trim().to_ascii_uppercase())
            .filter(|code| {
                matches!(
                    code.as_str(),
                    "AF" | "AN" | "AS" | "EU" | "NA" | "OC" | "SA"
                )
            }),
        country: text(data, &["country"]),
        region: text(data, &["region"]),
        city: text(data, &["city"]),
        organization: None,
        asn: asn(data, &["asn"]),
        asn_organization: None,
        longitude: number(data, &["longitude"]),
        latitude: number(data, &["latitude"]),
        timezone: text(data, &["timezone"]),
        ip_type: None,
        source: service,
        fetched_at_unix_ms: now_unix_ms(),
    };

    match service {
        Service::IpSb => {
            info.organization = text(data, &["organization"]).or_else(|| text(data, &["isp"]));
            info.asn_organization = text(data, &["asn_organization"]);
        }
        Service::IpInfo => {
            info.country_code = country_code(text(data, &["country"]));
            info.country = None;
            info.organization = text(data, &["org"]);
            info.asn = asn(data, &["org"]);
            if let Some(point) = text(data, &["loc"])
                && let Some((lat, lon)) = point.split_once(',')
            {
                info.latitude = lat.trim().parse().ok();
                info.longitude = lon.trim().parse().ok();
            }
        }
        Service::IpWhoIs => {
            info.organization =
                text(data, &["connection", "org"]).or_else(|| text(data, &["connection", "isp"]));
            info.asn = asn(data, &["connection", "asn"]);
            info.asn_organization = text(data, &["connection", "isp"]);
            info.timezone = text(data, &["timezone", "id"]);
        }
        Service::CountryIs => {
            info.country_code = country_code(text(data, &["country"]));
            info.country = None;
        }
        Service::IpQuery => {
            info.country_code = country_code(text(data, &["location", "country_code"]));
            info.country = text(data, &["location", "country"]);
            info.region = text(data, &["location", "state"]);
            info.city = text(data, &["location", "city"]);
            info.longitude = number(data, &["location", "longitude"]);
            info.latitude = number(data, &["location", "latitude"]);
            info.timezone = text(data, &["location", "timezone"]);
            info.organization = text(data, &["isp", "org"]).or_else(|| text(data, &["isp", "isp"]));
            info.asn = asn(data, &["isp", "asn"]);
        }
        Service::IdentMe => {
            info.country_code = country_code(text(data, &["cc"]));
            info.organization = text(data, &["aso"]);
            info.city = text(data, &["city"]);
            info.latitude = number(data, &["latitude"]).or_else(|| number(data, &["lat"]));
            info.longitude = number(data, &["longitude"]).or_else(|| number(data, &["lon"]));
            info.timezone = text(data, &["tz"]).or(info.timezone);
        }
        Service::Cloudflare => {}
        Service::GeoJs => {
            info.organization = text(data, &["organization_name"]);
            info.asn_organization = info.organization.clone();
        }
    }

    info.latitude = info.latitude.filter(|value| (-90.0..=90.0).contains(value));
    info.longitude = info
        .longitude
        .filter(|value| (-180.0..=180.0).contains(value));
    Ok(info)
}
