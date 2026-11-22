use super::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Instant;

#[test]
fn maps_each_service_and_its_provider_fields() {
    let samples = [
        (
            Service::IpSb,
            json!({"ip":"203.0.113.1", "country_code":"US", "country":"United States", "region":"CA", "city":"LA", "isp":"Provider A", "asn":64501, "asn_organization":"AS A", "longitude":-118.2, "latitude":34.1, "timezone":"America/Los_Angeles"}),
            "Provider A",
            "AS A",
            64501,
            -118.2,
            34.1,
        ),
        (
            Service::IpWhoIs,
            json!({"ip":"203.0.113.1", "country_code":"US", "country":"United States", "region":"CA", "city":"LA", "connection":{"asn":64504, "org":"Provider D", "isp":"AS D"}, "longitude":-118.2, "latitude":34.1, "timezone":{"id":"America/Los_Angeles"}}),
            "Provider D",
            "AS D",
            64504,
            -118.2,
            34.1,
        ),
        (
            Service::GeoJs,
            json!({"ip":"203.0.113.1", "country_code":"US", "country":"United States", "region":"CA", "city":"LA", "organization_name":"Provider F", "asn":"64506", "longitude":"-118.2", "latitude":"34.1", "timezone":"America/Los_Angeles"}),
            "Provider F",
            "Provider F",
            64506,
            -118.2,
            34.1,
        ),
    ];

    for (service, data, provider, organization, expected_asn, longitude, latitude) in samples {
        let info = mapping::parse(service, &data).unwrap();
        assert_eq!(info.source, service);
        assert_eq!(info.ip.to_string(), "203.0.113.1");
        assert_eq!(info.country_code.as_deref(), Some("US"));
        assert_eq!(info.country.as_deref(), Some("United States"));
        assert_eq!(info.region.as_deref(), Some("CA"));
        assert_eq!(info.city.as_deref(), Some("LA"));
        assert_eq!(info.isp(), Some(provider));
        assert_eq!(
            info.asn_organization.as_deref(),
            (!organization.is_empty()).then_some(organization)
        );
        assert_eq!(info.asn, Some(expected_asn));
        assert_eq!(info.longitude, Some(longitude));
        assert_eq!(info.latitude, Some(latitude));
        assert_eq!(info.timezone.as_deref(), Some("America/Los_Angeles"));
        assert!(info.fetched_at_unix_ms > 0);
    }
}

#[test]
fn missing_values_remain_missing_and_ipv6_is_accepted() {
    let info = mapping::parse(
        Service::IpWhoIs,
        &json!({"ip":"2001:db8::123", "connection":{"isp":"Fallback ISP", "asn":0}}),
    )
    .unwrap();
    assert_eq!(info.ip.to_string(), "2001:db8::123");
    assert_eq!(info.isp(), Some("Fallback ISP"));
    assert_eq!(info.asn, None);
    assert_eq!(info.latitude, None);
    assert_eq!(info.longitude, None);
    assert!(mapping::parse(Service::IpSb, &json!({"ip":"not an address"})).is_err());
    assert!(mapping::parse(Service::IpSb, &json!({"message":"quota exceeded"})).is_err());
}

#[test]
fn prefers_the_primary_provider_field_when_both_are_present() {
    let ip_sb = mapping::parse(
        Service::IpSb,
        &json!({"ip":"203.0.113.1", "organization":"Primary", "isp":"Fallback"}),
    )
    .unwrap();
    assert_eq!(ip_sb.isp(), Some("Primary"));

    let ipinfo = mapping::parse(
        Service::IpInfo,
        &json!({"ip":"8.8.8.8", "org":"AS15169 Google", "country":"US", "loc":"34.1,-118.2"}),
    )
    .unwrap();
    assert_eq!(ipinfo.asn, Some(15169));
    assert_eq!(ipinfo.latitude, Some(34.1));
    assert_eq!(ipinfo.country_code.as_deref(), Some("US"));
}

enum Reply {
    Http(u16, &'static str),
    Disconnect,
}

/// A loopback-only server with a deadline, so a failed test cannot hang.
fn server(replies: Vec<Reply>) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let task = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut requests = Vec::new();
        for reply in replies {
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "timed out waiting for a request");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let count = stream.read(&mut buf).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..count]);
                if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8_lossy(&request).into_owned());
            if let Reply::Http(status, body) = reply {
                let response = format!(
                    "HTTP/1.1 {status} test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        }
        requests
    });
    (format!("http://{address}"), task)
}

#[test]
fn falls_back_after_http_and_json_errors() {
    let (url, server) = server(vec![
        Reply::Http(503, "unavailable"),
        Reply::Http(200, "not json"),
        Reply::Http(
            200,
            r#"{"ip":"198.51.100.2","connection":{"isp":"Fallback provider"}}"#,
        ),
    ]);
    let detector = Detector::builder().direct().build().unwrap();
    let result = detector
        .detect_endpoints([
            Endpoint {
                service: Service::IpSb,
                url: &url,
            },
            Endpoint {
                service: Service::IpInfo,
                url: &url,
            },
            Endpoint {
                service: Service::IpWhoIs,
                url: &url,
            },
        ])
        .unwrap();
    assert_eq!(result.source, Service::IpWhoIs);
    assert_eq!(result.isp(), Some("Fallback provider"));
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn retries_a_transport_error_before_using_the_same_service() {
    let (url, server) = server(vec![
        Reply::Disconnect,
        Reply::Http(200, r#"{"ip":"198.51.100.7","organization":"Recovered"}"#),
    ]);
    let detector = Detector::builder()
        .direct()
        .retries(1)
        .retry_delay(Duration::ZERO)
        .build()
        .unwrap();
    let info = detector
        .detect_endpoints([Endpoint {
            service: Service::IpSb,
            url: &url,
        }])
        .unwrap();
    assert_eq!(info.isp(), Some("Recovered"));
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn reports_each_failed_service() {
    let (url, server) = server(vec![
        Reply::Http(429, "rate limited"),
        Reply::Http(200, r#"{"error":true}"#),
    ]);
    let detector = Detector::builder().direct().build().unwrap();
    let error = detector
        .detect_endpoints([
            Endpoint {
                service: Service::IpSb,
                url: &url,
            },
            Endpoint {
                service: Service::GeoJs,
                url: &url,
            },
        ])
        .unwrap_err();
    assert_eq!(error.failures.len(), 2);
    assert_eq!(error.failures[0].kind, FailureKind::HttpStatus(429));
    assert_eq!(error.failures[0].attempts, 1);
    assert!(matches!(
        error.failures[1].kind,
        FailureKind::InvalidResponse(_)
    ));
    assert_eq!(error.failures[1].attempts, 1);
    server.join().unwrap();
}

#[test]
fn explicit_http_proxy_carries_the_request() {
    let (proxy, server) = server(vec![Reply::Http(
        200,
        r#"{"ip":"198.51.100.9","organization":"Via proxy"}"#,
    )]);
    let detector = Detector::builder().proxy_url(&proxy).build().unwrap();
    let info = detector
        .detect_endpoints([Endpoint {
            service: Service::IpSb,
            url: "http://ip-test.invalid/geo",
        }])
        .unwrap();
    assert_eq!(info.isp(), Some("Via proxy"));
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET http://ip-test.invalid/geo HTTP/1.1\r\n"));
}

#[test]
fn explicit_socks_proxy_receives_the_hostname_and_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut greeting = [0u8; 2];
        stream.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting[0], 5);
        let mut methods = vec![0; greeting[1] as usize];
        stream.read_exact(&mut methods).unwrap();
        assert!(methods.contains(&0));
        stream.write_all(&[5, 0]).unwrap();

        let mut connect = [0u8; 5];
        stream.read_exact(&mut connect).unwrap();
        assert_eq!(&connect[..4], &[5, 1, 0, 3]); // Domain, resolved by the proxy.
        let mut host_and_port = vec![0; connect[4] as usize + 2];
        stream.read_exact(&mut host_and_port).unwrap();
        assert_eq!(&host_and_port[..connect[4] as usize], b"ip-test.invalid");
        stream
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
            .unwrap();

        let mut request = Vec::new();
        let mut buf = [0; 1024];
        loop {
            let count = stream.read(&mut buf).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buf[..count]);
            if request.windows(4).any(|part| part == b"\r\n\r\n") {
                break;
            }
        }
        let body = r#"{"ip":"198.51.100.10","organization":"Via SOCKS"}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        request
    });

    let detector = Detector::builder()
        .proxy_url(format!("socks5://{address}"))
        .timeout(Duration::from_secs(3))
        .retries(0)
        .build()
        .unwrap();
    let info = detector
        .detect_endpoints([Endpoint {
            service: Service::IpSb,
            url: "http://ip-test.invalid/geo",
        }])
        .unwrap();
    assert_eq!(info.isp(), Some("Via SOCKS"));
    assert!(server.join().unwrap().starts_with(b"GET /geo HTTP/1.1\r\n"));
}

#[test]
fn rejects_invalid_configuration() {
    assert!(matches!(
        Detector::builder().services([]).build(),
        Err(ConfigurationError::NoServices)
    ));
    assert!(matches!(
        Detector::builder().timeout(Duration::ZERO).build(),
        Err(ConfigurationError::ZeroTimeout)
    ));
    for proxy in [
        "not a url",
        "http://",
        "https://127.0.0.1:8888",
        "http://[::1]:8888",
        "http://127.0.0.1:8888/path",
    ] {
        assert!(
            matches!(
                Detector::builder().proxy_url(proxy).build(),
                Err(ConfigurationError::InvalidProxy)
            ),
            "{proxy}"
        );
    }
}

#[test]
fn lookup_urls_name_the_address() {
    let ip: IpAddr = "203.0.113.9".parse().unwrap();
    assert_eq!(
        Service::IpSb.lookup_url(ip).unwrap(),
        "https://api.ip.sb/geoip/203.0.113.9"
    );
    assert_eq!(
        Service::IpWhoIs.lookup_url(ip).unwrap(),
        "https://ipwho.is/203.0.113.9"
    );
    assert_eq!(
        Service::GeoJs.lookup_url(ip).unwrap(),
        "https://get.geojs.io/v1/ip/geo/203.0.113.9.json"
    );
    assert!(Service::Cloudflare.lookup_url(ip).is_none());
}

#[test]
fn a_lookup_only_accepts_an_answer_about_that_address() {
    let (url, server) = server(vec![
        Reply::Http(200, r#"{"ip":"198.51.100.1","organization":"Caller"}"#),
        Reply::Http(
            200,
            r#"{"ip":"203.0.113.9","organization":"Node host","country_code":"KR"}"#,
        ),
    ]);
    let detector = Detector::builder().direct().build().unwrap();
    let info = detector
        .detect_matching(
            [
                Endpoint {
                    service: Service::IpSb,
                    url: &url,
                },
                Endpoint {
                    service: Service::IpSb,
                    url: &url,
                },
            ],
            Some("203.0.113.9".parse().unwrap()),
        )
        .unwrap();
    assert_eq!(info.isp(), Some("Node host"));
    assert_eq!(info.country_code.as_deref(), Some("KR"));
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn declined_and_locationless_answers_give_way_to_a_located_one() {
    let (url, server) = server(vec![
        Reply::Http(
            200,
            r#"{"ip":"203.0.113.9","error":true,"reason":"RateLimited"}"#,
        ),
        Reply::Http(200, r#"{"ip":"203.0.113.9","organization":"Bare"}"#),
        Reply::Http(
            200,
            r#"{"ip":"203.0.113.9","country_code":"US","city":"Ashburn"}"#,
        ),
    ]);
    let detector = Detector::builder().direct().build().unwrap();
    let info = detector
        .detect_matching(
            [
                Endpoint {
                    service: Service::IpInfo,
                    url: &url,
                },
                Endpoint {
                    service: Service::IpSb,
                    url: &url,
                },
                Endpoint {
                    service: Service::IpSb,
                    url: &url,
                },
            ],
            Some("203.0.113.9".parse().unwrap()),
        )
        .unwrap();
    assert_eq!(info.city.as_deref(), Some("Ashburn"));
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn a_locationless_answer_is_the_last_resort() {
    let (url, server) = server(vec![
        Reply::Http(200, r#"{"ip":"203.0.113.9","organization":"Bare"}"#),
        Reply::Http(503, "down"),
    ]);
    let detector = Detector::builder().direct().build().unwrap();
    let info = detector
        .detect_matching(
            [
                Endpoint {
                    service: Service::IpSb,
                    url: &url,
                },
                Endpoint {
                    service: Service::IpSb,
                    url: &url,
                },
            ],
            None,
        )
        .unwrap();
    assert_eq!(info.isp(), Some("Bare"));
    assert!(!info.has_location());
    server.join().unwrap();
}

#[test]
fn flclash_sources_and_response_contracts() {
    assert_eq!(
        Service::ALL.map(Service::url),
        [
            "https://www.cloudflare.com/cdn-cgi/trace",
            "https://api.ip.sb/geoip",
            "https://ipinfo.io/json",
            "https://ipwho.is",
            "https://get.geojs.io/v1/ip/geo.json",
            "https://api.country.is",
            "https://api.ipquery.io/?format=json",
            "https://ident.me/json",
        ]
    );
    let trace = mapping::parse_body(
        Service::Cloudflare,
        b"fl=200
ip=8.8.8.8
loc=us
",
    )
    .unwrap();
    assert_eq!(trace.ip.to_string(), "8.8.8.8");
    assert_eq!(trace.country_code.as_deref(), Some("US"));
    let country =
        mapping::parse(Service::CountryIs, &json!({"ip":"8.8.8.8", "country":"US"})).unwrap();
    assert_eq!(country.country_code.as_deref(), Some("US"));
    let query = mapping::parse(
        Service::IpQuery,
        &json!({"ip":"8.8.8.8", "location":{"country_code":"US","timezone":"America/New_York"}}),
    )
    .unwrap();
    assert_eq!(query.timezone.as_deref(), Some("America/New_York"));
    let ident = mapping::parse(
        Service::IdentMe,
        &json!({"ip":"8.8.8.8", "cc":"US", "aso":"Provider", "tz":"America/Chicago"}),
    )
    .unwrap();
    assert_eq!(ident.isp(), Some("Provider"));
    assert_eq!(ident.country_code.as_deref(), Some("US"));
    for value in [
        "127.0.0.1",
        "100.64.1.2",
        "198.18.1.1",
        "192.0.2.1",
        "::1",
        "fc00::1",
        "2001:db8::1",
    ] {
        assert!(!public_ip(value.parse().unwrap()), "{value}");
    }
}

#[test]
fn flclash_ip_type_source_precedence_and_fallback() {
    use QualityService::*;
    let ip = "8.8.8.8".parse().unwrap();
    let classify = |source, data| quality::parse(source, ip, &data).unwrap();
    assert!(quality::parse(IdentMe, ip, &json!({"ip":"1.1.1.1", "type":"isp"})).is_none());
    assert_eq!(
        classify(IdentMe, json!({"ip":"8.8.8.8", "type":"isp"})).ip_type,
        IpType::Residential
    );
    assert_eq!(
        classify(IpApiIs, json!({"is_mobile":true, "is_datacenter":true})).ip_type,
        IpType::Server
    );
    assert_eq!(
        classify(IpLocate, json!({"company":{"type":"government"}})).ip_type,
        IpType::Commercial
    );
    let inferred = classify(
        IpApiCom,
        json!({"status":"success", "query":"8.8.8.8", "as":"AS15169 Google", "hosting":false,"mobile":false}),
    );
    assert!(inferred.inferred);
    assert_eq!(inferred.ip_type, IpType::Residential);
    assert_eq!(inferred.asn, Some(15169));
    assert_eq!(
        classify(IpQuery, json!({"risk":{"is_mobile":true}})).ip_type,
        IpType::Mobile
    );
    assert_eq!(
        classify(
            ProxyCheck,
            json!({"status":"ok", "8.8.8.8":{"network":{"type":"Wireless"}}})
        )
        .ip_type,
        IpType::Mobile
    );
}

#[test]
fn a_slow_source_does_not_delay_a_hedged_valid_answer() {
    let started = Instant::now();
    let results = race::race(
        [0, 1, 2],
        race::Schedule::address(Duration::from_secs(5)),
        |source, _| {
            if source == 0 {
                thread::sleep(Duration::from_secs(3));
            }
            source
        },
        |answer| *answer == 1,
    );
    assert_eq!(results, vec![1]);
    assert!(started.elapsed() < Duration::from_secs(2));
}
