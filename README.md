# Sudo Code

<p align="center">
  <img src="assets/scode-hero.jpeg" alt="Sudo Code" width="300" />
</p>

**Sudo Code** is a high-performance, autonomous-first AI agent engine written in Rust. It is designed to act as a robust, machine-readable "Operating System" for AI coding swarms.

Originally forked from [ultraworkers/claw-code](https://github.com/ultraworkers/claw-code) (last synced: 2026-04-23), Sudo Code has been evolved to support the **Agent Communication Protocol (ACP)** and is the core engine powering the **Sudowork** platform.

## Key Features

- **Native Performance**: Written in Rust for near-instant boot times and minimal resource footprint.
- **Machine-First (ACP)**: Supports headless JSON-RPC integration via `scode acp` for seamless use in IDEs and GUIs.
- **Autonomous-Ready**: Built-in state machine and Lane Event system for reliable, multi-agent coordination.
- **Production Safety**: Strict permission gating, path traversal prevention, and Linux-native sandboxing.

## Quick Start

```bash
# Build the engine
cd rust
cargo build --workspace --release

# Run a health check
./target/release/scode doctor

# Start a headless ACP server
./target/release/scode acp
```

## Documentation

- [Rust Workspace](./rust/README.md) — Crate architecture and internals.

---

### Ownership / Affiliation Disclaimer
- Sudo Code is a community-driven port and does **not** claim ownership of the original Claude Code source material.
- This repository is **not affiliated with, endorsed by, or maintained by Anthropic**.
