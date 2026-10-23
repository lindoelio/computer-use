# Coverage

`scripts/coverage-gate.py` enforces a line-coverage floor on a
`cargo llvm-cov --json` report. The CI job `coverage` runs it on macOS and
reports it on Linux and Windows.

## Run it locally

```bash
rustup component add llvm-tools-preview
cargo install cargo-llvm-cov          # or: brew install cargo-llvm-cov
cargo llvm-cov --all-targets --all-features --json --output-path cov.json
python3 scripts/coverage-gate.py cov.json --os macos
```

`cov.json` is not committed. `--mode report` prints the same numbers without
failing, which is what the Linux and Windows jobs do for now.

## Policy

`coverage-allowlist.json` holds the numbers, the measured roots, and the
exclusion list:

| Key | Meaning |
| --- | --- |
| `roots` | Paths that count. **`src/` only**: that is the codebase under test. |
| `target` | The goal for measurable code, **90%**. The gate prints the gap. |
| `threshold` | The enforced regression floor. Raise it as tests land, then make it equal to `target`. |
| `per_file_threshold` | Per-file floor. `0` while the target is still being worked toward. |

### Only the codebase counts

`target/` is Cargo's build output (compiled artifacts, build-script and bindgen
output, incremental state). It is git-ignored and never measured. The gate
enforces this: a file is counted only when it resolves inside the repository
**and** under `roots`; anything else is printed under `ignored` and left out of
the total. There is no basename fallback, so a dependency or `target/` path
cannot silently join the total. The repository has no `build.rs` and no
generated source committed under `src/`.

The current report contains 57 files, every one under `src/` — dependencies,
test files, and build output are already absent from the `cargo llvm-cov`
output, and the gate would ignore them anyway.

An allowlist entry is excluded from both the per-file check and the total. Every
entry names the runner (`os`) and a reason. The rule for admitting one: the code
**cannot execute** in the CI environment, not that it is merely hard to test.

Excluded today, grouped by reason:

- **macOS TCC**: `backend/macos/{ax,capture,input,menu,dock,apps,scroll_ax}.rs`
  need Accessibility or Screen Recording, which a CI test binary cannot be
  granted, and a real desktop session.
- **Compiled out of test builds**: `backend/macos/{overlay,focus}.rs`.
- **Signed-bundle identity**: `backend/macos/doctor.rs` (codesign, AppKit
  bundle metadata, the TCC responsible-app chain).
- **Linux session**: `backend/linux/*` needs an X server, an AT-SPI bus, or a
  portal daemon.
- **Windows desktop**: `backend/windows/*` needs an interactive desktop for
  UI Automation and `Windows.Graphics.Capture`.
- **Real hardware paths**: `backend/{capture,input,held,clipboard}.rs` post
  input, capture a display, or touch the system pasteboard; the stub backend
  covers the dispatch instead.
- **Managed Chromium**: `browser/launch.rs` spawns its own browser, and the
  live CDP suite starts its own. Remove the entry when a live test drives
  `browser launch`.
- **Test scaffolding**: `test_backend.rs`.

## Where the number stands

Measured on macOS with `--all-targets --all-features` (this includes the live
CDP suite, which needs Chrome on `PATH`):

| | Value |
| --- | --- |
| Whole crate (all files) | ~69% |
| Measurable set (allowlist applied) | 82.09% |
| Target | 90.0% |
| Gap | **~1,700 lines** |

The gap is dominated by code that is reachable but not yet exercised:

1. `src/browser/cdp.rs` (~800), `src/browser/actions.rs` (~270),
   `src/browser/keys.rs` (~80), `src/browser/mod.rs` (~60) — the live CDP
   suite covers attach mode and a handful of commands. Extending
   `tests/cdp_live.rs` with managed `browser launch`/`quit`, `select`,
   `check`, `upload`, `dialog`, `hover`, `press`, `scroll`, `tab-new/close`,
   `screenshot`, and `eval` is the single largest lever, and it also lets
   `browser/launch.rs` be removed from the allowlist.
2. `src/protocol.rs` (~1,000 production lines) — dispatcher branches and
   helpers that no unit test reaches.
3. `src/main.rs` (~260) — CLI flag combinations and error paths beyond the
   surface sweep in `tests/cli.rs`.
4. `src/policy.rs` (~220) — the remaining built-in-deny exemption paths (the
   registry is process-global, so those need a serialized test).
5. `src/backend/mod.rs` (~190) — trait defaults that the platform backend
   overrides; unreachable on the macOS runner, reachable on Linux and Windows.
6. `src/vision/ocr.rs` (~95), `src/cache.rs` (~90), `src/element.rs` (~65),
   and the small adapters/state/types gaps (~200).

## Why Linux and Windows report instead of enforce

Their numbers cannot be measured from a macOS development machine, and their
platform backends are allowed out (see above), so the measurable set is
smaller and shifts with each runner. Both jobs print the number. Once the
first green run reports their baselines, add the value to the allowlist as a
per-OS `threshold` override and switch `enforce` to `1` in the workflow.

## Caveat

Inline `#[cfg(test)]` modules count as covered lines when their tests run, and
stable Rust cannot exclude them (`#[coverage(off)]` is still behind
`coverage_attribute`). The measured number is therefore a little optimistic
about production code. The per-file view in the gate output is the honest
signal: it is where a file with a large test module can hide an untested
production half.
