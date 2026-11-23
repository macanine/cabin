//! Standalone, dependency-free Linux ARM64 helper for the opt-in VM tests.
//! The host compiles this with rustc and installs it only in disposable guests.
use std::io::{self, BufRead, Read, Write};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::Path;

// Return the raw JSON scalar following a named key. The test protocol uses
// string values and integer request IDs; retaining JSON strings verbatim also
// keeps escaping intact when they are copied into a response.
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let bytes = json.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos] != b'"' {
            pos += 1;
            continue;
        }
        let start = pos;
        pos += 1;
        while pos < bytes.len() {
            match bytes[pos] {
                b'\\' => pos += 2,
                b'"' => { pos += 1; break; }
                _ => pos += 1,
            }
        }
        if pos > bytes.len() || bytes.get(pos.wrapping_sub(1)) != Some(&b'"') {
            return None;
        }
        if &json[start + 1..pos - 1] != key {
            continue;
        }
        let mut value = pos;
        while bytes.get(value).is_some_and(u8::is_ascii_whitespace) { value += 1; }
        if bytes.get(value) != Some(&b':') { continue; }
        value += 1;
        while bytes.get(value).is_some_and(u8::is_ascii_whitespace) { value += 1; }
        let mut end = value;
        if bytes.get(end) == Some(&b'"') {
            end += 1;
            while end < bytes.len() {
                match bytes[end] {
                    b'\\' => end += 2,
                    b'"' => return Some(&json[value..=end]),
                    _ => end += 1,
                }
            }
            return None;
        }
        while bytes.get(end).is_some_and(u8::is_ascii_digit) { end += 1; }
        return (end > value).then(|| &json[value..end]);
    }
    None
}

fn codex(args: &[String]) -> io::Result<()> {
    match args {
        [arg] if arg == "--version" => println!("codex 0.0.0-test"),
        [login, status] if login == "login" && status == "status" => {
            let home = std::env::var_os("HOME").expect("HOME");
            if Path::new(&home).join(".codex/auth.json").is_file() {
                println!("Signed in using ChatGPT");
            } else {
                println!("Not signed in");
                std::process::exit(1);
            }
        }
        [arg] if arg == "app-server" => {
            let home = std::env::var_os("HOME").expect("HOME");
            let auth = std::fs::read_to_string(Path::new(&home).join(".codex/auth.json"))?;
            let email = field(&auth, "email").unwrap_or("\"\"");
            let plan = field(&auth, "plan_type").unwrap_or("\"plus\"");
            let used = if email == "\"second@example.test\"" { 71 } else { 42 };
            let stdout = io::stdout();
            let mut output = stdout.lock();
            for line in io::stdin().lock().lines() {
                let line = line?;
                let Some(id) = field(&line, "id") else { continue };
                let result = match field(&line, "method") {
                    Some("\"account/read\"") => format!(r#"{{"account":{{"email":{email},"planType":{plan}}}}}"#),
                    Some("\"account/rateLimits/read\"") => format!(r#"{{"rateLimits":{{"planType":{plan},"primary":{{"usedPercent":{used},"windowDurationMins":300,"resetsAt":1900000000}},"secondary":{{"usedPercent":23,"windowDurationMins":10080,"resetsAt":1900600000}}}}}}"#),
                    _ => "{}".to_string(),
                };
                writeln!(output, r#"{{"id":{id},"result":{result}}}"#)?;
                output.flush()?;
            }
        }
        _ => std::process::exit(2),
    }
    Ok(())
}

fn claude(args: &[String]) {
    if args == ["--version"] {
        println!("claude 0.0.0-test");
    } else if args == ["auth", "status", "--json"] {
        println!("{}", r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"claude@example.test","subscriptionType":"pro"}"#);
    } else if args.starts_with(&["-p".into(), "/usage".into()])
        && args.iter().any(|arg| arg == "--no-session-persistence")
    {
        println!("{}", r#"[{"type":"assistant","usage_report":{"rate_limits":{"limits":[{"kind":"session","percent":37.5,"resets_at":"2030-03-17T17:46:40Z"},{"kind":"weekly_all","percent":18,"resets_at":"2030-03-24T17:46:40Z"}]}}},{"type":"result","is_error":false,"result":"Current session: 37.5% used"}]"#);
    } else {
        std::process::exit(2);
    }
}


fn network(args: &[String]) -> io::Result<()> {
    match args.first().map(String::as_str) {
        Some("resolve") => {
            let host = args.get(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing host"))?;
            let address = (host.as_str(), 443).to_socket_addrs()?
                .find(|addr| addr.is_ipv4())
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no IPv4 address"))?;
            println!("{}", address.ip());
        }
        Some("http") => {
            let url = args.get(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing URL"))?;
            let host = url.trim_start_matches("https://").trim_start_matches("http://")
                .split('/').next().unwrap_or_default();
            let port = if url.starts_with("http://") { 80 } else { 443 };
            let address: Vec<_> = if host.contains(':') { host.to_socket_addrs()?.collect() } else { (host, port).to_socket_addrs()?.collect() };
            let mut stream = TcpStream::connect(address.first().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))?)?;
            stream.write_all(b"GET / HTTP/1.0\r\n\r\n")?;
            let mut buf = [0; 1024];
            let n = stream.read(&mut buf)?;
            print!("{}", String::from_utf8_lossy(&buf[..n]));
        }
        Some("connect") => {
            let target = args.get(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing target"))?;
            let mut stream = TcpStream::connect(target)?;
            if let Some(data) = args.get(2) {
                stream.write_all(data.as_bytes())?;
                let mut buf = [0; 1024];
                let n = stream.read(&mut buf)?;
                print!("{}", String::from_utf8_lossy(&buf[..n]));
            }
        }
        Some("status") => {
            let path = args.get(1).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing status path"))?;
            print!("{}", std::fs::read_to_string(path)?);
        }
        Some("pages") => {
            let port = args.get(1).map(String::as_str).unwrap_or("9222");
            let address = format!("127.0.0.1:{port}");
            let mut stream = TcpStream::connect(address)?;
            stream.write_all(b"GET /json/list HTTP/1.0\r\n\r\n")?;
            let mut body = String::new();
            io::Read::read_to_string(&mut stream, &mut body)?;
            let count = body.matches("\"type\":\"page\"").count();
            println!("{count}");
        }
        _ => std::process::exit(2),
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let mut args = std::env::args();
    let program = args.next().unwrap();
    let args: Vec<_> = args.collect();
    match Path::new(&program).file_name().and_then(|name| name.to_str()) {
        Some("codex") => codex(&args),
        Some("claude") => { claude(&args); Ok(()) }
        _ if args.first().map(String::as_str) == Some("network") => network(&args[1..]),
        _ if args.first().map(String::as_str) == Some("vnc") => {
            let listener = TcpListener::bind("127.0.0.1:5900")?;
            for connection in listener.incoming() {
                connection?.write_all(b"RFB 003.008\n")?;
            }
            Ok(())
        }
        _ => { eprintln!("unknown test helper invocation"); std::process::exit(2); }
    }
}

#[cfg(test)]
mod tests {
    use super::field;

    #[test]
    fn extracts_test_protocol_scalars() {
        let data = r#"{"nested":{"email":"a\\\"b@example.test"}, "plan_type" : "pro", "id":42}"#;
        assert_eq!(field(data, "email"), Some(r#""a\\\"b@example.test""#));
        assert_eq!(field(data, "plan_type"), Some(r#""pro""#));
        assert_eq!(field(data, "id"), Some("42"));
        assert_eq!(field(data, "missing"), None);
    }
}
