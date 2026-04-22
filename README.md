# Sudo Code

<p align="center">
  <a href="https://github.com/ultraworkers/sudo-code">ultraworkers/sudo-code</a>
  ·
  <a href="./USAGE.md">Usage</a>
  ·
  <a href="./rust/README.md">Rust workspace</a>
  ·
  <a href="./PARITY.md">Parity</a>
  ·
  <a href="./ROADMAP.md">Roadmap</a>
  ·
  <a href="https://discord.gg/5TUQKqFWd">UltraWorkers Discord</a>
</p>

<p align="center">
  <a href="https://star-history.com/#ultraworkers/sudo-code&Date">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=ultraworkers/sudo-code&type=Date&theme=dark" />
      <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=ultraworkers/sudo-code&type=Date" />
      <img alt="Star history for ultraworkers/sudo-code" src="https://api.star-history.com/svg?repos=ultraworkers/sudo-code&type=Date" width="600" />
    </picture>
  </a>
</p>

<p align="center">
  <img src="assets/scode-hero.jpeg" alt="Sudo Code" width="300" />
</p>

Sudo Code is the public Rust implementation of the `scode` CLI agent harness.
The canonical implementation lives in [`rust/`](./rust), and the current source of truth for this repository is **ultraworkers/sudo-code**.

> [!IMPORTANT]
> Start with [`USAGE.md`](./USAGE.md) for build, auth, CLI, session, and parity-harness workflows. Make `scode doctor` your first health check after building, use [`rust/README.md`](./rust/README.md) for crate-level details, read [`PARITY.md`](./PARITY.md) for the current Rust-port checkpoint, and see [`docs/container.md`](./docs/container.md) for the container-first workflow.
>
> **ACP / Zed status:** `sudo-code` does not ship an ACP/Zed daemon entrypoint yet. Run `scode acp` (or `scode --acp`) for the current status instead of guessing from source layout; `scode acp serve` is currently a discoverability alias only, and real ACP support remains tracked separately in `ROADMAP.md`.

## Current repository shape

- **`rust/`** — canonical Rust workspace and the `scode` CLI binary
- **`USAGE.md`** — task-oriented usage guide for the current product surface
- **`PARITY.md`** — Rust-port parity status and migration notes
- **`ROADMAP.md`** — active roadmap and cleanup backlog
- **`PHILOSOPHY.md`** — project intent and system-design framing
- **`src/` + `tests/`** — companion Python/reference workspace and audit helpers; not the primary runtime surface

## Quick start

> [!NOTE]
> [!WARNING]
> **`cargo install sudo-code` installs the wrong thing.** The `sudo-code` crate on crates.io is a deprecated stub that places `sudo-code-deprecated.exe` — not `scode`. Running it only prints `"sudo-code has been renamed to agent-code"`. **Do not use `cargo install sudo-code`.** Either build from source (this repo) or install the upstream binary:
> ```bash
> cargo install agent-code   # upstream binary — installs 'agent.exe' (Windows) / 'agent' (Unix), NOT 'agent-code'
> ```
> This repo (`ultraworkers/sudo-code`) is **build-from-source only** — follow the steps below.

```bash
# 1. Clone and build
git clone https://github.com/ultraworkers/sudo-code
cd sudo-code/rust
cargo build --workspace

# 2. Set your API key (Anthropic API key — not a Claude subscription)
export ANTHROPIC_API_KEY="sk-ant-..."

# 3. Verify everything is wired correctly
./target/debug/scode doctor

# 4. Run a prompt
./target/debug/scode prompt "say hello"
```

> [!NOTE]
> **Windows (PowerShell):** the binary is `scode.exe`, not `scode`. Use `.\target\debug\scode.exe` or run `cargo run -- prompt "say hello"` to skip the path lookup.

### Windows setup

**PowerShell is a supported Windows path.** Use whichever shell works for you. The common onboarding issues on Windows are:

1. **Install Rust first** — download from <https://rustup.rs/> and run the installer. Close and reopen your terminal when it finishes.
2. **Verify Rust is on PATH:**
   ```powershell
   cargo --version
   ```
   If this fails, reopen your terminal or run the PATH setup from the Rust installer output, then retry.
3. **Clone and build** (works in PowerShell, Git Bash, or WSL):
   ```powershell
   git clone https://github.com/ultraworkers/sudo-code
   cd sudo-code/rust
   cargo build --workspace
   ```
4. **Run** (PowerShell — note `.exe` and backslash):
   ```powershell
   $env:ANTHROPIC_API_KEY = "sk-ant-..."
   .\target\debug\scode.exe prompt "say hello"
   ```

**Git Bash / WSL** are optional alternatives, not requirements. If you prefer bash-style paths (`/c/Users/you/...` instead of `C:\Users\you\...`), Git Bash (ships with Git for Windows) works well. In Git Bash, the `MINGW64` prompt is expected and normal — not a broken install.

## Post-build: locate the binary and verify

After running `cargo build --workspace`, the `scode` binary is built but **not** automatically installed to your system. Here's where to find it and how to verify the build succeeded.

### Binary location

After `cargo build --workspace` in `sudo-code/rust/`:

**Debug build (default, faster compile):**
- **macOS/Linux:** `rust/target/debug/scode`
- **Windows:** `rust/target/debug/scode.exe`

**Release build (optimized, slower compile):**
- **macOS/Linux:** `rust/target/release/scode`
- **Windows:** `rust/target/release/scode.exe`

If you ran `cargo build` without `--release`, the binary is in the `debug/` folder.

### Verify the build succeeded

Test the binary directly using its path:

```bash
# macOS/Linux (debug build)
./rust/target/debug/scode --help
./rust/target/debug/scode doctor

# Windows PowerShell (debug build)
.\rust\target\debug\scode.exe --help
.\rust\target\debug\scode.exe doctor
```

If these commands succeed, the build is working. `scode doctor` is your first health check — it validates your API key, model access, and tool configuration.

### Optional: Add to PATH

If you want to run `scode` from any directory without the full path, choose one of these approaches:

**Option 1: Symlink (macOS/Linux)**
```bash
ln -s $(pwd)/rust/target/debug/scode /usr/local/bin/scode
```
Then reload your shell and test:
```bash
scode --help
```

**Option 2: Use `cargo install` (all platforms)**

Build and install to Cargo's default location (`~/.cargo/bin/`, which is usually on PATH):
```bash
# From the sudo-code/rust/ directory
cargo install --path . --force

# Then from anywhere
scode --help
```

**Option 3: Update shell profile (bash/zsh)**

Add this line to `~/.bashrc` or `~/.zshrc`:
```bash
export PATH="$(pwd)/rust/target/debug:$PATH"
```

Reload your shell:
```bash
source ~/.bashrc  # or source ~/.zshrc
scode --help
```

### Troubleshooting

- **"command not found: scode"** — The binary is in `rust/target/debug/scode`, but it's not on your PATH. Use the full path `./rust/target/debug/scode` or symlink/install as above.
- **"permission denied"** — On macOS/Linux, you may need `chmod +x rust/target/debug/scode` if the executable bit isn't set (rare).
- **Debug vs. release** — If the build is slow, you're in debug mode (default). Add `--release` to `cargo build` for faster runtime, but the build itself will take 5–10 minutes.

> [!NOTE]
> **Auth:** scode requires an **API key** (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.) — Claude subscription login is not a supported auth path.

Run the workspace test suite after verifying the binary works:

```bash
cd rust
cargo test --workspace
```

## Documentation map

- [`USAGE.md`](./USAGE.md) — quick commands, auth, sessions, config, parity harness
- [`rust/README.md`](./rust/README.md) — crate map, CLI surface, features, workspace layout
- [`PARITY.md`](./PARITY.md) — parity status for the Rust port
- [`rust/MOCK_PARITY_HARNESS.md`](./rust/MOCK_PARITY_HARNESS.md) — deterministic mock-service harness details
- [`ROADMAP.md`](./ROADMAP.md) — active roadmap and open cleanup work
- [`PHILOSOPHY.md`](./PHILOSOPHY.md) — why the project exists and how it is operated

## Ecosystem

Sudo Code is built in the open alongside the broader UltraWorkers toolchain:

- [sudocodehip](https://github.com/Yeachan-Heo/scodehip)
- [oh-my-openagent](https://github.com/code-yeongyu/oh-my-openagent)
- [oh-my-claudecode](https://github.com/Yeachan-Heo/oh-my-claudecode)
- [oh-my-codex](https://github.com/Yeachan-Heo/oh-my-codex)
- [UltraWorkers Discord](https://discord.gg/5TUQKqFWd)

## Ownership / affiliation disclaimer

- This repository does **not** claim ownership of the original Claude Code source material.
- This repository is **not affiliated with, endorsed by, or maintained by Anthropic**.
