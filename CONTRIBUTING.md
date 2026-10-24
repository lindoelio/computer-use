# Contributing

Thanks for your interest in `computer-use`. This page covers the development workflow; see `README.md` for the user-facing surface and `docs/CLI.md` for the full command and JSON reference.

## Before you start

Use Rust 1.91 or newer, as declared in `Cargo.toml`. Follow [AGENTS.md](AGENTS.md#commands) for build commands and Linux native prerequisites; optional test tools and browser requirements are described in [TESTING.md](TESTING.md).

## Repository structure

- `src/` — the crate. `src/lib.rs` re-exports the public API; `src/main.rs` is the clap CLI; `src/protocol.rs` defines `Action`, `Response`, and `dispatch`; `src/backend/` holds the `DesktopBackend` trait and the platform backends.
- `tests/` — integration tests and JSON fixtures for the OpenAI and Anthropic adapters and OCR.
- `docs/` — documentation shipped with the crate (see the `include` list in `Cargo.toml`).
- `scripts/` — `package-macos-app.sh`, `check-macos-app-status.sh`, and `coverage-gate.py`.
- `packaging/macos/` — `Info.plist` and entitlements for `Computer Use.app`.
- `.github/workflows/` — `ci.yml`, `release.yml`, and `publish-crate.yml`.
- `.config/nextest.toml` — nextest timeout and retry policy.
- `coverage-allowlist.json` — the coverage gate configuration.

## Build and test

Run the relevant checks from [AGENTS.md](AGENTS.md#commands) before requesting review. See [TESTING.md](TESTING.md) for the test layers and [docs/COVERAGE.md](docs/COVERAGE.md) for the coverage policy.

## Making changes

- Use a descriptive topic branch, for example `feat/<topic>`. No enforced branch-name pattern is configured in this repository.
- Recent commit subjects use prefixes such as `feat:`, `fix(scope):`, `test:`, `docs:`, `ci:`, and `chore:`. Match the surrounding history; no commit-message validator is configured.
- Keep a change focused. Do not mix refactors with behavior changes.
- Record user-visible changes in `CHANGELOG.md`, following its release sections. The file follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project follows [Semantic Versioning](https://semver.org/).
- Update `README.md` and `docs/CLI.md` when the command surface, JSON schema, or flags change. `tests/cli_doc_matches_help.rs` fails when a documented action or subcommand is not reachable on a fresh build.
- `docs/ROADMAP.md` records the shipped 0.2.0 phase plan. Do not add new work there; later releases are documented in `CHANGELOG.md`.
- Follow `STYLEGUIDE.md` for code conventions and `TESTING.md` for tests.

## Pull requests

The configured CI checks are:

- `rustfmt` check (`cargo fmt --all -- --check`).
- Clippy with `--all-targets --all-features -- -D warnings` on Ubuntu, macOS, and Windows.
- The test job on Ubuntu, macOS, and Windows: doctests, the default-feature build, the `portal` compile check on Linux, and `cargo nextest run --all-targets --all-features`.
- The `cdp-live` job, which installs Chrome and runs `cargo test --features cdp --test cdp_live -- --test-threads=1` with `COMPUTER_USE_REQUIRE_LIVE=1`.
- The coverage job: macOS enforces a 95% line-coverage floor; Linux and Windows report the number.

When you add code that cannot execute on a CI runner, add it to `coverage-allowlist.json` with the affected `os` and a reason. The admission rule is that the code **cannot execute** in the CI environment, not that it is hard to test. `docs/COVERAGE.md` lists every current exclusion with its reasoning.

Describe behavior changes and how you verified them. Do not include generated files, local paths, or scratch artifacts in a pull request.

## Documentation workflow

- `docs/CLI.md` is bound to the binary by `tests/cli_doc_matches_help.rs`; update it in the same change that alters a command, flag, action, or error code.
- `docs/` files are shipped in the crate tarball when they are listed in the `include` array of `Cargo.toml`.
- Keep `README.md` accurate for users who install from a release or from crates.io.
- Behavior notes that matter to callers (defaults, limits, platform differences) belong in doc comments as well as in the user documentation.

## Releases

Maintainer process. A release is a tag whose name matches the version in `Cargo.toml` (`vX.Y.Z`); `publish-crate.yml` refuses a tag that disagrees and skips a version crates.io already carries.

- `release.yml` builds and attaches `computer-use-linux-x64.tar.gz` (`--features portal,cdp`), `computer-use-macos-arm64.tar.gz` (`--features cdp`), and `computer-use-windows-x64.zip` (`--features cdp`), each with a `.sha256` file, plus a combined `SHA256SUMS`.
- The macOS helper app is always packaged in the macOS release build; without signing secrets it is ad-hoc signed. Tag releases require a signed and notarized bundle, enforced by `scripts/check-macos-app-status.sh`. The `ALLOW_UNSIGNED_RELEASE=1` override permits an unsigned helper; the release workflow marks an unsigned archive pre-release. See `docs/PACKAGING.md`.
- `publish-crate.yml` publishes the crate to crates.io from the tag, one version per tag.

## License

The crate's license declaration is `MIT OR Apache-2.0` in `Cargo.toml`; see [README.md](README.md#license). Do not change licensing as part of unrelated contributions.
