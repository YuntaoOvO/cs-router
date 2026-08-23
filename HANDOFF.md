# HANDOFF：CS Router 项目交接

日期：2026-08-23
状态：v1.0.0 已发版，CI 流水线运行中。本文件是全部有效结论与教训的交接，下一位执行者从这里继续。

## 项目是什么

CS Router 是 claude-science 的供应商路由器，替代旧的 CSSwitch。功能：卡片列表点切换即生效、虚拟登录免 claude.ai 登录、本地中继控制模型目录与角色映射、系统托盘驻留、连通测试、JSON 配置实时写穿。

仓库：https://github.com/YuntaoOvO/cs-router
发版：https://github.com/YuntaoOvO/cs-router/releases
项目目录：`~/cs-router`
二进制安装位置：`~/.local/bin/cs-router`
配置目录：`~/.cs-router/config.json`（权限 0600，含供应商密钥）

## 技术栈与文件结构

Tauri 2 + Rust 后端 + 原生 HTML/CSS/JS 前端（无框架无构建步骤）。

```
~/cs-router/
├── src-tauri/
│   ├── src/
│   │   ├── main.rs          # 存储、托盘、窗口、命令、生命周期
│   │   ├── relay.rs         # 本地中继：127.0.0.1:39171，目录应答+角色映射+转发
│   │   ├── oauth_forge.rs   # 虚拟登录：铸造本地 OAuth 令牌
│   │   └── model_fetch.rs   # 模型列表获取，候选地址规则
│   ├── icons/icon.png       # 512px 圆角矩形
│   ├── icons/tray.png       # 22px 圆角矩形
│   ├── Cargo.toml           # 包名 cs-router，版本 1.0.0
│   ├── tauri.conf.json      # identifier io.github.yuntao.cs-router
│   ├── capabilities/default.json
│   └── build.rs             # 监听 ../ui/ 变化触发重新嵌入
├── ui/
│   ├── index.html           # 单页布局
│   ├── main.js              # 状态、渲染、乐观切换
│   ├── style.css            # 主题令牌
│   ├── logo.png             # 品牌标识
│   └── banner.png           # 发版横幅
├── .github/workflows/build.yml  # CI：三平台构建+自动发版
├── README.md                # 英文
├── README_ZH.md             # 中文
└── LICENSE                  # MIT
```

## 核心机制

### 虚拟登录（oauth_forge.rs）

claude-science 的网页端认证只认磁盘上的加密 OAuth 令牌文件（`.oauth-tokens/*.enc`），环境变量密钥只进推理通道，对认证状态接口无效（源码标注 no env-key fallback）。

我们在启动时铸造一枚本地令牌：
- 读取 `~/.claude-science/encryption.key` 的 `OAUTH_ENCRYPTION_KEY`
- HKDF-SHA256 派生 AES-256-GCM 密钥（info=`operon:aes-256-gcm:oauth`，AAD=`v2:oauth`）
- 令牌 JSON 含 `access_token`（承载当前供应商密钥）、远期过期时间 `2099-01-01`
- 写入 `.oauth-tokens/` 下恰好一个 `.enc` 文件（多了会导致登录失败）
- 令牌的 `access_token` 就是供应商密钥，模型列表请求也用它认证

### 本地中继（relay.rs）

固定监听 `127.0.0.1:39171`。claude-science 的 `ANTHROPIC_BASE_URL` 指向它。

- GET `/v1/models`：按登记目录以 `claude-csswitch-` 前缀选择器标识应答，界面只显示登记模型
- POST `/v1/messages` 等：读体解析 JSON、重写 `model` 字段后转发到真实供应商，流式响应透传
- 模型名解析三级：选择器表命中 → claude 家族名按角色映射（sonnet/opus/haiku/fable）→ 回落默认模型
- 供应商地址归一：剥掉尾部 `/v1`（守护进程请求自带 `/v1` 前缀）

### 切换流程

点切换按钮 → 保存 config.json + 重建托盘 + 广播（快路径，立即返回）→ 后台线程串行停止旧守护进程、铸造新令牌、以新环境启动 serve（慢路径）→ 界面乐观更新，零等待。

## 踩过的坑（绝对不要再踩）

1. **WebKitGTK 磁盘缓存**：页面资产缓存在 `~/.local/share/io.github.yuntao.cs-router/WebKitCache` 与 `CacheStorage`，二进制更新后窗口仍读旧缓存。程序启动时必须先删这两个目录（已实现于 `clear_webview_cache()`）。曾导致多轮界面改动看似无效。

2. **`<form>` 内按钮隐式提交**：编辑页表单装在 `<form>` 元素里，预设模板等按钮没加 `type="button"` 会触发表单提交导致页面刷新回主页。所有表单内按钮必须显式 `type="button"`，并在 form 上兜底 `preventDefault`。

3. **窗口拖拽**：`data-tauri-drag-region` 属性必须挂在接收 mousedown 的元素上。子元素会拦截事件，需加 `pointer-events: none` 让事件穿透到带属性的容器。编辑页整窗被遮罩铺满时，遮罩内各级容器都需要加该属性。最终方案：挂在 `<body>` 上，空白处全部可拖。

4. **Tauri 前端资产嵌入**：前端目录在 crate 外（`../ui/`），`build.rs` 必须显式声明 `cargo:rerun-if-changed`，否则改了前端文件 cargo 不会重新嵌入。

5. **cargo clean 后目录改名**：项目目录从 `~/cs-switch` 改为 `~/cs-router` 后，target 目录里的绝对路径缓存导致构建失败，必须 `cargo clean` 全量重建。

6. **圆角蒙版方向**：ImageMagick 的 `roundrectangle` 默认黑色填充，在白色 alpha 提取图上画黑矩形导致蒙版反转（中间透明、四角保留）。正确做法：`xc:black -fill white -draw 'roundrectangle ...'`。

7. **GNOME 启动台**：桌面项指向开发路径不可靠，程序启动时自动把二进制复制到 `~/.local/bin/`，桌面项 `Exec` 指向该标准路径。

8. **GitHub 推送网络**：直连 github.com 常超时或 GnuTLS 错误，需走本地代理 `git config http.proxy http://127.0.0.1:7890`。

9. **claude-science 升级后兼容性**：从 0.1.25 升到 0.1.27 后虚拟登录、中继、映射全部正常，无需改动。但升级前要停守护进程再 `claude-science update`。

10. **配置迁移**：`~/.cs-switch` → `~/.cs-router` 的迁移在程序启动时一次性 `fs::rename`，只在旧目录存在且新目录不存在时执行。

## 已验证的事实

- serve 默认端口 8000，`claude-science status` 输出 JSON 含 `running` 与 `port`
- serve 以 `--detached` 启动，环境变量完整传递给守护进程
- 界面模型清单数据源只认登录令牌，因此目录必须由中继应答
- 登录链路依赖 claude.ai 授权页与 platform.claude.com 令牌交换，虚拟登录使两者不再必要
- 模型列表候选规则：地址以版本段结尾时候选为地址加 `/models`；其余情况地址加 `/v1/models`；以兼容子路径结尾时追加剥掉子路径后的根地址候选
- 兼容子路径清单：`/api/claudecode`、`/api/anthropic`、`/apps/anthropic`、`/api/coding`、`/claudecode`、`/anthropic`、`/step_plan`、`/coding`、`/claude`

## CI 发版流程

`.github/workflows/build.yml` 已配置：
- 打 tag（如 `v1.0.0`）推送后自动触发
- 三条流水线并行：Linux deb、macOS ARM dmg、macOS Intel dmg
- 构建完成后 release job 自动 `gh release create` 建发版页并附三个安装包
- 产物命名：`cs-router-linux-amd64.deb`、`cs-router-macos-aarch64.dmg`、`cs-router-macos-x86_64.dmg`
- macOS dmg 未签名，用户首次打开需右键选"打开"

## 下一步：模型路由

用户明确要求的下一个功能方向是**模型路由**——在多个供应商之间按规则分发请求，类似于 OpenRouter 的路由能力。可能的方向：

1. **故障转移**：主供应商失败时自动切到备用供应商
2. **按模型路由**：不同模型名路由到不同供应商（如 claude-* 走 A、gpt-* 走 B）
3. **负载均衡**：多密钥轮询同一供应商
4. **成本感知**：按 token 单价选最便宜的可用供应商

当前中继的模型名解析已有三级（选择器→角色→默认），扩展为多供应商路由需要：
- Provider 增加 `fallback` 或 `routing_rules` 字段
- relay.rs 支持按规则选择目标 upstream 而非单一目标
- 界面增加路由规则编辑

## 当前环境

- OS：Ubuntu（Linux 7.0.0-29-generic x64）
- Rust：1.93.1
- claude-science：0.1.27
- Node：v24.19.0
- 已装 dev 包：libwebkit2gtk-4.1-dev、libayatana-appindicator3-dev、librsvg2-dev
- 代理：mihomo 127.0.0.1:7890（git 已配置）
- 字体：Anthropic Serif Text + MiSans（用户 ~/.local/share/fonts/）
- gh CLI 已登录 YuntaoOvO
