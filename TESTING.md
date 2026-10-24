# Testing

How this repository tests itself, and the rules tests must follow. The suite is designed to run on CI runners with no desktop session, no Accessibility or Screen Recording grant, and no user watching.

## Strategy

Confidence comes from layers, with the cost of each layer matched to what it proves:

1. **Unit tests** next to the module they cover. Pure logic — tree rendering, coordinate remapping, policy resolution, adapter translation, key parsing — is tested directly.
2. **Dispatch tests** through `StubBackend` (`src/test_backend.rs`). The `Action` → `Response` path is exercised end to end without touching a real desktop.
3. **Binary integration tests** in `tests/cli.rs`. The compiled binary is run as a subprocess and its JSON output is asserted; no test moves the pointer or captures the screen.
4. **Documentation-binding tests** in `tests/cli_doc_matches_help.rs`. `docs/CLI.md` is parsed and every documented action and subcommand is checked against a fresh build.
5. **Live suites** for engines that cannot be stubbed: `tests/cdp_live.rs` drives a real headless Chromium over CDP, and `tests/ocr_live.rs` runs macOS Vision OCR on a committed image. Neither needs TCC.

## Commands

```bash
cargo test --all-targets
cargo test --all-targets --all-features
cargo test --doc --all-features
cargo nextest run --all-targets --all-features
```

The nextest command requires cargo-nextest; `cargo test` still works without it. CI uses nextest to isolate process-global state between tests. The default-feature test run is separate on Linux because it exercises feature-off code paths, while the portal compile check requires the native dependencies listed in [AGENTS.md](AGENTS.md#commands). Neither run needs a portal daemon:

```bash
cargo check --features portal --all-targets   # Linux compile gate for the feature
cargo nextest run --all-targets               # Linux, default features
```

Run the live CDP suite with a browser available:

```bash
CHROME_BIN="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
  COMPUTER_USE_REQUIRE_LIVE=1 cargo test --features cdp --test cdp_live -- --test-threads=1
```

## Test layout

| Path | What it covers |
| --- | --- |
| `src/**` `#[cfg(test)] mod tests` | Unit tests for the module they sit in. |
| `src/test_backend.rs` | `StubBackend`, the in-memory `DesktopBackend` used by dispatch tests. Compiled into test builds only. |
| `tests/cli.rs` | Binary tests over the whole command surface; each test gets its own `XDG_*` tree. |
| `tests/cli_doc_matches_help.rs` | Binds `docs/CLI.md` to the binary: action names, subcommands, and the version in `Cargo.toml`. |
| `tests/cdp_live.rs` | Live CDP against a real headless Chromium and an in-process HTTP server. `#![cfg(feature = "cdp")]`. |
| `tests/ocr_live.rs` | Live macOS Vision OCR on `tests/fixtures/ocr/hello.png`. macOS only, no TCC needed. |
| `tests/fixtures/openai/`, `tests/fixtures/anthropic/` | Request fixtures for the adapter translation tests. |

## Rules

- Never move the pointer, capture the screen, or require Accessibility or Screen Recording in a test. Live suites may start a browser or run OCR; nothing else touches the real desktop.
- Isolate the environment. Tests that reach the cache, policy, audit, or pause paths point `XDG_CACHE_HOME`, `XDG_STATE_HOME`, and `XDG_CONFIG_HOME` at a per-test temp directory. `tests/cli.rs` uses a `TempDir` that removes itself on drop, including while unwinding from a failed assertion.
- The session cache panics in test builds when it is reached without `XDG_CACHE_HOME`, so a forgotten variable fails loudly instead of writing `~/.cache/computer-use`.
- Off-platform commands are tested on every OS: the subcommand is present in `--help`, and invoking it on the wrong OS returns `unsupported`. `doctor` is the exception: it runs everywhere and reports which probes the OS has.
- Test names are descriptive phrases (`cli_doc_lists_every_json_action`, `auto_plan_uses_ax_for_presses_and_event_then_activate_for_points`) so a failure line names the behavior.
- A new `Action` variant or alias must be added to the canonical list in `tests/cli_doc_matches_help.rs` and to `docs/CLI.md` in the same change.
- When the `DesktopBackend` trait grows, extend `StubBackend` in the same change.

## Live tests

`tests/cdp_live.rs` uses a process-local fixture containing a headless browser and an HTTP server bound to loopback, then runs the compiled binary against them. A normal `cargo test` process shares that fixture across tests under a mutex; nextest's process-per-test execution starts a fresh fixture per test. Without a Chromium executable the suite returns early with a skip message. Setting `COMPUTER_USE_REQUIRE_LIVE=1` turns a missing browser into a failure; it does not enable the `cdp` feature. CI installs Chrome, enables `cdp`, and sets the variable. `CHROME_BIN`, `COMPUTER_USE_CHROME`, and `GOOGLE_CHROME_SHIM` are checked for a browser path.

`tests/ocr_live.rs` is a normal macOS test: Vision needs no TCC grant, so it runs in the default suite. On other platforms the platform gate skips the module.

## Timeouts and retries

`.config/nextest.toml` bounds the suite: a test slower than 90 seconds is reported slow and is killed after three periods, and `fail-fast` is false so one failure does not hide the rest. The `cdp_live` binary is overridden to run one test at a time (`threads-required = "num-cpus"`, since each test starts its own browser) with a single retry to absorb browser timing flakes.

## Coverage

`scripts/coverage-gate.py` enforces a line-coverage floor on a `cargo llvm-cov --json` report. macOS enforces **95%**; Linux and Windows report the number. Only files under `src/` count: build output, dependencies, and other generated paths are listed as ignored and never folded into the total. Files that cannot execute on a runner are excluded through `coverage-allowlist.json`, and each entry names the runner and a reason. The admission rule is that the code **cannot execute** in the CI environment, not that it is hard to test.

Follow [docs/COVERAGE.md](docs/COVERAGE.md#run-it-locally) for tool installation and the full command sequence. Install Chrome/Chromium and set `COMPUTER_USE_REQUIRE_LIVE=1` so a missing browser cannot silently reduce the measurement. That variable prevents skipping; tests still run when a browser is available without it. The coverage document also records exclusions and the caveat that inline `#[cfg(test)]` modules count as covered lines.

## CI

`ci.yml` runs on every push and pull request:

- `fmt` — `cargo fmt --all -- --check`.
- `cdp-live` — Ubuntu, macOS, and Windows; installs Chrome, then runs the live CDP suite with `COMPUTER_USE_REQUIRE_LIVE=1`.
- `test` — Ubuntu, macOS, and Windows; clippy, doctests, the default-feature build, the `portal` compile check on Ubuntu, `cargo nextest run` for the default feature set on Ubuntu, and `cargo nextest run --all-targets --all-features` everywhere. On macOS it also packages an ad-hoc `Computer Use.app` and asserts `doctor` reports the bundle identity. It also runs the release-guard and coverage-gate fixture checks over `scripts/`.
- `coverage` — Ubuntu, macOS, and Windows; measures with `cargo llvm-cov --all-targets --all-features`, enforces the floor on macOS, and reports elsewhere.
