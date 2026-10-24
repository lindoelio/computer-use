# AGENTS.md

Repository context and working rules for agents contributing to `computer-use`.

## What this repository is

`computer-use` is cross-platform desktop automation for agents: screenshots, mouse, keyboard, scrolling, displays, accessibility trees, OCR, CDP browser control, isolation, and a permission doctor. It ships as one Rust binary and as a Rust library (`computer_use`). The binary and the library share the same dispatch path, so an action behaves the same through `exec`, `serve`, stdin, or the library API.

Every action writes one JSON object to stdout with a trailing newline. See `README.md` for the user-facing surface and `docs/CLI.md` for the full command, flag, action, and error-code reference.

## Technology stack

- Rust 2021 edition; minimum supported Rust version 1.91 (`package.rust-version` in `Cargo.toml`).
- CLI: `clap` 4 derive. JSON: `serde` / `serde_json`. Errors: `thiserror`.
- Desktop: `xcap` (capture), `enigo` (foreground input), `image` (encode), plus `dirs`, `toml`, and `base64`.
- Optional features, all off by default:
  - `cdp` — `chromiumoxide` 0.9.1 plus a minimal `tokio` and `futures` for Chrome DevTools browser control. On in the release binaries.
  - `portal` — `ashpd`, `pipewire`, and `reis` on Linux for Wayland capture and input. On in the Linux release binary.
  - `skylight` — macOS private focus path behind `--tier synthetic`.
  - `virtual-display` — macOS private `CGVirtualDisplay` probe. Probe only; it never creates a display.
- Platform-gated dependencies: the `objc2*` crates on macOS, `windows` / `uiautomation` / `windows-capture` on Windows, `atspi` / `x11rb` on Linux.

## Repository layout

- `src/` — library and binary. `src/lib.rs` re-exports the public surface; `src/main.rs` is the clap CLI; `src/protocol.rs` holds `Action`, `Response`, and `dispatch`; `src/backend/` holds the `DesktopBackend` trait and the macOS, Linux, and Windows backends; `src/browser/`, `src/vision/`, and `src/adapters/` hold the CDP, OCR/marks, and OpenAI/Anthropic translation modules.
- `tests/` — integration tests and JSON fixtures.
- `docs/` — CLI reference, isolation, packaging, coverage policy, SkyLight notes, and the historical 0.2.0 roadmap. Only documents named in `Cargo.toml`'s `include` list ship in the crate tarball.
- `scripts/` — macOS app packaging, the release guard, and the coverage gate.
- `packaging/macos/` — `Info.plist` and entitlements for `Computer Use.app`.
- `.github/workflows/` — CI, release, and crates.io publishing.

## Commands

Local checks, as documented in `README.md`:

```bash
cargo fmt --all
cargo build
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

CI uses stable Rust and runs nextest, doctests, the default-feature build, and the Linux `portal` compile check. The nextest command requires cargo-nextest to be installed:

```bash
cargo check --all-targets
cargo check --features portal --all-targets   # Linux only
cargo test --doc --all-features
cargo nextest run --all-targets --all-features
```

Linux build prerequisites (Debian/Ubuntu):

```bash
sudo apt-get install -y pkg-config libxdo-dev libx11-dev libxcb1-dev \
  libxrandr-dev libdbus-1-dev libxkbcommon-dev libatspi2.0-dev \
  libpipewire-0.3-dev libclang-dev clang
```

Coverage (requires cargo-llvm-cov, Python 3, and Chrome/Chromium; the macOS job enforces the floor). See [coverage setup](docs/COVERAGE.md) for installation:

```bash
rustup component add llvm-tools-preview
export COMPUTER_USE_REQUIRE_LIVE=1
cargo llvm-cov --all-targets --all-features --json --output-path cov.json
python3 scripts/coverage-gate.py cov.json --os macos
```

## Agent constraints

- Never run tests that move the pointer, capture the screen, or require Screen Recording or Accessibility. The suite is designed to run on CI runners without a desktop session.
- Tests that touch the cache, policy, audit, or pause paths must point `XDG_CACHE_HOME` / `XDG_STATE_HOME` / `XDG_CONFIG_HOME` at a per-test temp directory. A test that reaches the session cache without `XDG_CACHE_HOME` panics instead of writing `~/.cache/computer-use`.
- Preserve the current interface boundaries: `serve` is stdin/stdout, not a TCP or MCP service; the overlay helper is a local Unix socket. Do not add a network listener as an incidental part of another change.
- Keep off-platform behavior honest: every command exists in the clap tree on every OS and returns `unsupported` with the OS name when the backend cannot run it.
- Keep `docs/CLI.md` in sync with the binary. `tests/cli_doc_matches_help.rs` fails when a documented action or subcommand is not reachable.
- Keep the crate-root `#![deny(unsafe_code)]` attributes. Scope exceptions to the platform FFI or test helper that needs them; see [STYLEGUIDE.md](STYLEGUIDE.md#unsafe-code).
- Prefer `ComputerError` variants over panics. `ComputerError::code()` is the stable JSON contract, and existing codes must not change meaning.
- Do not log typed text, element values, clipboard bytes, or blocked URLs. The audit writer never records them.
- Add code comments only when they are highly necessary to explain non-obvious intent, workarounds, or critical constraints. Preserve API documentation and `SAFETY` explanations; see [STYLEGUIDE.md](STYLEGUIDE.md#documentation-comments).

## Behavioral Guidelines

1. Think Before Coding. Don't assume. Don't hide confusion. Surface tradeoffs.
2. Simplicity First. Minimum code that solves the problem. Nothing speculative. Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.
3. Surgical Changes. Touch only what you must. Clean up only your own mess.
4. Goal-Driven Execution. Strong success criteria let you loop independently. Weak criteria ("make it work") require clarification.

## Mandatory Self-Review Gate (Pre-Handoff)

Before marking any task as complete or handing it off to a reviewer agent, you are strictly required to run an internal self-audit and cleanup cycle. Never deliver your raw first draft.

Pause, inspect your work against the 4 pillars below, and fix any issues immediately:

1. Zero Execution Debris (Clean Up Your Tracks): Delete all temporary files, scratchpads, one-off test scripts, logs, or unrequested summary notes created to aid your reasoning during execution. Leave only the requested final artifacts behind.
2. Clean Replacement (Forward-Only): When modifying existing work (including your own output from earlier steps), replace the old version completely. Leave no dead code, orphaned text, unnecessary backward-compatibility layers, or "just-in-case" fallbacks unless explicitly instructed to do so.
3. Cohesion & Reuse: Ensure you have not duplicated logic, patterns, or content that already exists in the current workspace. Reuse established modules, styles, and conventions rather than reinventing them locally.
4. Target the Source of Truth: Verify that your changes and validations/tests target the primary source files directly—never compiled, transpiled, generated, or secondary outputs.

**Handoff Rule:** If you detect any violation of the points above during your check, fix it silently within your current loop. Only conclude your turn or invoke the next agent once all 4 checks pass cleanly.

## Related documents

- `README.md` — install, usage, permissions, and the action table.
- `CONTRIBUTING.md` — workflow, repository structure, and the release process.
- `STYLEGUIDE.md` — code conventions.
- `TESTING.md` — test strategy and commands.
- `ARCHITECTURE.md` — system structure and data flow.
- `SECURITY.md` — security model and reporting.
- `docs/CLI.md` — full CLI and JSON reference.
- `docs/COVERAGE.md` — coverage policy and exclusions.
