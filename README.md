# Kimi GUI (Fork)

> **Attribution:** This is a fork of the original [KimiCode-GUI](https://github.com/ZacharyZhang-NY/KimiCode-GUI) created by [@ZacharyZhang-NY](https://github.com/ZacharyZhang-NY). **99% of this project was built by the original creator.** This fork contains minor bug fixes for CLI session history loading on macOS. All credit for the design, architecture, and implementation goes to the original author.

---

Desktop UI for Kimi Code CLI built with Rust + Tauri 2 and a static HTML/CSS/JS
frontend. The GUI embeds the real `kimi` CLI inside a
PTY-powered terminal to keep feature parity with the CLI.

## Fork Changes

This fork includes the following fixes:

- **Fixed session directory hashing** - Changed from Rust's `DefaultHasher` to MD5 to match the Kimi CLI's session directory lookup
- **Fixed wire.jsonl parsing** - Updated to handle the actual nested message format (`message.type`, `message.payload.user_input`, etc.)
- **Fixed work directory detection** - Now correctly uses the `PWD` environment variable to determine the working directory when launching
- **Improved session filtering** - Only displays sessions that have actual chat content (non-empty `wire.jsonl` files)

## Prerequisites

- Node.js 18+
- Rust 1.75+
- Tauri CLI 2.x (via npm dev dependency or `cargo install tauri-cli --version "^2.0.0" --locked`)

## Development

```bash
npm install
npm run build
npm run tauri dev
```

If the CLI binary is not on PATH, set the command explicitly:

```bash
KIMI_GUI_COMMAND="python -m kimi_cli" npm run tauri dev
```

`npm run build` validates the static UI assets (no bundler required).

## Build

```bash
npm run tauri build
```

## Notes

- The Tauri config lives in `src-tauri/tauri.conf.json`.
- The Rust backend exposes PTY commands and config helpers used by the UI.
- The GUI searches for `kimi`/`kimi-cli` on PATH, then falls back to `python -m kimi_cli`.
- Use the Control Center to configure models, skills, MCP, and login, or set
  `KIMI_GUI_COMMAND` to override the launch command.
