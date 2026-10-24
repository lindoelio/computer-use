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
- **Platform-overridden defaults**: `backend/mod.rs` on macOS. Every platform
  method is implemented by `MacOsDesktopBackend`, so the trait's default bodies
  cannot execute there; the Linux and Windows runners measure their own.
- **macOS module wrappers**: `backend/macos/mod.rs`. `capture_window`,
  `marks_for_tree`, `secure_ocr_frames`, `inspect_accessibility`, and
  `element_action` need Accessibility or Screen Recording TCC.
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
CDP suite and the in-process Vision test; both need their real engine):

| | Value |
| --- | --- |
| Whole crate (all files) | ~75.6% |
| Measurable set (allowlist applied) | **91.37%** |
| Enforced floor | 90.0% |
| Target | 95.0% |
| Gap | **~790 lines** |

The remaining gap is small clusters (3–20 lines each) of error and edge paths:

1. `src/protocol.rs` (~540): dispatcher error arms, artifact/output helpers, the
   sensitive-press path, and a few context-parsing branches.
2. `src/browser/cdp.rs` (~290): attach/discovery fallbacks, `sysctl` argv
   parsing, the raw dialog rescue, and batch error shaping.
3. `src/browser/actions.rs` (~124): element error mapping, key dispatch
   variants, and dialog error paths.
4. `src/main.rs` (~105): flag conversions and the serve/emit loop.
5. `src/policy.rs` (~107), `src/cache.rs` (~74), `src/adapters/*` (~69),
   `src/isolation.rs` (~37), `src/browser/snapshot.rs` (~35),
   `src/overlay.rs` (~34), `src/doctor.rs` (~33), `src/target.rs` (~31),
   `src/vision/marks.rs` (~30).

The first two entries account for ~60% of what is left and are the next
target; the rest is a long tail of one-branch tests.

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
