//! 本地虚拟登录：在 claude-science 数据目录内铸一枚本地加密的 OAuth 令牌，
//! 使网页端认证状态直接变为已登录；推理凭据仍走环境变量密钥，与 claude.ai 解耦。
//! 加密格式与 CSSwitch 的 oauth_forge 一致：HKDF-SHA256 派生 AES-256-GCM，
//! 输出 v2: 前缀加 base64(IV ‖ 密文 ‖ tag)，AAD 为 v2:oauth。

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use hkdf::Hkdf;
use serde_json::json;
use sha2::Sha256;

const KEY_NAMES: [&str; 4] = [
    "ANTHROPIC_API_KEY_ENCRYPTION_KEY",
    "OAUTH_ENCRYPTION_KEY",
    "JWT_SIGNING_SECRET",
    "USER_SECRET_ENCRYPTION_KEY",
];
const HKDF_INFO: &[u8] = b"operon:aes-256-gcm:oauth";
const AAD: &[u8] = b"v2:oauth";
const FAKE_EMAIL: &str = "cs-router@localhost.invalid";

fn rand_bytes(n: usize) -> std::io::Result<Vec<u8>> {
    let mut f = fs::File::open("/dev/urandom")?;
    let mut b = vec![0u8; n];
    f.read_exact(&mut b)?;
    Ok(b)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn uuid_v4() -> Result<String, String> {
    let mut b = rand_bytes(16).map_err(|e| e.to_string())?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let s = hex(&b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    ))
}

fn derive_key(oauth_key_b64: &str) -> Result<[u8; 32], String> {
    let ikm = B64
        .decode(oauth_key_b64.trim())
        .map_err(|e| format!("OAUTH_ENCRYPTION_KEY 非法 base64：{e}"))?;
    let hk = Hkdf::<Sha256>::new(Some(&[]), &ikm);
    let mut out = [0u8; 32];
    hk.expand(HKDF_INFO, &mut out)
        .map_err(|_| "hkdf expand 失败".to_string())?;
    Ok(out)
}

pub fn encrypt_token_v2(plaintext: &[u8], oauth_key_b64: &str) -> Result<String, String> {
    let derived = derive_key(oauth_key_b64)?;
    let iv = rand_bytes(12).map_err(|e| e.to_string())?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&derived));
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&iv),
            Payload { msg: plaintext, aad: AAD },
        )
        .map_err(|_| "aes-gcm 加密失败".to_string())?;
    let mut framed = iv;
    framed.extend_from_slice(&ct);
    Ok(format!("v2:{}", B64.encode(&framed)))
}

pub fn decrypt_token_v2(body: &str, oauth_key_b64: &str) -> Result<Vec<u8>, String> {
    let raw = B64
        .decode(body.strip_prefix("v2:").ok_or("缺 v2: 前缀")?)
        .map_err(|e| format!("v2 体非法 base64：{e}"))?;
    if raw.len() < 12 + 16 {
        return Err("v2 密文过短".into());
    }
    let (iv, rest) = raw.split_at(12);
    let derived = derive_key(oauth_key_b64)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&derived));
    cipher
        .decrypt(
            Nonce::from_slice(iv),
            Payload { msg: rest, aad: AAD },
        )
        .map_err(|_| "aes-gcm 解密或验签失败".to_string())
}

fn safe_write(path: &Path, data: &[u8], mode: u32) -> Result<(), String> {
    if fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return Err(format!("拒绝：{} 是符号链接", path.display()));
    }
    let parent = path.parent().ok_or("目标无父目录")?;
    let suffix = hex(&rand_bytes(6).map_err(|e| e.to_string())?);
    let tmp = parent.join(format!(".tmp-{suffix}"));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)
            .map_err(|e| format!("建临时文件失败：{e}"))?;
        use std::io::Write;
        f.write_all(data).map_err(|e| format!("写临时文件失败：{e}"))?;
    }
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 解析 encryption.key 的 KEY=VALUE 行。
fn parse_key_file(path: &Path) -> BTreeMap<String, String> {
    let mut keys = BTreeMap::new();
    if let Ok(txt) = fs::read_to_string(path) {
        for line in txt.lines() {
            if let Some(eq) = line.find('=') {
                if eq > 0 {
                    let v = line[eq + 1..].trim();
                    if !v.is_empty() {
                        keys.insert(line[..eq].trim().to_string(), v.to_string());
                    }
                }
            }
        }
    }
    keys
}

/// 读取已铸造的稳定虚拟账号标识，供中继应答认证探测使用。
pub fn virtual_ids(app_dir: &Path) -> Option<(String, String)> {
    let txt = fs::read_to_string(app_dir.join("virtual-login.json")).ok()?;
    let v = serde_json::from_str::<serde_json::Value>(&txt).ok()?;
    let a = v["account_uuid"].as_str()?.to_string();
    let o = v["org_uuid"].as_str()?.to_string();
    if a.is_empty() || o.is_empty() {
        return None;
    }
    Some((a, o))
}

/// 稳定的虚拟账号标识存于本程序自己的目录，重铸时沿用，避免旧对话挂不上。
fn stable_ids(app_dir: &Path) -> Result<(String, String), String> {
    let path = app_dir.join("virtual-login.json");
    if let Some(ids) = virtual_ids(app_dir) {
        return Ok(ids);
    }
    let ids = json!({
        "account_uuid": uuid_v4()?,
        "org_uuid": uuid_v4()?,
    });
    fs::create_dir_all(app_dir).map_err(|e| e.to_string())?;
    safe_write(&path, (serde_json::to_string_pretty(&ids).unwrap() + "\n").as_bytes(), 0o600)?;
    Ok((
        ids["account_uuid"].as_str().unwrap().to_string(),
        ids["org_uuid"].as_str().unwrap().to_string(),
    ))
}

/// 幂等保证：目录内令牌可解密且承载的密钥与当前供应商一致则跳过；
/// 密钥变更或令牌缺失损坏时重铸。真实登录的令牌同样会被解密比对，
/// 与供应商密钥不同即被替换，这是切换机制的固有行为。
pub fn ensure_virtual_login(science_dir: &Path, app_dir: &Path, bearer: &str) -> Result<(), String> {
    fs::create_dir_all(science_dir).map_err(|e| format!("数据目录不存在：{e}"))?;

    // encryption.key：复用已有键，缺的补齐
    let key_file = science_dir.join("encryption.key");
    let mut keys = parse_key_file(&key_file);
    let oauth_usable = keys
        .get("OAUTH_ENCRYPTION_KEY")
        .map(|v| B64.decode(v.trim()).map(|b| b.len() >= 16).unwrap_or(false))
        .unwrap_or(false);
    if !oauth_usable {
        keys.remove("OAUTH_ENCRYPTION_KEY");
    }
    let mut changed = false;
    for k in KEY_NAMES {
        if !keys.contains_key(k) {
            let v = B64.encode(rand_bytes(32).map_err(|e| e.to_string())?);
            keys.insert(k.to_string(), v);
            changed = true;
        }
    }
    if changed {
        let mut lines: Vec<String> = KEY_NAMES
            .iter()
            .map(|k| format!("{k}={}", keys[*k]))
            .collect();
        for (k, v) in &keys {
            if !KEY_NAMES.contains(&k.as_str()) {
                lines.push(format!("{k}={v}"));
            }
        }
        safe_write(&key_file, (lines.join("\n") + "\n").as_bytes(), 0o600)?;
    }
    let oauth_key = keys
        .get("OAUTH_ENCRYPTION_KEY")
        .ok_or("缺 OAUTH_ENCRYPTION_KEY")?
        .clone();

    // 已有恰好一枚可解密的令牌则不动，真实登录同样命中此分支
    let tok_dir = science_dir.join(".oauth-tokens");
    fs::create_dir_all(&tok_dir).map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(&tok_dir, fs::Permissions::from_mode(0o700));
    let encs: Vec<PathBuf> = fs::read_dir(&tok_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "enc").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    if encs.len() == 1 {
        if let Ok(body) = fs::read_to_string(&encs[0]) {
            if let Ok(plain) = decrypt_token_v2(&body, &oauth_key) {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&plain) {
                    if v.get("access_token").and_then(|t| t.as_str()) == Some(bearer) {
                        return Ok(());
                    }
                }
            }
        }
    }

    // 铸虚拟令牌；access_token 承载供应商密钥，使模型列表与推理共用同一凭证
    let (account_uuid, org_uuid) = stable_ids(app_dir)?;
    let access = if bearer.is_empty() {
        format!(
            "sk-ant-virtual-{}",
            hex(&rand_bytes(24).map_err(|e| e.to_string())?)
        )
    } else {
        bearer.to_string()
    };
    let blob = json!({
        "access_token": access,
        "refresh_token": "",
        "api_key": null,
        "token_expires_at": "2099-01-01T00:00:00.000Z",
        "provider": "claude_ai",
        "scopes": "user:inference user:file_upload user:profile user:mcp_servers user:plugins",
        "email": FAKE_EMAIL,
        "account_uuid": account_uuid,
        "subscription_type": "max",
        "rate_limit_tier": null,
        "seat_tier": null,
        "org_uuid": org_uuid,
        "billing_type": null,
        "has_extra_usage_enabled": false
    });
    let plaintext = serde_json::to_vec(&blob).map_err(|e| e.to_string())?;
    let enc_body = encrypt_token_v2(&plaintext, &oauth_key)?;

    for p in &encs {
        fs::remove_file(p).map_err(|e| {
            format!("删除旧令牌 {} 失败：{e}（目录内须恰好一个 .enc）", p.display())
        })?;
    }
    let user_id: String = account_uuid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    safe_write(&tok_dir.join(format!("{user_id}.enc")), enc_body.as_bytes(), 0o600)?;

    let org_json = serde_json::to_string_pretty(&json!({ "org_uuid": org_uuid })).unwrap() + "\n";
    safe_write(&science_dir.join("active-org.json"), org_json.as_bytes(), 0o600)?;

    // 自校验：确保守护进程能解开
    let roundtrip = decrypt_token_v2(&enc_body, &oauth_key)?;
    let rt: serde_json::Value = serde_json::from_slice(&roundtrip).map_err(|e| e.to_string())?;
    if rt.get("account_uuid").and_then(|v| v.as_str()) != Some(account_uuid.as_str()) {
        return Err("自校验失败：解密回读的账号不符".into());
    }
    Ok(())
}
