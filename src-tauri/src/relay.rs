//! 本地中继：给 claude-science 的模型清单请求应答切换器登记的目录，
//! 其余请求读体重写模型名后转发到真实供应商并回流流式响应。
//! 选择器标识沿用 CSSwitch 的 claude-csswitch- 前缀，界面按 claude 家族规则放行；
//! 未登记的 claude 家族请求回落到供应商默认模型，等价于旧网关的角色绑定后备。

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct RelayTarget {
    /// 供应商基地址，已剥掉尾部斜杠与版本段
    pub upstream: String,
    /// 供应商默认模型的上游标识
    pub default_model: String,
    /// 目录清单的上游标识
    pub catalog: Vec<String>,
    /// claude 家族角色到上游模型的映射，未配置的角色回退默认模型
    pub roles: BTreeMap<String, (String, String)>,
}

#[derive(Clone)]
struct Inner {
    target: RelayTarget,
    map: BTreeMap<String, String>,
}

pub struct Relay {
    pub port: u16,
    inner: Arc<Mutex<Inner>>,
}

const FORWARD_HEADERS: [&str; 7] = [
    "authorization",
    "x-api-key",
    "anthropic-version",
    "anthropic-beta",
    "content-type",
    "accept",
    "user-agent",
];

/// 固定端口保证程序重启后已运行守护进程的接口地址仍然有效；
/// 被占用时退回随机端口。
const FIXED_PORT: u16 = 39171;

pub fn start() -> Option<Relay> {
    let listener = TcpListener::bind(("127.0.0.1", FIXED_PORT))
        .or_else(|_| TcpListener::bind("127.0.0.1:0"))
        .ok()?;
    let port = listener.local_addr().ok()?.port();
    let inner = Arc::new(Mutex::new(Inner {
        target: RelayTarget {
            upstream: String::new(),
            default_model: String::new(),
            catalog: vec![],
            roles: BTreeMap::new(),
        },
        map: BTreeMap::new(),
    }));
    let shared = inner.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let t = shared.clone();
            std::thread::spawn(move || serve(stream, &t));
        }
    });
    Some(Relay { port, inner })
}

impl Relay {
    pub fn set_target(&self, target: RelayTarget) {
        let mut inner = self.inner.lock().unwrap();
        let mut map = BTreeMap::new();
        for id in target.catalog.iter().chain(std::iter::once(&target.default_model)) {
            if id.is_empty() {
                continue;
            }
            map.insert(selector_for(id), id.clone());
        }
        // 带显示名的角色映射作为独立选择器条目出现在界面清单里
        for (role, (model, display)) in &target.roles {
            if !model.is_empty() && !display.is_empty() {
                map.insert(format!("claude-csswitch-role-{role}"), model.clone());
            }
        }
        inner.target = target;
        inner.map = map;
    }
}

/// claude-csswitch- 前缀加净化后的上游标识，界面按 claude 家族规则放行。
fn selector_for(upstream_id: &str) -> String {
    let slug: String = upstream_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("claude-csswitch-{slug}")
}

/// 供应商地址归一：去尾部斜杠；结尾的 /v1 剥掉，因为守护进程请求时自带 /v1 前缀。
pub fn normalize_upstream(base: &str) -> String {
    let mut u = base.trim().trim_end_matches('/').to_string();
    if let Some(stripped) = u.strip_suffix("/v1") {
        u = stripped.trim_end_matches('/').to_string();
    }
    u
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 8192];
    let head_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > 1 << 20 {
            return None;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.lines();
    let first = lines.next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let raw_path = parts.next().unwrap_or("/").to_string();
    let (path, query) = match raw_path.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw_path, String::new()),
    };
    let mut headers = vec![];
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let content_length: usize = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);
    Some(Request { method, path, query, headers, body })
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn models_response(inner: &Inner) -> String {
    // 条目 = 目录模型 + 带显示名的角色映射
    let mut items: Vec<(String, String)> = vec![];
    for id in inner.target.catalog.iter().chain(std::iter::once(&inner.target.default_model)) {
        if id.is_empty() {
            continue;
        }
        let sel = selector_for(id);
        if !items.iter().any(|(s, _)| *s == sel) {
            items.push((sel, id.clone()));
        }
    }
    for (role, (model, display)) in &inner.target.roles {
        if model.is_empty() || display.is_empty() {
            continue;
        }
        let sel = format!("claude-csswitch-role-{role}");
        if !items.iter().any(|(s, _)| *s == sel) {
            items.push((sel, display.clone()));
        }
    }
    if items.is_empty() {
        return "{\"data\":[],\"has_more\":false,\"first_id\":null,\"last_id\":null}".to_string();
    }
    let entries: Vec<String> = items
        .iter()
        .map(|(sel, display)| {
            let d = json_escape(display);
            format!(
                "{{\"type\":\"model\",\"id\":\"{sel}\",\"display_name\":\"{d}\",\"supports_tools\":true,\"capabilities\":{{\"reasoning_round_trip\":\"none\",\"forced_tool_choice\":null,\"structured_output\":null,\"vision\":null}},\"created_at\":\"2026-01-01T00:00:00Z\"}}"
            )
        })
        .collect();
    format!(
        "{{\"data\":[{}],\"has_more\":false,\"first_id\":\"{}\",\"last_id\":\"{}\"}}",
        entries.join(","),
        items[0].0,
        items.last().unwrap().0
    )
}

/// 请求体里的模型名映射：命中选择器表用上游标识；
/// claude 家族名先按角色映射解析，未配置的角色回落默认模型；其余原样透传。
fn rewrite_model(inner: &Inner, body: &[u8]) -> Vec<u8> {
    let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };
    let Some(name) = v.get("model").and_then(|m| m.as_str()).map(|s| s.to_string()) else {
        return body.to_vec();
    };
    let mapped = if let Some(up) = inner.map.get(&name) {
        up.clone()
    } else if name.starts_with("claude-") {
        let role = ["fable", "sonnet", "opus", "haiku"]
            .iter()
            .find(|r| name.contains(&format!("-{r}")) || name.contains(&format!("{r}-")));
        role.and_then(|r| inner.target.roles.get(*r))
            .map(|(m, _)| m.clone())
            .unwrap_or_else(|| inner.target.default_model.clone())
    } else {
        name
    };
    if let Some(obj) = v.as_object_mut() {
        obj.insert("model".to_string(), serde_json::Value::String(mapped));
    }
    serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec())
}

fn serve(mut stream: TcpStream, inner: &Mutex<Inner>) {
    let Some(req) = read_request(&mut stream) else { return };
    let inner = inner.lock().unwrap().clone();
    if inner.target.upstream.is_empty() {
        let _ = write_simple(&mut stream, 503, "relay not configured");
        return;
    }
    if req.method == "GET" && req.path == "/v1/models" {
        let body = models_response(&inner);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(resp.as_bytes());
        return;
    }
    let url = if req.query.is_empty() {
        format!("{}{}", inner.target.upstream, req.path)
    } else {
        format!("{}{}?{}", inner.target.upstream, req.path, req.query)
    };
    let body = rewrite_model(&inner, &req.body);
    let mut request = ureq::request(&req.method, &url);
    for (k, v) in &req.headers {
        if FORWARD_HEADERS.contains(&k.as_str()) {
            request = request.set(k, v);
        }
    }
    let response = match if body.is_empty() {
        request.call()
    } else {
        request.send_bytes(&body)
    } {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => {
            let msg = format!("relay upstream error: {e}");
            let _ = write_simple(&mut stream, 502, &msg);
            return;
        }
    };
    let status = response.status();
    let content_type = response.content_type().to_string();
    let mut reader = response.into_reader();
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
        status,
        reason(status),
        if content_type.is_empty() { "application/octet-stream".to_string() } else { content_type }
    );
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut buf = [0u8; 16384];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if stream.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = stream.flush();
            }
            Err(_) => break,
        }
    }
}

fn write_simple(stream: &mut TcpStream, code: u16, msg: &str) -> std::io::Result<()> {
    let body = format!("{{\"error\":{{\"message\":\"{}\"}}}}", json_escape(msg));
    let resp = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        reason(code),
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes())
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Response",
    }
}
