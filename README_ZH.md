# CS Router

![CS Router](ui/banner.png)

claude-science 的供应商路由器。一个窗口、一个卡片列表、一个系统托盘、一个本地中继——
不依赖 claude.ai 登录即可使用第三方推理。

[English](README.md)

## 安装

从 [Releases](https://github.com/YuntaoOvO/cs-router/releases) 下载最新 `.deb` 安装：

```sh
sudo dpkg -i cs-router_*.deb
cs-router
```

首次启动时，CS Router 会自动拉起 claude-science 守护进程（除非开启了手动模式）。

## 工作原理

```
claude-science 守护进程
  ANTHROPIC_BASE_URL=http://127.0.0.1:39171
        │
        ▼
  CS Router 中继（端口 39171）
  ┌─────────────────────────────────────────────┐
  │  GET /v1/models  → 本地目录应答              │
  │  POST /v1/messages → 重写模型字段            │
  │                    → 转发到供应商            │
  │  GET /api/oauth/profile → 本地 200 虚拟应答  │
  └─────────────────────────────────────────────┘
        │
        ▼
  第三方供应商（DeepSeek / OpenRouter 等）
```

**存储** — `~/.cs-router/config.json`，权限 0600。每个供应商含名称、备注、官网、接口地址、认证字段类型、密钥、上游格式、默认模型、模型目录、角色映射（sonnet / opus / haiku / fable → 真实上游模型）。接口地址为空即官方 claude.ai 条目。

**虚拟登录** — `oauth_forge.rs` 在 `~/.claude-science` 内以本地 `encryption.key` 经 HKDF-SHA256 派生 AES-256-GCM 密钥铸造 OAuth 令牌，令牌的 `access_token` 承载当前供应商密钥，过期时间 2099 年。网页端认证状态始终为已登录，与 claude.ai 账号完全解耦。

**本地中继** — `relay.rs` 固定监听 `127.0.0.1:39171`。模型清单请求按登记目录以 `claude-csswitch-` 选择器标识本地应答，界面只显示登记的模型。消息请求读体重写 `model` 字段后转发真实供应商，流式响应透传。模型名解析三级：选择器表命中 → claude 家族角色映射 → 回落默认模型。

**认证探测解耦** — CS Router 启动时幂等写入 `~/.claude-science/config.toml` 的 `[update]` 段 `claude_ai_base_url`，令守护进程的 `/api/oauth/profile` 探测打到中继而非 claude.ai。登录态不再受 claude.ai 可达性或虚拟令牌有效性影响。

**切换** — 点击即生效。CS Router 立即保存配置、重建托盘、更新中继目标，后台停止旧守护进程并以新环境重启。界面乐观更新，零等待。

**冷启动自动拉起** — 打开 CS Router 时，若守护进程未在运行，自动以当前供应商启动它。不再需要"先切换到其他供应商再切回来"的操作。

**守护进程状态栏** — 主界面顶部实时显示守护进程状态（绿点 = 运行中，红点 = 已停止），附启动/停止按钮。托盘菜单同步。

**手动守护进程模式** — 设置中可开启，CS Router 不再自动管理守护进程，切换只更新中继与配置，由用户自行在终端运行 `claude-science serve`。

## 构建

```sh
# 系统依赖（Debian/Ubuntu）
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev build-essential

cargo install tauri-cli --locked
cargo tauri build          # 在仓库根目录运行
```

## 目录结构

```
cs-router/
├── src-tauri/src/
│   ├── main.rs          # 配置、托盘、窗口、命令、生命周期
│   ├── relay.rs         # 本地中继：目录应答、角色映射、转发
│   ├── oauth_forge.rs   # 虚拟登录令牌铸造
│   └── model_fetch.rs   # 模型列表获取，候选地址规则
├── ui/
│   ├── index.html       # 单页布局
│   ├── main.js          # 状态、渲染、乐观切换
│   └── style.css        # 主题令牌（亮色/暗色）
└── .github/workflows/build.yml   # CI：Linux deb + macOS dmg
```

## 声明

本项目是社区开源工具，与 Anthropic、Claude 及任何模型供应商无关联、无合作、未获授权。Claude、Anthropic 及各供应商名称与商标归其各自所有者，本项目仅在描述性意义上引用。

虚拟登录机制仅在本机数据目录内铸造本地令牌，用于界面认证状态标记，不伪造、不截获、不存储任何真实账号凭证。使用者应自行确认并遵守所用供应商的服务条款；因使用本工具产生的任何后果由使用者自行承担。

本项目不收集、不上传任何数据。供应商密钥仅存储于本机 `~/.cs-router/config.json`，权限 0600。

## License

MIT
