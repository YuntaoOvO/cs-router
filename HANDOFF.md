# CS Router 技术文档

版本：v1.1.0  
仓库：https://github.com/YuntaoOvO/cs-router  
项目目录：`~/cs-router`

## 这个项目是什么

CS Router 是 claude-science 的本地供应商路由器。claude-science 的网页 UI 只支持官方 claude.ai 账号登录；CS Router 通过在本地伪造 OAuth 令牌 + 拦截守护进程的 API 流量，使第三方推理供应商（DeepSeek、OpenRouter 等）可以被 claude-science 无感使用。

用户看到的是一个卡片列表，点击切换供应商，底层自动停止并重启守护进程、写入新环境变量。

## 架构

```
claude-science 守护进程
  ANTHROPIC_BASE_URL=http://127.0.0.1:39171
        │
        ▼
  relay.rs（固定端口 39171）
  ├── GET /api/oauth/profile  → 本地固定应答 200，登录态与 claude.ai 解耦
  ├── GET /v1/models          → 本地目录应答，返回 claude-csswitch-{id} 格式
  └── POST /v1/messages 等    → 重写 model 字段 → 转发真实供应商 → 透传流式响应
        │
        ▼
  第三方供应商
```

## 文件结构

```
src-tauri/src/
├── main.rs        — 存储、托盘菜单、窗口、Tauri 命令、生命周期
├── relay.rs       — 本地 HTTP 中继
├── oauth_forge.rs — OAuth 令牌铸造与解密
└── model_fetch.rs — 模型列表获取（候选地址规则）

ui/
├── index.html     — 单页布局，无框架
├── main.js        — 状态管理、渲染、事件绑定
└── style.css      — CSS 变量主题（亮/暗）
```

## 核心机制详解

### 虚拟登录（oauth_forge.rs）

claude-science 的 web 认证只认 `~/.claude-science/.oauth-tokens/*.enc` 磁盘令牌，不认环境变量密钥（源码标注 no env-key fallback）。

铸造流程：
1. 读 `~/.claude-science/encryption.key` 中 `OAUTH_ENCRYPTION_KEY` 字段
2. HKDF-SHA256（info=`operon:aes-256-gcm:oauth`）派生 32 字节 AES-256-GCM 密钥
3. 令牌 JSON 含 `access_token`（当前供应商密钥）、过期 `2099-01-01`、`subscription_type: "max"`
4. AES-256-GCM 加密（AAD=`v2:oauth`），前缀 `v2:`，输出 `{uuid}.enc`
5. `.oauth-tokens/` 目录必须恰好一个 `.enc` 文件，多了会导致登录失败
6. 同时写 `active-org.json` 和 `virtual-login.json`（含 org/account UUID）

幂等条件：令牌的 `access_token` 与当前密钥相同时不重写，避免不必要的磁盘操作。

### 本地中继（relay.rs）

- 固定监听 `127.0.0.1:39171`，被占时退回随机端口
- `set_target()` 更新上游地址、默认模型、目录清单、角色映射
- `set_profile()` 注册 org UUID 用于 `/api/oauth/profile` 应答
- 选择器格式：`claude-csswitch-{净化后的上游模型id}`
- 角色映射额外生成 `claude-csswitch-role-{sonnet|opus|haiku|fable}` 条目（仅有显示名时出现在目录）
- 供应商地址归一：`normalize_upstream()` 剥掉尾部 `/v1`，守护进程请求自带该前缀

三级模型名解析（rewrite_model）：
1. 选择器表命中 → 直接映射
2. 名字含 claude- 前缀 → 识别 sonnet/opus/haiku/fable → 查角色映射
3. 都没中 → 用默认模型

### 认证探测解耦

claude-science 支持 `update.claude_ai_base_url` 覆盖 `/api/oauth/profile` 探测地址。  
CS Router 启动时幂等写入 `~/.claude-science/config.toml`：

```toml
[update]
claude_ai_base_url = "http://127.0.0.1:39171"
```

中继对 `GET /api/oauth/profile` 固定返回：

```json
{"organization":{"uuid":"..."},"enabled_plugins":[]}
```

效果：登录状态不再随 claude.ai 可达性或代理状态随机翻转。

### 守护进程管理

`spawn_serve()` 在启动前：
1. 无条件剥离全部代理环境变量（`http_proxy`、`https_proxy` 等大小写变体）
2. 若非官方供应商，注入 `ANTHROPIC_BASE_URL=http://127.0.0.1:{relay_port}`
3. 注入 `ANTHROPIC_API_KEY` 或 `ANTHROPIC_AUTH_TOKEN`（按 `key_type` 选择）
4. 若配置了 `daemon_proxy`，再注入代理并设 `no_proxy` 排除供应商主机与 127.0.0.1

**不能在终端手动 `claude-science serve`**，除非手动带上正确的环境变量，否则守护进程继承终端代理、探测打到真 claude.ai、虚拟令牌被判 401、UI 弹"session no longer valid"、agent 死亡。这是 v1.0.0 之前的主要事故根源。

### 冷启动与手动模式

`main()` 启动时，若 `!settings.manual_daemon && !query_status().running`，在后台线程直接调用 `spawn_serve()` 启动守护进程，不需要用户点任何按钮。

`manual_daemon = true` 时：
- 冷启动不自动拉起
- `do_switch()` 和 `do_launch()` 都跳过 `spawn_serve()`，只更新中继目标和配置
- 适合希望自己控制守护进程生命周期的用户

## 踩过的坑（绝对不要再踩）

**守护进程继承代理变量**  
在带 `HTTPS_PROXY` 的终端手动 `claude-science serve`，守护进程能通代理访问 claude.ai，虚拟令牌在真 claude.ai 上吃 401，fail-closed，界面弹退登录。`spawn_serve()` 已无条件剥代理变量，但手动终端启动时必须自己清。

**`/api/oauth/profile` 探测与代理叠加**  
即使没有手动代理，claude.ai 不可达时探测超时 fail-open、可达时虚拟令牌 401 fail-closed——两种随机态都无法接受。解法是把探测完全指向中继本地应答，见认证探测解耦一节。

**守护进程缓存旧模型列表**  
切换供应商后，已在运行的守护进程缓存了上一个供应商的模型列表，需重启才能拿到新目录。这是切换时必须 stop-then-relaunch 的原因。

**bundle targets 漏平台**  
`tauri.conf.json` 的 `bundle.targets` 只写 `["deb"]` 会导致 macOS 构建器不产出任何安装包。必须写 `["deb", "dmg", "app"]`。

**CI release job 缺 checkout**  
`gh release create` 需要在 git 仓库内运行，release job 第一步必须 `actions/checkout@v4`。

**WebKitGTK 磁盘缓存**  
页面资产缓存在 `~/.local/share/io.github.yuntao.cs-router/{WebKitCache,CacheStorage}`，二进制更新后仍读旧缓存。`clear_webview_cache()` 在程序启动时删除这两个目录。

**`<form>` 内按钮隐式提交**  
编辑页表单内所有按钮必须显式 `type="button"`，否则触发表单 submit 跳回主页。

**OAuth tokens 目录必须恰好一个 .enc**  
多文件时 claude-science 不知道读哪个，登录失败。`ensure_virtual_login()` 在写新令牌前先删掉所有旧 `.enc`。

**OpenRouter Anthropic 端点**  
`https://openrouter.ai/v1/messages` 返回营销页 HTML（200），真端点是 `https://openrouter.ai/api/v1/messages`。base_url 配 `https://openrouter.ai/api`。

**图标必须 RGBA**  
Tauri `generate_context!` 要求嵌入图标为 RGBA PNG，RGB 会报错。ImageMagick 转换加 `-define png:color-type=6`。

## 已知无害噪音

**每隔数分钟一条 `claudeAiFetch: 401`**  
来自守护进程内另一个硬编码 `api.anthropic.com` 的 caFetch 实例（`config.toml` 只覆盖默认实例），虚拟令牌在真 API 上必然 401。实测非致命，不驱动 UI 横幅或 agent terminal。根除代价大（需等长替换二进制常量或本地 TLS 拦截），暂不做。

**OpenRouter stealth 模型 429**  
`stealth/ox-alpha` 是共享池，高峰期容量 429，守护进程自动退避重试，属供应商侧现象。

## 构建与发布

```sh
# 构建
cargo tauri build

# 安装（先卸载旧版）
sudo dpkg -r cs-router
sudo dpkg -i src-tauri/target/release/bundle/deb/cs-router_*.deb

# 发版（打 tag 推送后 CI 自动构建三平台并发布 release）
git tag v1.1.0 && git push origin v1.1.0
```

CI 产物命名：`cs-router-linux-amd64.deb`、`cs-router-macos-aarch64.dmg`、`cs-router-macos-x86_64.dmg`

## 环境信息

- OS：Ubuntu（Linux 7.0.0-30-generic x64）
- Rust：1.93.1
- claude-science：0.1.27
- 系统 dev 包：libwebkit2gtk-4.1-dev、libayatana-appindicator3-dev、librsvg2-dev
- 代理：mihomo 127.0.0.1:7890（git 已配置 http.proxy）
- 字体：Anthropic Serif Text + MiSans（~/.local/share/fonts/）
- gh CLI 已登录 YuntaoOvO
