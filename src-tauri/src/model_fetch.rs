//! 模型列表获取：按候选地址依次请求，解析 data 数组中的 id。
//! 候选规则沿用 cc-switch 的 build_models_url_candidates。

use serde::Deserialize;
use std::time::Duration;

const COMPAT_SUBPATHS: [&str; 9] = [
    "/api/claudecode",
    "/api/anthropic",
    "/apps/anthropic",
    "/api/coding",
    "/claudecode",
    "/anthropic",
    "/step_plan",
    "/coding",
    "/claude",
];

#[derive(Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    id: String,
}

/// 判断地址是否以 /v数字 形式的版本段结尾，返回版本段与剥掉版本段的根地址。
fn version_segment(base: &str) -> Option<(String, String)> {
    let pos = base.rfind('/')?;
    let (head, tail) = base.split_at(pos);
    let inner = &tail[1..];
    if inner.len() > 1 && inner.starts_with('v') && inner[1..].bytes().all(|b| b.is_ascii_digit()) {
        Some((tail.to_string(), head.to_string()))
    } else {
        None
    }
}

pub fn build_models_url_candidates(base: &str, explicit: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    if let Some(u) = explicit.filter(|s| !s.trim().is_empty()) {
        return vec![u.trim().to_string()];
    }
    let base = base.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return out;
    }
    if let Some((seg, root)) = version_segment(&base) {
        out.push(format!("{}/models", base));
        if seg != "/v1" {
            out.push(format!("{}/v1/models", root));
        }
    } else {
        out.push(format!("{}/v1/models", base));
    }
    let mut subs: Vec<&str> = COMPAT_SUBPATHS.to_vec();
    subs.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for sp in subs {
        if let Some(root) = base.strip_suffix(sp) {
            let root = root.trim_end_matches('/');
            if root.contains("://") {
                out.push(format!("{}/v1/models", root));
                out.push(format!("{}/models", root));
            }
            break;
        }
    }
    out.dedup();
    out
}

pub fn fetch_models(
    base_url: &str,
    api_key: &str,
    _key_type: &str,
    models_url: &str,
) -> Result<Vec<String>, String> {
    let candidates = build_models_url_candidates(base_url, Some(models_url));
    if candidates.is_empty() {
        return Err("接口地址为空".to_string());
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();
    let mut last_err = String::from("未请求任何候选地址");
    for url in &candidates {
        let mut req = agent
            .get(url)
            .set("anthropic-version", "2023-06-01")
            .set("Accept", "application/json");
        if !api_key.trim().is_empty() {
            req = req
                .set("x-api-key", api_key.trim())
                .set("Authorization", &format!("Bearer {}", api_key.trim()));
        }
        match req.call() {
            Ok(resp) => match resp.into_json::<ModelsResponse>() {
                Ok(body) => {
                    let ids: Vec<String> = body
                        .data
                        .into_iter()
                        .map(|e| e.id)
                        .filter(|s| !s.is_empty())
                        .collect();
                    if ids.is_empty() {
                        last_err = format!("{} 返回的模型列表为空", url);
                    } else {
                        return Ok(ids);
                    }
                }
                Err(e) => last_err = format!("{} 响应解析失败：{}", url, e),
            },
            Err(ureq::Error::Status(code, _)) => {
                last_err = format!("{} 返回 HTTP {}", url, code)
            }
            Err(e) => last_err = format!("{} 请求失败：{}", url, e),
        }
    }
    Err(last_err)
}
