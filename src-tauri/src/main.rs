//! CS Router：claude-science 的供应商路由器。
//! 数据在 ~/.cs-router/config.json，对 claude-science 的写入限于
//! ~/.claude-science/config.toml 的 default_model 字段，其余一切保持官方默认。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod model_fetch;
mod oauth_forge;
mod relay;

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Emitter, Manager, WindowEvent,
    menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
};

const OFFICIAL_ID: &str = "official";

/// claude 家族角色映射：实际请求模型加界面显示名称，均空表示未映射
#[derive(Serialize, Deserialize, Clone, Default)]
struct RoleBinding {
    #[serde(default)]
    model: String,
    #[serde(default)]
    display: String,
}

/// claude 家族角色到上游模型的映射，空模型表示回退默认模型
#[derive(Serialize, Deserialize, Clone, Default)]
struct RoleBindings {
    #[serde(default)]
    sonnet: RoleBinding,
    #[serde(default)]
    opus: RoleBinding,
    #[serde(default)]
    haiku: RoleBinding,
    #[serde(default)]
    fable: RoleBinding,
}

impl RoleBindings {
    /// 序列化为 角色 → (模型, 显示名)，模型为空的条目剔除
    fn to_map(&self) -> std::collections::BTreeMap<String, (String, String)> {
        let mut m = std::collections::BTreeMap::new();
        for (role, b) in [
            ("sonnet", &self.sonnet),
            ("opus", &self.opus),
            ("haiku", &self.haiku),
            ("fable", &self.fable),
        ] {
            if !b.model.trim().is_empty() {
                m.insert(role.to_string(), (b.model.trim().to_string(), b.display.trim().to_string()));
            }
        }
        m
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Provider {
    id: String,
    name: String,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    website: String,
    base_url: String,
    key_type: String,
    #[serde(default)]
    api_format: String,
    api_key: String,
    model: String,
    #[serde(default)]
    models: Vec<String>,
    models_url: String,
    #[serde(default)]
    roles: RoleBindings,
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone)]
struct Settings {
    close_action: String,
    autostart: bool,
    fast_fail: bool,
    #[serde(default)]
    daemon_proxy: String,
    /// 应用级窗口按钮：开启用自建最小化、最大化、关闭，关闭沿用系统窗口
    #[serde(default = "default_true")]
    custom_controls: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            close_action: "tray".into(),
            autostart: false,
            fast_fail: false,
            daemon_proxy: String::new(),
            custom_controls: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Config {
    version: u32,
    current: String,
    providers: Vec<Provider>,
    settings: Settings,
}

#[derive(Serialize, Clone, Default)]
struct StatusInfo {
    running: bool,
    port: String,
}

#[derive(Serialize, Clone)]
struct StateResp {
    providers: Vec<Provider>,
    current: String,
    settings: Settings,
    status: StatusInfo,
}

struct AppState {
    cfg: Mutex<Config>,
    tray_ok: Mutex<bool>,
    status: Mutex<StatusInfo>,
    relay: Option<relay::Relay>,
    /// 串行化后台的守护进程重启，快速连点切换按顺序收敛到最后一次
    restart: Mutex<()>,
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn app_dir() -> PathBuf {
    home_dir().join(".cs-router")
}

fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

fn science_config_toml() -> PathBuf {
    home_dir().join(".claude-science").join("config.toml")
}

fn official_provider() -> Provider {
    Provider {
        id: OFFICIAL_ID.to_string(),
        name: "Claude Official".to_string(),
        notes: String::new(),
        website: String::new(),
        base_url: String::new(),
        key_type: String::new(),
        api_format: "anthropic".to_string(),
        api_key: String::new(),
        model: String::new(),
        models: vec![],
        models_url: String::new(),
        roles: RoleBindings::default(),
    }
}

fn default_config() -> Config {
    Config {
        version: 1,
        current: OFFICIAL_ID.to_string(),
        providers: vec![official_provider()],
        settings: Settings::default(),
    }
}

fn new_id() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("p{:x}", ms)
}

fn save_config(cfg: &Config) -> Result<(), String> {
    fs::create_dir_all(app_dir()).map_err(|e| e.to_string())?;
    let _ = fs::set_permissions(app_dir(), fs::Permissions::from_mode(0o700));
    let text = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    fs::write(config_path(), text).map_err(|e| e.to_string())?;
    fs::set_permissions(config_path(), fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 首次运行时从 ~/.csswitch/config.json 读取旧供应商。
/// claude-science 只讲 Anthropic 协议，api_format 非 anthropic 的条目导入后无法工作，跳过。
fn import_legacy() -> Option<(Vec<Provider>, String)> {
    let path = home_dir().join(".csswitch").join("config.json");
    let text = fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let profiles = v.get("profiles")?.as_array()?;
    let mut out: Vec<Provider> = vec![];
    for pf in profiles {
        if pf.get("api_format").and_then(|f| f.as_str()) != Some("anthropic") {
            continue;
        }
        let name = pf.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let base_url = pf.get("base_url").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        if name.is_empty() || base_url.is_empty() {
            continue;
        }
        let api_key = pf.get("api_key").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let id = pf
            .get("id")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(new_id);
        let route = pf.get("default_model_route_id").and_then(|x| x.as_str());
        let model = pf
            .get("model_catalog")
            .and_then(|c| c.as_array())
            .and_then(|cat| {
                cat.iter()
                    .find(|m| m.get("selector_id").and_then(|s| s.as_str()) == route)
                    .or_else(|| cat.first())
            })
            .and_then(|m| m.get("upstream_model").and_then(|u| u.as_str()))
            .unwrap_or("")
            .to_string();
        out.push(Provider {
            id,
            name,
            notes: String::new(),
            website: String::new(),
            base_url,
            key_type: "api_key".to_string(),
            api_format: "anthropic".to_string(),
            api_key,
            model: model.clone(),
            models: if model.is_empty() { vec![] } else { vec![model] },
            models_url: String::new(),
            roles: RoleBindings::default(),
        });
    }
    let current = v
        .get("active_id")
        .and_then(|a| a.as_str())
        .and_then(|a| out.iter().find(|p| p.id == a))
        .map(|p| p.id.clone())
        .unwrap_or_else(|| OFFICIAL_ID.to_string());
    Some((out, current))
}

fn load_config() -> Config {
    if let Ok(text) = fs::read_to_string(config_path()) {
        // 旧结构的角色映射是字符串，迁移为 模型加显示名 的对象
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(providers) = v.get_mut("providers").and_then(|p| p.as_array_mut()) {
                for p in providers.iter_mut() {
                    if let Some(roles) = p.get_mut("roles").and_then(|r| r.as_object_mut()) {
                        for (_role, val) in roles.iter_mut() {
                            if let Some(s) = val.as_str().map(|x| x.to_string()) {
                                *val = serde_json::json!({ "model": s, "display": "" });
                            }
                        }
                    }
                }
            }
            if let Ok(mut cfg) = serde_json::from_value::<Config>(v) {
                let mut dirty = false;
                // 官方种子只在清单为空时播种；用户删除即消失，全删后下次启动重新播种
                if cfg.providers.is_empty() {
                    cfg.providers.insert(0, official_provider());
                    dirty = true;
                }
                // 存量官方条目沿用默认旧名的换成英文名，用户自改过的不动
                for p in cfg.providers.iter_mut() {
                    if p.id == OFFICIAL_ID && p.name == "官方登录" {
                        p.name = "Claude Official".to_string();
                        dirty = true;
                    }
                }
                // 单模型旧结构迁移：models 为空时以默认模型补齐
                for p in cfg.providers.iter_mut() {
                    if p.models.is_empty() && !p.model.is_empty() {
                        p.models = vec![p.model.clone()];
                        dirty = true;
                    }
                }
                if dirty {
                    let _ = save_config(&cfg);
                }
                return cfg;
            }
        }
    }
    let mut cfg = default_config();
    if let Some((imported, current)) = import_legacy() {
        cfg.providers.extend(imported);
        cfg.current = current;
    }
    let _ = save_config(&cfg);
    cfg
}

/// 维护 ~/.claude-science/config.toml 顶层的 default_model 字段，保留文件其余内容。
/// 只认第一个表头之前的顶层键，避免误改任何小节内的同名字段。
fn set_default_model(model: Option<&str>) -> Result<(), String> {
    let path = science_config_toml();
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    let section_start = lines
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let key_line = lines[..section_start].iter().position(|l| {
        let t = l.trim_start();
        t.starts_with("default_model") && t["default_model".len()..].trim_start().starts_with('=')
    });
    match (model, key_line) {
        (Some(m), Some(i)) => lines[i] = format!("default_model = \"{}\"", m),
        (Some(m), None) => {
            let mut insert_at = 0;
            for (i, l) in lines.iter().enumerate().take(section_start) {
                let t = l.trim();
                if !t.is_empty() && !t.starts_with('#') {
                    insert_at = i + 1;
                }
            }
            lines.insert(insert_at, format!("default_model = \"{}\"", m));
        }
        (None, Some(i)) => {
            lines.remove(i);
        }
        (None, None) => {}
    }
    let mut out = lines.join("\n");
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    fs::write(&path, out).map_err(|e| e.to_string())
}

fn host_of(url: &str) -> String {
    let u = url.trim();
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .unwrap_or(u);
    rest.split(['/', ':']).next().unwrap_or("").to_string()
}

fn query_status() -> StatusInfo {
    let out = Command::new("claude-science").arg("status").output();
    match out {
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            if let Some(start) = text.find('{') {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[start..]) {
                    return StatusInfo {
                        running: v["running"].as_bool().unwrap_or(false),
                        port: v["port"].as_u64().map(|p| p.to_string()).unwrap_or_default(),
                    };
                }
            }
            StatusInfo::default()
        }
        Err(_) => StatusInfo::default(),
    }
}

/// 回收退出的子进程，避免僵尸进程堆积。
fn reap(mut child: Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn spawn_serve(
    provider: &Provider,
    settings: &Settings,
    base_override: Option<&str>,
) -> Result<(), String> {
    let mut cmd = Command::new("claude-science");
    // 分离模式已在真实环境验证：环境变量会完整传递给守护进程
    cmd.arg("serve")
        .arg("--detached")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // 与 cc-switch 同判：接口地址为空即官方，走登录态与官方接口
    let is_official = provider.base_url.trim().is_empty();
    if is_official {
        set_default_model(None)?;
        cmd.env_remove("ANTHROPIC_BASE_URL");
        cmd.env_remove("ANTHROPIC_API_KEY");
        cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
    } else {
        set_default_model(Some(&provider.model))?;
        let base = base_override.unwrap_or(&provider.base_url);
        cmd.env("ANTHROPIC_BASE_URL", base);
        if provider.key_type == "auth_token" {
            cmd.env("ANTHROPIC_AUTH_TOKEN", &provider.api_key);
        } else {
            cmd.env("ANTHROPIC_API_KEY", &provider.api_key);
        }
    }
    // 代理设置优先：claude.ai 登录与令牌交换需要可达的出口，
    // 供应商主机加入 no_proxy 保持推理直连
    let proxy = settings.daemon_proxy.trim().to_string();
    if !proxy.is_empty() {
        let mut no_proxy = String::from("127.0.0.1,localhost,::1");
        if !is_official {
            no_proxy.push_str(&format!(",{}", host_of(&provider.base_url)));
        }
        cmd.env("http_proxy", &proxy)
            .env("https_proxy", &proxy)
            .env("HTTP_PROXY", &proxy)
            .env("HTTPS_PROXY", &proxy)
            .env("no_proxy", &no_proxy)
            .env("NO_PROXY", &no_proxy);
    } else if settings.fast_fail {
        if is_official {
            cmd.env("https_proxy", "http://127.0.0.1:9")
                .env("no_proxy", "127.0.0.1,localhost,::1");
        } else {
            // 快速失败只针对启动时对 claude.ai 的组织探测，
            // 推理流量仍需直连供应商，否则全部外联都会撞上无监听的代理端口
            cmd.env("https_proxy", "http://127.0.0.1:9").env(
                "no_proxy",
                format!("127.0.0.1,localhost,::1,{}", host_of(&provider.base_url)),
            );
        }
    }
    cmd.process_group(0);
    let child = cmd
        .spawn()
        .map_err(|e| format!("启动 claude-science 失败：{}", e))?;
    reap(child);
    Ok(())
}

fn stop_daemon_blocking() -> Result<(), String> {
    let out = Command::new("claude-science")
        .arg("stop")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if msg.is_empty() { "停止失败".to_string() } else { msg });
    }
    Ok(())
}

fn open_ui_cmd() -> Result<(), String> {
    let mut cmd = Command::new("claude-science");
    cmd.arg("open")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let child = cmd.spawn().map_err(|e| format!("打开界面失败：{}", e))?;
    reap(child);
    Ok(())
}

fn apply_autostart(enabled: bool) -> Result<(), String> {
    let path = home_dir().join(".config").join("autostart").join("cs-router.desktop");
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let body = format!(
            "[Desktop Entry]\nType=Application\nName=CS Router\nExec={}\n",
            exe.display()
        );
        fs::write(&path, body).map_err(|e| e.to_string())?;
    } else {
        let _ = fs::remove_file(&path);
    }
    Ok(())
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let state = app.state::<AppState>();
    let cfg = state.cfg.lock().unwrap().clone();
    drop(state);
    let mut switch_items: Vec<MenuItem<tauri::Wry>> = vec![];
    for p in &cfg.providers {
        // 官方式条目不常用于日常切换，撤出托盘菜单以免误触；主界面仍可切换
        if p.base_url.trim().is_empty() {
            continue;
        }
        let label = if p.id == cfg.current {
            format!("✓ {}", p.name)
        } else {
            p.name.clone()
        };
        switch_items.push(MenuItem::with_id(
            app,
            format!("switch:{}", p.id),
            label,
            true,
            None::<&str>,
        )?);
    }
    let show = MenuItem::with_id(app, "act:show", "打开主界面", true, None::<&str>)?;
    let lite = MenuItem::with_id(app, "act:lite", "轻量模式", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "act:quit", "退出", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let mut refs: Vec<&dyn IsMenuItem<tauri::Wry>> = vec![];
    refs.push(&show);
    refs.push(&sep1);
    for it in &switch_items {
        refs.push(it);
    }
    refs.push(&sep2);
    refs.push(&lite);
    refs.push(&quit);
    Menu::with_items(app, &refs)
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    let menu = build_menu(app)?;
    TrayIconBuilder::with_id("main")
        .icon(icon)
        .tooltip("CS Router")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| {
            let id = event.id().0.clone();
            handle_tray_action(app, &id);
        })
        .build(app)?;
    Ok(())
}

fn rebuild_tray(app: &AppHandle) {
    if let Some(tray) = app.tray_by_id("main") {
        if let Ok(menu) = build_menu(app) {
            let _ = tray.set_menu(Some(menu));
        }
    } else {
        let _ = build_tray(app);
    }
}

fn handle_tray_action(app: &AppHandle, id: &str) {
    if let Some(pid) = id.strip_prefix("switch:") {
        // 菜单事件在主线程派发，切换的阻塞流程放后台线程，避免冻结界面
        let a = app.clone();
        let pid = pid.to_string();
        std::thread::spawn(move || {
            let _ = do_switch(&a, &pid);
        });
        return;
    }
    match id {
        "act:show" => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        // 轻量模式：仅驻留托盘，主窗口隐藏
        "act:lite" => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.hide();
            }
        }
        "act:quit" => app.exit(0),
        _ => {}
    }
}

fn find_provider<'a>(cfg: &'a Config, id: &str) -> Option<&'a Provider> {
    cfg.providers.iter().find(|p| p.id == id)
}

fn do_launch(app: &AppHandle) -> Result<(), String> {
    let (provider, settings, relay_port) = {
        let state = app.state::<AppState>();
        let cfg = state.cfg.lock().unwrap().clone();
        let provider = find_provider(&cfg, &cfg.current)
            .cloned()
            .ok_or_else(|| "当前供应商不存在".to_string())?;
        let is_official = provider.base_url.trim().is_empty();
        let port = state.relay.as_ref().map(|r| {
            // 中继目录有且只含切换器登记的模型；消息路径按选择器映射后转发
            if !is_official {
                let catalog = if provider.models.is_empty() {
                    vec![provider.model.clone()]
                } else {
                    provider.models.clone()
                };
                r.set_target(relay::RelayTarget {
                    upstream: relay::normalize_upstream(&provider.base_url),
                    default_model: provider.model.clone(),
                    catalog,
                    roles: provider.roles.to_map(),
                });
            }
            r.port
        });
        (provider, cfg.settings, port)
    };
    // 虚拟登录幂等保证：令牌承载供应商密钥，模型列表与推理共用同一凭证
    let is_official = provider.base_url.trim().is_empty();
    let bearer = if is_official { String::new() } else { provider.api_key.clone() };
    if let Err(e) = oauth_forge::ensure_virtual_login(&home_dir().join(".claude-science"), &app_dir(), &bearer) {
        eprintln!("虚拟登录检查失败：{e}");
    }
    let base_override = if is_official {
        None
    } else {
        relay_port.map(|p| format!("http://127.0.0.1:{p}"))
    };
    spawn_serve(&provider, &settings, base_override.as_deref())?;
    // 状态回填放后台延迟刷新，拉起路径不阻塞任何人
    let a = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1500));
        update_status(&a);
        std::thread::sleep(Duration::from_millis(3500));
        update_status(&a);
    });
    Ok(())
}

fn do_switch(app: &AppHandle, id: &str) -> Result<(), String> {
    // 快路径：保存、重建托盘、广播，命令立即返回，界面零等待
    {
        let state = app.state::<AppState>();
        let mut cfg = state.cfg.lock().unwrap();
        if !cfg.providers.iter().any(|p| p.id == id) {
            return Err("供应商不存在".to_string());
        }
        cfg.current = id.to_string();
        save_config(&cfg)?;
        drop(cfg);
        drop(state);
    }
    rebuild_tray(app);
    emit_state(app);
    // 慢路径：守护进程停止与重启全部后台串行，连点切换按序收敛到最后选择
    let a = app.clone();
    std::thread::spawn(move || {
        let st = a.state::<AppState>();
        let _guard = st.restart.lock().unwrap();
        let running = a.state::<AppState>().status.lock().unwrap().running;
        if running {
            let _ = stop_daemon_blocking();
        }
        if let Err(e) = do_launch(&a) {
            eprintln!("切换拉起失败：{e}");
        }
    });
    Ok(())
}

fn update_status(app: &AppHandle) {
    let st = query_status();
    {
        let state = app.state::<AppState>();
        *state.status.lock().unwrap() = st.clone();
    }
    let _ = app.emit("status", &st);
    if let Some(tray) = app.tray_by_id("main") {
        let tip = if st.running {
            if st.port.is_empty() {
                "CS Router · 运行中".to_string()
            } else {
                format!("CS Router · 运行中 · 端口 {}", st.port)
            }
        } else {
            "CS Router · 已停止".to_string()
        };
        let _ = tray.set_tooltip(Some(tip));
    }
}

fn emit_state(app: &AppHandle) {
    let state = app.state::<AppState>();
    let cfg = state.cfg.lock().unwrap().clone();
    let status = state.status.lock().unwrap().clone();
    drop(state);
    let _ = app.emit(
        "state",
        StateResp {
            providers: cfg.providers,
            current: cfg.current,
            settings: cfg.settings,
            status,
        },
    );
}

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> StateResp {
    let cfg = state.cfg.lock().unwrap().clone();
    let status = state.status.lock().unwrap().clone();
    StateResp {
        providers: cfg.providers,
        current: cfg.current,
        settings: cfg.settings,
        status,
    }
}

#[tauri::command]
async fn switch_provider(app: AppHandle, id: String) -> Result<(), String> {
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || do_switch(&a, &id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn launch_daemon(app: AppHandle) -> Result<(), String> {
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || do_launch(&a))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn stop_daemon(app: AppHandle) -> Result<(), String> {
    let a = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let r = stop_daemon_blocking();
        update_status(&a);
        r
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn open_ui() -> Result<(), String> {
    open_ui_cmd()
}

#[tauri::command]
async fn refresh_status(app: AppHandle) -> StatusInfo {
    let a = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || update_status(&a)).await;
    app.state::<AppState>().status.lock().unwrap().clone()
}

#[tauri::command]
fn save_provider(
    app: AppHandle,
    state: tauri::State<AppState>,
    provider: Provider,
) -> Result<(), String> {
    let mut p = provider;
    if p.id.trim().is_empty() {
        p.id = new_id();
    }
    {
        let mut cfg = state.cfg.lock().unwrap();
        match cfg.providers.iter().position(|x| x.id == p.id) {
            Some(i) => cfg.providers[i] = p.clone(),
            None => cfg.providers.push(p),
        }
        save_config(&cfg)?;
    }
    drop(state);
    rebuild_tray(&app);
    emit_state(&app);
    Ok(())
}

#[tauri::command]
fn delete_provider(app: AppHandle, state: tauri::State<AppState>, id: String) -> Result<(), String> {
    {
        let mut cfg = state.cfg.lock().unwrap();
        cfg.providers.retain(|p| p.id != id);
        if cfg.current == id {
            cfg.current = cfg
                .providers
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_else(|| OFFICIAL_ID.to_string());
        }
        save_config(&cfg)?;
    }
    drop(state);
    rebuild_tray(&app);
    emit_state(&app);
    Ok(())
}

/// 供应商连通测试：请求模型列表候选端点，返回耗时与模型数
#[tauri::command]
async fn test_provider(provider: Provider) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let start = std::time::Instant::now();
        match model_fetch::fetch_models(
            &provider.base_url,
            &provider.api_key,
            &provider.key_type,
            &provider.models_url,
        ) {
            Ok(ids) => Ok(format!("{} ms · {} 个模型", start.elapsed().as_millis(), ids.len())),
            Err(e) => Err(e),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn fetch_models(
    base_url: String,
    api_key: String,
    key_type: String,
    models_url: String,
) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        model_fetch::fetch_models(&base_url, &api_key, &key_type, &models_url)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn save_settings(
    app: AppHandle,
    state: tauri::State<AppState>,
    settings: Settings,
) -> Result<(), String> {
    let (autostart, custom_controls) = {
        let mut cfg = state.cfg.lock().unwrap();
        cfg.settings = settings;
        save_config(&cfg)?;
        (cfg.settings.autostart, cfg.settings.custom_controls)
    };
    drop(state);
    apply_autostart(autostart)?;
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_decorations(!custom_controls);
    }
    emit_state(&app);
    Ok(())
}

/// 应用级窗口按钮的动作：最小化、最大化切换、关闭遵循关闭行为设置
#[tauri::command]
fn window_control(app: AppHandle, action: String) -> Result<(), String> {
    let Some(w) = app.get_webview_window("main") else {
        return Ok(());
    };
    match action.as_str() {
        "min" => {
            let _ = w.minimize();
        }
        "max" => {
            if w.is_maximized().unwrap_or(false) {
                let _ = w.unmaximize();
            } else {
                let _ = w.maximize();
            }
        }
        "close" => {
            let state = app.state::<AppState>();
            let settings = state.cfg.lock().unwrap().settings.clone();
            let tray_ok = *state.tray_ok.lock().unwrap();
            drop(state);
            if tray_ok && settings.close_action == "tray" {
                let _ = w.hide();
            } else {
                app.exit(0);
            }
        }
        _ => {}
    }
    Ok(())
}

fn ensure_single_instance() -> io::Result<()> {
    fs::create_dir_all(app_dir())?;
    let file = File::create(app_dir().join("lock"))?;
    file.try_lock()?;
    std::mem::forget(file);
    Ok(())
}

/// 更名迁移：旧 ~/.cs-switch 一次性整体搬至 ~/.cs-router
fn migrate_old_dir() {
    let old = home_dir().join(".cs-switch");
    let new = home_dir().join(".cs-router");
    if old.exists() && !new.exists() {
        let _ = fs::rename(&old, &new);
    }
}

/// Wayland 的 dock 依桌面项匹配图标，X11 依窗口属性；
/// 自注册两者都覆盖：图标入 hicolor 主题，桌面项带 StartupWMClass。
fn install_desktop_entry() {
    let icon_dir = home_dir()
        .join(".local/share/icons/hicolor/512x512/apps");
    if fs::create_dir_all(&icon_dir).is_ok() {
        let _ = fs::write(icon_dir.join("cs-router.png"), include_bytes!("../icons/icon.png"));
    }
    let apps_dir = home_dir().join(".local/share/applications");
    if fs::create_dir_all(&apps_dir).is_ok() {
        let exec = std::env::current_exe()
            .map(|p| format!("\"{}\"", p.display()))
            .unwrap_or_else(|_| "cs-router".to_string());
        let desk = format!(
            "[Desktop Entry]\nType=Application\nName=CS Router\nExec={exec}\nIcon=cs-router\nStartupWMClass=cs-router\nCategories=Utility;\n"
        );
        let _ = fs::write(apps_dir.join("cs-router.desktop"), desk);
        // 清理旧名桌面项与图标
        let _ = fs::remove_file(apps_dir.join("cs-switch.desktop"));
        let _ = fs::remove_file(home_dir().join(".local/share/icons/hicolor/512x512/apps/cs-switch.png"));
        // 刷新图标主题缓存与应用数据库，dock 即时取到新图标
        let _ = Command::new("gtk-update-icon-cache")
            .args(["-f", "-t", &home_dir().join(".local/share/icons/hicolor").display().to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("update-desktop-database")
            .arg(&apps_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// WebKitGTK 会把页面资产缓存到磁盘，嵌入资产更新后窗口仍读旧缓存；
/// 启动时清掉缓存目录，保证界面始终随二进制更新。
fn clear_webview_cache() {
    let base = home_dir().join(".local/share/io.github.yuntao.cs-router");
    for dir in ["WebKitCache", "CacheStorage"] {
        let _ = fs::remove_dir_all(base.join(dir));
    }
}

fn main() {
    migrate_old_dir();
    clear_webview_cache();
    if ensure_single_instance().is_err() {
        eprintln!("CS Router 已在运行");
        return;
    }
    let cfg = load_config();
    let _ = apply_autostart(cfg.settings.autostart);
    // 启动即按当前供应商保证虚拟登录在场，切换与拉起服务时同样幂等检查
    let cur = find_provider(&cfg, &cfg.current).cloned();
    let bearer = cur
        .filter(|p| p.id != OFFICIAL_ID)
        .map(|p| p.api_key)
        .unwrap_or_default();
    if let Err(e) = oauth_forge::ensure_virtual_login(&home_dir().join(".claude-science"), &app_dir(), &bearer) {
        eprintln!("虚拟登录检查失败：{e}");
    }

    // 中继与主程序同生命周期：模型清单在本地应答，推理 307 直连供应商
    install_desktop_entry();
    let mut relay_handle = relay::start();
    if let Some(r) = relay_handle.as_ref() {
        if let Some(p) = find_provider(&cfg, &cfg.current) {
            if !p.base_url.trim().is_empty() {
                let catalog = if p.models.is_empty() { vec![p.model.clone()] } else { p.models.clone() };
                r.set_target(relay::RelayTarget {
                    upstream: relay::normalize_upstream(&p.base_url),
                    default_model: p.model.clone(),
                    catalog,
                    roles: p.roles.to_map(),
                });
            }
        }
    }

    tauri::Builder::default()
        .manage(AppState {
            cfg: Mutex::new(cfg),
            tray_ok: Mutex::new(false),
            status: Mutex::new(StatusInfo::default()),
            relay: relay_handle.take(),
            restart: Mutex::new(()),
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let state = app.state::<AppState>();
                let settings = state.cfg.lock().unwrap().settings.clone();
                let tray_ok = *state.tray_ok.lock().unwrap();
                drop(state);
                if tray_ok && settings.close_action == "tray" {
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    // 守护进程与图形界面生死无关，退出图形界面时保持其运行
                    app.exit(0);
                }
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            // 启动即按设置应用窗口模式：应用级按钮或系统窗口
            if let Some(w) = app.get_webview_window("main") {
                let custom = app.state::<AppState>().cfg.lock().unwrap().settings.custom_controls;
                let _ = w.set_decorations(!custom);
                if let Ok(img) = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png")) {
                    let _ = w.set_icon(img);
                }
            }
            let ok = build_tray(&handle).is_ok();
            *app.state::<AppState>().tray_ok.lock().unwrap() = ok;
            std::thread::spawn({
                let h = handle.clone();
                move || loop {
                    std::thread::sleep(Duration::from_secs(5));
                    update_status(&h);
                }
            });
            update_status(&handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            switch_provider,
            launch_daemon,
            stop_daemon,
            open_ui,
            refresh_status,
            save_provider,
            delete_provider,
            fetch_models,
            save_settings,
            test_provider,
            window_control
        ])
        .run(tauri::generate_context!())
        .expect("启动失败");
}
