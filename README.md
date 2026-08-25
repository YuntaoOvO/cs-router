# CS Router

![CS Router](ui/banner.png)

A provider router for claude-science. One window, one card list, one system tray, one local relay — no claude.ai login required for third-party inference.

[中文](README_ZH.md)

## Install

Download the latest `.deb` from [Releases](https://github.com/YuntaoOvO/cs-router/releases) and install:

```sh
sudo dpkg -i cs-router_*.deb
cs-router
```

On first launch, CS Router starts the claude-science daemon automatically (unless Manual Daemon Mode is on).

## How it works

```
claude-science daemon
  ANTHROPIC_BASE_URL=http://127.0.0.1:39171
        │
        ▼
  CS Router relay (port 39171)
  ┌─────────────────────────────────────────────┐
  │  GET /v1/models  → answer from local catalog │
  │  POST /v1/messages → rewrite model field     │
  │                    → forward to provider     │
  │  GET /api/oauth/profile → local 200 response │
  └─────────────────────────────────────────────┘
        │
        ▼
  Third-party provider (DeepSeek / OpenRouter / etc.)
```

**Storage** — `~/.cs-router/config.json` (0600). Each provider carries: name, notes, website, endpoint, key type, API key, upstream format, default model, model catalog, and role mappings (sonnet / opus / haiku / fable → real upstream model). Empty endpoint means the official claude.ai entry.

**Virtual login** — `oauth_forge.rs` forges a local OAuth token inside `~/.claude-science` using HKDF-SHA256 + AES-256-GCM against the existing `encryption.key`. The token carries the current provider's API key and expires in 2099. claude-science's web UI stays authenticated without any real claude.ai account.

**Local relay** — `relay.rs` binds to `127.0.0.1:39171`. Model catalog requests are answered locally using `claude-csswitch-` selector IDs, so the web UI shows exactly the models you registered. Inference requests have their `model` field rewritten then forwarded to the real provider; streaming responses pass through. Model resolution: selector table → claude-family role mapping (sonnet/opus/haiku/fable) → fallback to default model.

**Authentication decoupling** — CS Router writes `claude_ai_base_url = "http://127.0.0.1:39171"` into `~/.claude-science/config.toml [update]`, so the daemon's `/api/oauth/profile` probe hits the relay instead of claude.ai. Login state no longer depends on claude.ai reachability or the validity of a virtual token against the real API.

**Switching** — Click to switch. CS Router saves config, rebuilds tray, updates the relay target immediately, then in the background stops the old daemon and launches with the new environment. UI updates optimistically with zero wait.

**Cold start** — On launch, if the daemon is not running, CS Router starts it automatically for the current provider. No "switch-then-switch-back" ritual needed.

**Daemon status bar** — The main window shows a live status indicator (green = running, red = stopped) with Launch / Stop buttons. Tray menu mirrors this.

**Manual Daemon Mode** — Enable in Settings to stop CS Router from auto-managing the daemon. Switching only updates the relay and config; you run `claude-science serve` yourself.

## Build

```sh
# System deps (Debian/Ubuntu)
sudo apt install libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev build-essential

cargo install tauri-cli --locked
cargo tauri build          # run from repo root
```

## File layout

```
cs-router/
├── src-tauri/src/
│   ├── main.rs          # Config, tray, window, commands, lifecycle
│   ├── relay.rs         # Local relay: catalog, role mapping, forwarding
│   ├── oauth_forge.rs   # Virtual login token forge
│   └── model_fetch.rs   # Model list fetch with candidate URL rules
├── ui/
│   ├── index.html       # Single-page layout
│   ├── main.js          # State, rendering, optimistic switching
│   └── style.css        # Theme tokens (light/dark)
└── .github/workflows/build.yml   # CI: Linux deb + macOS dmg
```

## Disclaimer

Community open-source tool, not affiliated with or endorsed by Anthropic or any model provider. All trademarks belong to their respective owners.

The virtual login mechanism forges a local token within the machine's own data directory as a UI authentication marker only. It does not intercept or store real credentials. Users are responsible for their providers' terms of service.

No data collection. Keys stored only in `~/.cs-router/config.json` (0600).

## License

MIT
