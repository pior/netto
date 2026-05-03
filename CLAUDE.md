# Netto

A lightweight macOS menu bar app for monitoring internet connectivity health, built in Rust with native AppKit bindings.

## Build & Test

```
cargo build
cargo test
```

## Architecture

- `main.rs` — App delegate, status item, dropdown menu, timer (10s refresh)
- `ping.rs` — Ping execution and output parsing

Key dependencies: `objc2` / `objc2-app-kit` (native AppKit), system `ping` command for connectivity checks.

## Design Principles

- No webview, Electron, or heavy runtime.
- Native Rust binary using `objc2` / `objc2-app-kit` for all macOS APIs.
- Menu bar-only app (no dock icon, `LSUIElement = true`).
- Minimal resource usage between refresh intervals.
