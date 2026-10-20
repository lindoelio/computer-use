# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project follows [Semantic Versioning](https://semver.org/).

## [0.2.0] - 2026-10-20

### Added

#### P1 — discovery and launches (macOS)

- `list-apps`, `list-windows` (z-order, index 0 is front), `launch`, `activate`, `quit` against NSWorkspace.
- Built-in deny list for Terminals and Password Managers. Off macOS the launch / activate / quit surface returns `unsupported`.
- `displays` reports connected monitors with `width`, `height`, `x`, `y`, `scale`, and a `display_id` for `--display`.
- JSON request parser, `Action` enum, `dispatch` / `dispatch_with`, and `CallContext`.

#### P2 — accessibility tree, electron helpers, session cache

- Numbered `tree` and `get-state` from the native AX tree; `source: "ax"`.
- `--diff` and `--full` against the per-session cache. Truncation caps: 40 children, depth 12, 400 lines.
- URL shortening (scheme, host, at most 60 characters of path; query stripped) and redaction of secure values.
- `AXManualAccessibility` and `AXEnhancedUserInterface` set on first attach; restored on `session end` and process exit.
- Foreign-pid walker with a cap of 3 helpers.
- Cache files at `${XDG_CACHE_HOME:-~/.cache}/computer-use/sessions/<session>/` (`%LOCALAPPDATA%\computer-use\cache\sessions\<session>\` on Windows), mode `0600` on Unix.

#### P3 — element targeting and value actions

- `--element` indexing for `press`, `focus`, `set-value`, `select-text`, `perform-action`, `scroll-to`, and `click --element`.
- `AXValue` path for `set_value` and `type --element` when the attribute is settable. Non-settable elements are focused and posted to the target pid.
- Hit-test fallback (`AXUIElementCopyElementAtPosition`) for `--x`/`--y` when no index is given. Reports `hit.role` and `hit.title`.
- `stale_element` walks the cached child-index path; a role, identifier, or frame change (beyond 2 points) or a revision header mismatch fails the action with the fresh tree in the body.
- `AXScrollToVisible` runs before the action when the element is outside the window and the action supports it. Failure is a warning unless `--require-visible`.

#### P4 — per-window screenshots and the 1440 coordinate map

- `screenshot --window` via `SCContentFilter` `initWithDesktopIndependentWindow` (`capture_source: "sck"`).
- Default `max_width` is 1440; `--max-width 0` keeps the source width; `--canvas WxH` letterboxes.
- `image_to_screen` and `screen_to_image` map stored in `screenshot-map.json` (mode 0600). `scale` survives every remap.
- Stale SCK frame falls back to a desktop xcap crop with `capture_source: "xcap"` and `occluded: true`.

#### P5 — state attachment, batched actions, adapters

- `--state` / `include_screenshot` attaches the post-action tree and screenshot. That diff becomes the `tree --diff` baseline. Read-only actions ignore it.
- `action: "batch"` runs a single resolved target and revision sequence. `stop_on_error` defaults true; a step's own `return_state` is ignored.
- `--adapter openai` and `--adapter anthropic` translate the public `computer` and `computer_20251124` shapes into the native protocol. `coord=image` is set automatically.
- `Image coordinate remap, about 1440×900` parity row satisfied: 1440 default, `screen_to_image` and `image_to_screen` in every image, `fit: canvas`, `content_width`, `content_height`, and `pad` reporting.

#### P6 — policy, confirms, terminals deny, pause

- User and admin policy files (`~/.config/computer-use/policy.toml`, `/etc/computer-use/policy.toml`, `%ProgramData%\computer-use\policy.toml` on Windows). Deny wins. Admin allow entries do not grant. Built-in deny list covers Terminals, Password Managers, Apple Passwords, and Keychain Access.
- `--confirm-cmd` / `COMPUTER_USE_CONFIRM_CMD` hook for sensitive actions. `confirm_required`, `confirm_denied`, `policy_denied` error codes.
- Secure-field refusal: `set_value`, `type --element`, and a key aimed at a secure field return `policy_denied`; the value is never read.
- `[browser].blocked_urls` rejects URLs (and bare origins) before the CDP navigate and before `page.url`-using browser ops.
- Listen-only `CGEventTap` with `CGEventField::EventSourceUserData == 0x00C0_5EC0_0DE1` (field 42) marker; `--pause` flips `paused` and the `paused` file is the cross-process source of truth.

#### P7 — input tiers, parallel agents, overlay

- `--tier ax | event | synthetic | activate`. `event` posts to the target pid (`CGEventPostToPid`) and sets `cursor_moved` when the pointer moves by more than 1 point.
- `--require-background` refuses `activate`; `--allow-foreground` skips `foreground.lock`.
- `computer-use overlay serve` draws a click-through 16×16 ring at the last agent point. The main CLI sends `{x, y, session}` on `<cache>/overlay.sock`; a missing helper adds `overlay helper not running` and the action still succeeds.
- `--features skylight` enables `SLPSSetFrontProcessWithOptions` / `CPSSetFrontProcess` resolution behind `--tier synthetic`. Off unless the feature is compiled in and the caller passes the matching flag.

#### P8 — menus, Apple events, Shortcuts, Dock, Spaces, doctor

- `menu list` walks `AXMenuBar → AXMenuBarItem → AXMenu` (default depth 4, max 8). `menu click --path "File > Save"` `AXPress`es the leaf.
- `apple-event --script <SCRIPT>` runs `osascript -e`. The script body is never logged verbatim; the audit line records `script_bytes` and a stable FNV-1a 64-bit hash. `language` is `applescript` (default) or `javascript`.
- `shortcuts run <NAME>` invokes `shortcuts run` with a 60 s deadline.
- `dock list` and `dock click --title <T>` (case-insensitive title match).
- `spaces` groups on-screen windows by the screen that contains each window's center. Mission Control moves are not attempted.
- `doctor` reports permissions, the TCC identity, the bundle id, and named `fix` steps. `prompt` calls the request APIs; `open_settings` opens the first missing pane (macOS only).
- `Computer Use.app` (`scripts/package-macos-app.sh`, bundle id `ai.lindoelio.computer-use`). Developer ID signing and notarization only run when the secrets in `docs/PACKAGING.md` exist. The crate tarball does not include `packaging/` or `scripts/` until P15.

#### P9 — clipboard, OCR helper, sign-and-notarize, audit

- `clipboard get` returns `kind: "text" | "image" | "empty"`; an image is written to the session cache. `clipboard set` requires a confirm hook.
- `computer-use --clipboard` is classified sensitive and runs through the same hook as `quit --force`.
- Audit log at `${XDG_STATE_HOME:-~/.local/state}/computer-use/audit.jsonl` (`%LOCALAPPDATA%\computer-use\audit.jsonl` on Windows). Records `decision`, `source`, `code`, `chars` (for `type` / `browser type`), `script_bytes` (for `apple-event` / `browser eval`), and the resolved `app` / `window`.

#### P10 — Chrome DevTools Protocol attach

- `--features cdp` enables `chromiumoxide` 0.9.1. `browser tabs | activate | navigate | click | text | type | eval` go through a fresh `tokio::runtime::Builder::new_current_thread()` runtime per call; the chromiumoxide handler is spawned on that runtime so it is also dropped.
- Port discovery reads the `--app` pid's argv: macOS `sysctl KERN_PROCARGS2`, Linux `/proc/<pid>/cmdline` (NUL-separated UTF-8). Windows port discovery is best-effort `unsupported`; pass `--cdp-url ws://host:port`.
- `browser navigate` blocks the request URL and the current page URL against `[browser].blocked_urls`. `browser click` uses CDP `Input.dispatchMouseEvent` (no screen-space transform; documented in the source).
- `browser eval` returns `script_chars` and a stable `DefaultHasher` hash; `browser type` returns `chars`.

#### P11 — OCR, set-of-marks

- macOS: a real `VNRecognizeTextRequest` call with `VNRequestTextRecognitionLevel::Accurate` and `setUsesLanguageCorrection(true)`. One `OcrBlock` per `VNRecognizedTextObservation`. Vision's bottom-left origin is flipped to image pixels and redacted against secure-field frames.
- Windows: a real `Windows.Media.Ocr.OcrEngine::TryCreateFromUserProfileLanguages()` call on a dedicated worker that calls `RoInitialize(RO_INIT_MULTITHREADED)` itself, joined with a 10 s `recv_timeout`. One `OcrBlock` per `OcrLine`, whose box is the union of its words' `BoundingRect`s.
- Linux: `unsupported` ("no OCR backend on Linux in 0.2.0").
- `marks` stamps numbered element rectangles (tree indices or OCR `oN`) on a window capture.

#### P12a — Windows UIA backend

- `windows::Win32::UI::WindowsAndMessaging::EnumWindows` drives `list_apps` and `list_windows`. Executable path comes from `QueryFullProcessImageNameW`.
- UI Automation control-view walker reports `source: "uia"`. `GetForegroundWindow` is never used to resolve a window the caller already named.
- UIA control patterns (`Invoke`, `Value`, `Text`, `ScrollItem`, `ExpandCollapse`) for element actions. `IsPassword` refuses `set_value`, `type --element`, and `select_text` before any pattern runs.

#### P12b — Windows tier-activate retry and `windows-capture`

- Missing or failed UIA patterns retry as tier `activate` (`SetForegroundWindow` + enigo under `foreground.lock`) for `press` and `type`. Result carries the `foreground required` warning. `--allow-foreground` skips the lock and still falls back; `--require-background` refuses the fallback outright.
- `set_value` stays off the `activate` retry on purpose (value-not-keystrokes contract). `select_text` and `scroll_to` likewise.
- `windows-capture` 2.0.1 for per-window capture: `Settings::new(Window::from_raw_hwnd(...), WithoutCursor, WithoutBorder, Exclude, ...)`, `start_free_threaded`, the first `FrameBuffer` is copied out. 5 s first-frame wait, then xcap crop fallback with `capture_source: "xcap"` and `occluded: true`.
- `GetDpiForWindow(hwnd) / 96` is the `scale` in the screenshot map.
- `focus_restored` reports the verified `GetForegroundWindow` restore instead of assuming true.

#### P13a — Linux AT-SPI and X11

- AT-SPI through `AccessibilityConnection` reports `source: "atspi"`. `Role::PasswordText` is the only redaction signal.
- Element actions: `EditableText::set_text_contents` wins over `Value::set_current_value` when both are advertised; `Text::set_selection(0, start, end)` falls back to `Text::add_selection(start, end)`. `Component::scroll_to(ScrollType::TopLeft)` for scroll-into-view.
- `Role::PasswordText` nodes refuse `set_value`, `select_text`, and `type` before the bus call.
- X11 XTest for tier `event`; enigo for tier `activate`.

#### P13b — Linux XDG portal capture and RemoteDesktop input

- `--features portal` enables `ashpd` 0.13.13 ScreenCast + RemoteDesktop, `pipewire` 0.9, `reis` 0.7.
- ScreenCast capture (prefer a window source, else the containing monitor cropped to the window rect) with `capture_source: "portal"`. A cancelled dialog is `permission_required` and does **not** fall back; any other portal failure falls back to the xcap crop with `occluded: true`.
- RemoteDesktop pointer / scroll / key combinations on Wayland. `--require-background` refuses it because it moves the real pointer.
- `reis` 0.7 has no public `ei_device` API, so the EIS handshake reports availability only.

#### P14 — isolation: display filter, Xvfb recipe, private CGVirtualDisplay

- `--display <id>` resolves a monitor from `displays` and gates every screen point (`click`, `double-click`, `triple-click`, `move`, `scroll`, `drag`, `mouse-down`, `mouse-up`) plus `--region`/`--window` bounds. Anything outside is `invalid_arguments`; an unknown id names the ids that do exist.
- `--display virtual` prints the Xvfb recipe (`Xvfb :99 -screen 0 1440x900x24`) and, with `--exec -- <cmd...>`, execs that command with `DISPLAY=:99`. **It never starts Xvfb** (an agent must not leave a stray X server behind). On macOS/Windows it is `unsupported`.
- `--features virtual-display` resolves `CGVirtualDisplayDescriptorCreate` and `CGVirtualDisplayCreate` with `dlsym` against an explicitly `dlopen`ed CoreGraphics and **stops there**. No display is ever created. `isolation` reports `experimental` only when both symbols resolved; on macOS 27 (the development machine) both are absent, so it reports `unsupported`.
- `docs/ISOLATION.md` documents the VM proxy and the five infeasible items; the `isolation` JSON mirrors them, with a snapshot test binding the two together.

### P15 — docs and 0.2.0 release

- `Cargo.toml` `version = "0.2.0"`, `rust-version = "1.91"`.
- `include` list now ships `docs/ROADMAP.md`, `docs/ISOLATION.md`, `docs/PACKAGING.md`, `docs/SKYLIGHT.md`, `docs/policy.example.toml`, `packaging/macos/Info.plist`, `packaging/macos/entitlements.plist`, and `scripts/package-macos-app.sh`.
- `tests/cli.rs` expects `0.2.0` on `--version`.
- README rewritten from the current CLI surface: all commands, default max-width, policy path, feature flags, permissions per OS, tiers, JSON actions, error codes, library surface.
- New `docs/CLI.md` (cli reference): every command, flag, JSON action name, and error code, with a `tests/cli_doc_matches_help.rs` snapshot that parses the doc and asserts each entry is reachable on a fresh build. The doc notes that `smart-computer-use/references/cli-reference.md` is regenerated in a separate repo.
- A `vs Codex` summary under the parity table listing where we exceed, match, and fall short, with no marketing claims.

### Known limitations

Items tested by code and CI but not exercised live for this release.

- **Accessibility was off for this process on the development Mac.** The full live path (press, value, set-value, perform-action, scroll-into-view, hit-test, event posting, AX press via SkyLight) was not exercised against a real window because the TCC grant is missing in this profile. `doctor` reports it. The code paths and unit tests pass.
- **Windows is CI-only, not live.** UIA patterns, tier `activate`, and the per-window `windows-capture` path are wired and tested on the macOS harness with the mock backend, and CI compiles and unit-tests on `windows-latest`. No live Windows host ran in P15.
- **Linux is CI-only, not live.** No Wayland compositor was available on the development machine. AT-SPI works on X11 with `DISPLAY`; RemoteDesktop portal works when `portal` is compiled in. `list_windows` / `list_apps` / `launch` / `activate` / `quit` are still `unsupported`, so `screenshot --app` cannot reach `capture_window` yet.
- **CDP click uses `Input.dispatchMouseEvent`.** No screen-space transform; the click is in the page coordinate system. The deviation is documented at the function and in P10.
- **Windows CDP port discovery is best-effort `unsupported`.** `QueryFullProcessImageNameW` does not give us argv; pass `--cdp-url ws://host:port` on Windows.
- **Linux OCR is `unsupported`** in 0.2.0. No Tesseract, no `asyc-tesseract` glue, no plugin.
- **Private-API features are experimental, fail closed.** `--tier synthetic` / `--features skylight` and `--features virtual-display` use `dlsym` and never call the symbol on the development machine (macOS 27). They are off unless the feature is compiled in and the caller passes the matching flag.
- **Unsigned app bundle unless secrets exist.** `Computer Use.app` is built and ad-hoc signed by `scripts/package-macos-app.sh`; Developer ID signing and notarization only run when the secrets in `docs/PACKAGING.md` exist on CI. They are not configured today.

### Cut from 0.2.0

Nothing was cut. Every parity row landed; two remain `PARTIAL` because each platform implementation can fail (`scroll_to` returns `execution_failed` on Linux when `Component` is missing; `select_text` returns `execution_failed` when `Text` is missing), not because the parity contract is unmet. The honest tally is in the parity table.

### Removed

Nothing. The 0.1.0 surface is still present, including the examples the README keeps (`wait --ms 10`, `serve` over stdin, `exec` for one-shot actions).

## [0.1.0] - 2026-10-18

### Added

- `computer-use` library and CLI for screenshots, mouse, keyboard, scroll, displays, and a best-effort desktop snapshot.
- Newline-delimited JSON mode (`serve` and `--json`) and one-shot `exec`.
- GitHub Actions CI and tagged release workflows for Linux x64, macOS arm64, and Windows x64.

[0.1.0]: https://github.com/lindoelio/computer-use/releases/tag/v0.1.0