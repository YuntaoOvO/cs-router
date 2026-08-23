# CS Router

![CS Router](ui/banner.png)

claude-science 的供应商路由器。一个窗口、一个卡片列表、一个系统托盘、一个本地中继，
不依赖 claude.ai 登录即可使用第三方供应商推理。

[English](README.md)

## 架构树

```
cs-router/
├── src-tauri/
│   ├── src/
│   │   ├── main.rs          # 存储、托盘、窗口、命令、生命周期
│   │   ├── relay.rs         # 本地中继：目录应答、角色映射、转发
│   │   ├── oauth_forge.rs   # 虚拟登录：铸造本地 OAuth 令牌
│   │   └── model_fetch.rs   # 模型列表获取，候选地址规则
│   ├── icons/
│   │   ├── icon.png         # 应用图标 512px 圆角矩形
│   │   └── tray.png         # 托盘图标 22px 圆角矩形
│   ├── Cargo.toml
│   ├── tauri.conf.json
│   └── capabilities/
│       └── default.json     # 窗口权限
├── ui/
│   ├── index.html           # 单页布局
│   ├── main.js              # 状态、渲染、乐观切换
│   ├── style.css            # 主题令牌，Anthropic 衬线加 MiSans
│   ├── logo.png             # 品牌标识
│   └── banner.png           # 发版横幅
├── LICENSE                  # MIT
└── README.md
```

## 构建

系统依赖：`libwebkit2gtk-4.1-dev`、`build-essential`；打包 deb 另需
`libayatana-appindicator3-dev`、`librsvg2-dev`。

```sh
cargo install tauri-cli --locked
cd src-tauri && cargo tauri build
```

## 工作原理

**存储** — `~/.cs-router/config.json`，权限 0600。每个供应商含名称、备注、官网、
接口地址、认证字段、上游格式、密钥、默认模型、模型目录、角色映射。
接口地址为空即官方登录条目，可编辑可删除，清单清空后下次启动重新播种。

**虚拟登录** — `oauth_forge.rs` 在 `~/.claude-science` 内以本地 `encryption.key`
经 HKDF 派生 AES-256-GCM 密钥铸造远期过期的 OAuth 令牌，令牌的 `access_token`
承载当前供应商密钥。网页端认证状态因此常为已登录，与 claude.ai 网络完全解耦。

**本地中继** — `relay.rs` 固定监听 127.0.0.1:39171。守护进程的接口地址指向它：
模型清单请求按登记目录以 `claude-csswitch-` 选择器标识应答，界面只显示登记模型；
消息请求读体重写模型名后转发真实供应商并回流流式响应。模型名解析三级：
选择器表命中、claude 家族名按角色映射、未配置角色回落默认模型。

**切换** — 点击即生效。未运行则以新供应商拉起；运行中则停止后以新环境重启。
全部后台串行，界面乐观更新零等待。

## 声明

本项目是社区开源工具，与 Anthropic、Claude 及任何模型供应商无关联、无合作、未获授权。
Claude、Anthropic 及各供应商名称与商标归其各自所有者，本项目仅在描述性意义上引用。

虚拟登录机制仅在本机数据目录内铸造本地令牌，用于界面认证状态标记，
不伪造、不截获、不存储任何真实账号凭证。使用者应自行确认并遵守所用供应商
与服务端的服务条款；因使用本工具产生的任何后果由使用者自行承担。

本项目不收集、不上传任何数据。供应商密钥仅存储于本机 `~/.cs-router/config.json`，
权限 0600，请妥善保管。

## License

MIT
