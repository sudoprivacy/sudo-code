# Sudo Code

<p align="center">
  <img src="assets/scode-hero.jpeg" alt="Sudo Code" width="300" />
</p>

**Sudo Code** (`scode`) is an AI coding agent engine written in Rust. Fast boot, headless ACP support, multi-provider auth.

Originally forked from [ultraworkers/claw-code](https://github.com/ultraworkers/claw-code) (last synced: 2026-04-23), Sudo Code has been evolved to support the **Agent Communication Protocol (ACP)** and is the core engine powering the **Sudowork** platform.

## Quick Start

```bash
cd rust
cargo build --release

# Set your credentials (pick one)
export ANTHROPIC_API_KEY="sk-ant-..."        # direct API key
export CLAUDE_CODE_OAUTH_TOKEN="sk-ant-oat-..." # subscription token
# or use a proxy:
export PROXY_AUTH_TOKEN="your-token"
export PROXY_BASE_URL="https://your-proxy.com"

# Interactive REPL
./target/release/scode

# One-shot prompt
./target/release/scode "explain this codebase"

# Health check
./target/release/scode doctor
```

## Authentication

Use `--auth` to explicitly select an auth mode:

```bash
scode --auth api-key          # uses ANTHROPIC_API_KEY, OPENAI_API_KEY, etc.
scode --auth subscription     # uses CLAUDE_CODE_OAUTH_TOKEN
scode --auth proxy            # uses PROXY_AUTH_TOKEN + PROXY_BASE_URL
```

When `--auth` is omitted, auto-detection applies: subscription > proxy > api-key.

| Mode | Env vars | Endpoint |
|------|----------|----------|
| `api-key` | `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, `DASHSCOPE_API_KEY` | Provider default |
| `subscription` | `CLAUDE_CODE_OAUTH_TOKEN` (run `claude setup-token` to get one) | api.anthropic.com |
| `proxy` | `PROXY_AUTH_TOKEN` + `PROXY_BASE_URL` | `PROXY_BASE_URL` |

## Model Aliases

```bash
scode --model opus      # claude-opus-4-6
scode --model sonnet    # claude-sonnet-4-6
scode --model haiku     # claude-haiku-4-5
scode --model grok      # grok-3 (xAI)
```

## Documentation

- [Usage Guide](./USAGE.md) — Commands, integration, local models
- [Rust Workspace](./rust/README.md) — Crate architecture and internals

---

Sudo Code is a community-driven project. Not affiliated with or endorsed by Anthropic.
