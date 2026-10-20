# computer-use 0.2.0 roadmap
Status: P8 is done. Resume from P9 in the Progress table.
Target release: `0.2.0`. The crate stays on `0.1.0` until phase P15.
Public repo: <https://github.com/lindoelio/computer-use>.

This plan makes the CLI more complete than OpenAI Codex Computer Use while keeping a single binary, a Rust library, and newline-delimited JSON on stdin/stdout. There is no MCP server in any phase.

An interrupted implementation resumes from the [Progress](#progress) table. Finish the in-progress phase, update that row (status, commit, notes) and any parity rows the phase closed, then stop.

## What 0.1.0 already does
`dispatch` in `src/protocol.rs` takes an `Action` and returns one `Response` (`ok`, optional `id`, optional `error.code` / `error.message`, flattened result fields). The CLI in `src/main.rs` prints that object. `serve` and `--json` are a stdin protocol, not a socket server. `exec` runs one JSON value. `--help` and `--version` stay human-readable.

| Piece | Where | Behavior to preserve |
| --- | --- | --- |
| Actions | `Action` in `src/protocol.rs` | `screenshot`, `click`, `double_click`, `move` (alias `move_mouse`), `type`, `key`, `scroll`, `get_state` (alias `get-state`), `wait`, `displays` |
| Coordinates | `Region` in `src/types.rs` | Logical points. Origin is the top-left of the virtual desktop. Same space as `click`. |
| Capture | `src/backend/capture.rs` | `xcap` 0.3.3. Region crop multiplies by the monitor scale. Canvas cap 32768 px and 512 MiB. |
| Input | `src/backend/input.rs` | `enigo` 0.3. `Settings { open_prompt_to_get_permissions: false, release_keys_when_dropped: true }`. One `Enigo` per call. |
| macOS a11y | `src/backend/macos.rs` | `osascript` JXA via `command::run_timeout` (8s). Depth 6, 30 children, 1 MiB drop. Frontmost app is a separate AppleScript (4s). Permission text recognizes "not allowed", "assistive", `-1719`, `-25211`, `1002`. |
| Linux | `src/backend/linux/` | Frontmost window from `xdotool` when installed. P13a adds a real accessibility tree (AT-SPI 0.30.0 with the `connection` and `proxies` features, lazy current-thread `tokio::runtime` reused via `runtime::block_on`) and an X11 XTest `event` tier (x11rb 0.13 with the `xtest` feature). P13b adds `portal.rs`, behind the `portal` feature: `ashpd` 0.13.13 ScreenCast for capture, `pipewire` 0.9 to pull one frame, `ashpd` RemoteDesktop for tier `event` on Wayland, and optional `reis` 0.7 for an EIS handshake. A pure three-state `EventRoute` picks XTest on X11, RemoteDesktop on Wayland with the feature, and `unsupported` otherwise. The Linux `doctor` reports `atspi_bus` (presence and connection), X11/Wayland session, and a `portal` field for feature-compiled status. |
| Windows | `src/backend/windows/` | Native `EnumWindows` + `GetWindowTextW` + `GetWindowThreadProcessId` + `QueryFullProcessImageNameW` through the `windows` 0.62 crate. UI Automation tree + element actions through `uiautomation` 0.25.1 control patterns, with a `SetForegroundWindow` + enigo `activate` retry under `foreground.lock` when a pattern is missing or fails. Per-window capture through `windows-capture` 2.0.1 (`Windows.Graphics.Capture`), OCR through `Windows.Media.Ocr.OcrEngine`. |
| Images | `src/image_util.rs` | PNG or JPEG (quality 80). `--max-width` is opt-in and uses Lanczos3. No default cap. |
| Errors | `src/error.rs` | `invalid_arguments`, `execution_failed`, `io_error`, `unsupported`. |
| Tests | `src/**` unit tests, `tests/cli.rs` | No test moves the pointer or captures the screen. `tests/cli.rs` asserts version `0.1.0`. |
| CI | `.github/workflows/ci.yml` | rustfmt, clippy `-D warnings`, `cargo test --all-targets` on Ubuntu, macOS, and Windows. Linux apt packages: `pkg-config libxdo-dev libx11-dev libxcb1-dev libxrandr-dev libdbus-1-dev libxkbcommon-dev libatspi2.0-dev libpipewire-0.3-dev libclang-dev clang` (`libpipewire-0.3-dev` plus clang/libclang are P13b: `pipewire-sys` and `libspa-sys` run bindgen against those headers). Ubuntu also runs an explicit `cargo check --features portal --all-targets`. No portal daemon is ever contacted. |
| Release | `.github/workflows/release.yml` | Tag `v*` builds linux-x64, macos-arm64, windows-x64. No `.app` bundle. Toolchain is `dtolnay/rust-toolchain@stable`. |
| Safety boundary | `src/lib.rs`, `src/main.rs` | `#![forbid(unsafe_code)]`. |

`DesktopBackend` (`src/backend/mod.rs`) is synchronous and `Send + Sync`. `dispatch_with` catches panics. `collect_state` turns subsystem failures into `null` plus `note`. Those two properties stay.

## Goals
The agent addresses a running app by name, bundle id, or pid, and a window inside it. Each observation is the key window's accessibility tree as compact numbered text, plus a screenshot of that window. Element numbers are stable for one `(pid, window, revision)` and are checked again before the action runs. Text is set through the accessibility value, not by keystrokes, when the element supports it. Input prefers a path that does not move the user's pointer or change the frontmost app. A policy file can refuse apps, terminals, and URLs. The same policy and the same JSON action names exist on Windows and Linux once those backends land. Optional adapters accept OpenAI `computer` tool actions and Anthropic `computer_20251124` actions on `exec` and stdin.

## Non-goals
- An MCP server, an HTTP server, or a long-running daemon as the primary interface. `serve` stays stdin/stdout. The overlay helper in P7 is a local Unix-socket helper, not a remote service.
- Driving iOS, Android, or a browser that is not Chrome/Chromium via CDP.
- Granting macOS TCC permission from code. `doctor` reports state and prints the Settings path.
- A hidden sandbox on macOS. Phase P14 states which isolation ideas are real and which are not.
- Replacing `enigo` for the existing foreground actions. It remains the tier used when the caller asks to activate the target.

## MSRV and crates
Raise `package.rust-version` from `1.81` to `1.91` in P1. The local toolchain is Rust 1.91. Optional crates need a newer floor than 1.81: `chromiumoxide` 0.9.1 declares 1.85, `ashpd` 0.13.13 declares 1.87, and `windows-capture` 2.0.1 / `uiautomation` 0.25.1 / `chromiumoxide` 0.9.1 use edition 2024. CI already installs stable via `dtolnay/rust-toolchain@stable`, which is at least 1.91 on the date of this plan.

Do not pin `ashpd` below 0.13.6. Releases 0.13.0 through 0.13.5 advertise an MSRV of 1.92.

Versions below were the crates.io maxima checked on 2026-10-18. Re-check once at the start of the phase that adds the crate. If a patch release breaks the API, stay on the version named here.

| Crate | Version | Phase | Role |
| --- | --- | --- | --- |
| `objc2` | 0.6.5 | P1 | Objective-C runtime. MSRV 1.71. |
| `objc2-foundation` | 0.3.2 | P1 | `NSString`, `NSURL`, `NSArray`. Match the 0.3.2 framework line. |
| `block2` | 0.6 | P1 | Completion handlers. Use the release that `objc2` 0.6.5 accepts. |
| `objc2-app-kit` | 0.3.2 | P1 | `NSWorkspace`, `NSRunningApplication`, `NSScreen`, `NSWindow`, `NSEvent`, `NSMenu`. Enable only the features those types need. |
| `objc2-application-services` | 0.3.2 | P1 | Feature `AXUIElement`: `AXUIElement`, `AXObserver`, `AXError`, `AXUIElementSetMessagingTimeout`. |
| `objc2-core-graphics` | 0.3.2 | P1 | P1 enables `CGWindow` and `CGGeometry` (window list, preflight, request). `CGEvent`, `CGEventSource`, `CGEventTypes`, and `CGDirectDisplay` land with the phases that post events. |
| `core-graphics` | 0.25.0 | P1 fallback | Servo bindings. Use only if an `objc2-core-graphics` 0.3.2 feature flag omits a C function (`CGWindowListCopyWindowInfo`, `CGPreflightScreenCaptureAccess`). Do not take both stacks into one function. |
| `accessibility-sys` | 0.2.0 | — | Not a dependency. `objc2-application-services` exports `AXIsProcessTrustedWithOptions` and `kAXTrustedCheckOptionPrompt`. |
| `dirs` | 6 | P2 | Cache directories. Deferred from P1 because P1 writes no cache files. Unix session paths stay explicit. |
| `objc2-screen-capture-kit` | 0.3.2 | P4 | `SCShareableContent`, `SCContentFilter`, `SCScreenshotManager`, `SCStreamConfiguration`. |
| `arboard` | 3.7.0 | P8 | `Clipboard::get_text`, `set_text`, `get_image`, `set_image`. MSRV 1.71. |
| `toml` | 0.8.23 | P6 | `policy.toml`. `Error::span()` names the line. 0.9 was not required. |
| `chromiumoxide` | 0.9.1 | P10 | Feature `cdp` only. `Browser::connect`, page `goto`, `evaluate`, CSS query. MSRV 1.85. |
| `tokio` | 1 | P10, P13a | Feature `cdp` and the Linux async backends (lazily-built current-thread runtime, `runtime::block_on` from the CLI thread only). P13a keeps the same shared `tokio` feature set so the Linux AT-SPI work `block_on(AccessibilityConnection::new())` without a second async runtime. |
| `uiautomation` | 0.25.1 | P12 | Features `control`, `pattern`. `UIAutomation`, invoke / value / scroll / text / expand-collapse. |
| `windows` | 0.62 | P12 | Unified with whatever `windows-capture` 2.0.1 and `uiautomation` 0.25.1 already pull; no second version in the tree. P12a features: `Win32_Foundation`, `Win32_Security`, `Win32_System_Threading`, `Win32_UI_WindowsAndMessaging`, `Win32_Graphics_Gdi`. P12b adds exactly what its own calls need: `Foundation` + `Globalization` (WinRT async / recognizer language), `Graphics_Imaging` (`SoftwareBitmap`), `Media_Ocr`, `Security_Cryptography` (`CryptographicBuffer::CreateFromByteArray`), `Storage_Streams` (`IBuffer`), `Win32_System_Com` (`CoIncrementMTAUsage`), `Win32_System_WinRT` (`RoInitialize`), `Win32_UI_HiDpi` (`GetDpiForWindow`). Capture itself is `windows-capture`'s own `Graphics_Capture`, not a feature here. |
| `windows-capture` | 2.0.1 | P12 | One frame from the resolved HWND through `Windows.Graphics.Capture`, never the monitor when the caller named a window. The monitor is never captured on any code path. |
| `atspi` | 0.30.0 | P13 (a and b) | Features `connection`, `proxies`. `AccessibilityConnection`, `AccessibleProxy`, `Action`, `Component`, `Text`, `Value`, `EditableText`. MSRV 1.77.2. P13a compiles `connection` + `proxies` only; P13b keeps those and adds nothing new at the atspi layer. |
| `ashpd` | 0.13.13 | P13b | Features `screencast`, `remote_desktop`. MSRV 1.87. Linux-gated optional dependency, off until the `portal` feature is built. |
| `pipewire` | 0.9 | P13b | Consume the ScreenCast stream. Native library `libpipewire-0.3`. |
| `reis` | 0.7.1 | P13b | libei client when the compositor offers an EIS socket. The crate is incomplete. Feature `tokio`. |
| `x11rb` | 0.13 | P13a | Feature `xtest` (plus the `allow-unsafe-code` feature gate it requires). `x11rb::protocol::xtest::fake_input` drives the Linux `event` tier on plain X11. The 0.14 pin from the original P13 design is deferred to P13b so P13a lands on the already-resolved 0.13 line (which `arboard` and `xcap` already pull). |
| `objc2-vision` | 0.3.2 | P11 | Feature `VNRecognizeTextRequest` only. |

Kept: `enigo` 0.3.0, `xcap` 0.3.3, `clap` 4, `serde` 1, `serde_json` 1, `thiserror` 2, `image` 0.25, `base64` 0.22.

Cargo features (empty by default):

```toml
[features]
default = []
cdp = ["dep:chromiumoxide", "dep:tokio"]
skylight = []
virtual-display = ["skylight"]
portal = ["dep:ashpd", "dep:pipewire", "dep:reis", "dep:tokio"]
```

`cdp` and `skylight` are the heavy optional switches. `portal` is off in default Linux CI and on in the Linux release binary, because `libpipewire` is a system package. `cdp` is pure Rust and must compile on all three OS targets. `skylight` and `virtual-display` compile everywhere: the private-framework code sits in `#[cfg(target_os = "macos")]` and other targets return `unsupported`.

P1 declares those feature names with empty arrays (`cdp = []`, `portal = []`, `virtual-display = ["skylight"]`) so `--all-features` compiles before `chromiumoxide`, `ashpd`, `pipewire`, and `reis` are dependencies. The `dep:` lines in the block above are the end state, attached in P10 and P13.

Platform crates that are not features go in `[target.'cfg(target_os = "...")'.dependencies]` so a macOS build does not fetch `atspi` and a Linux build does not fetch `objc2-app-kit`.

`cargo check --all-features` and `cargo clippy --all-targets --all-features -- -D warnings` must pass on Ubuntu, macOS, and Windows. That is the compile gate for every phase after P1.

## Unsafe code
`#![forbid(unsafe_code)]` cannot survive in-process Accessibility, ScreenCaptureKit, or a `CGEventTap`. In P1:

- Change both crate attributes to `#![deny(unsafe_code)]`.
- Put `#![allow(unsafe_code)]` only on the files that call Apple or Win32 directly: `src/backend/macos/ax.rs`, `apps.rs`, `capture.rs`, `input.rs`, `overlay.rs`, `focus.rs`, `doctor.rs`, `src/vision/macos.rs`, `src/vision/windows.rs`. P1 allows it only in `apps.rs` and `doctor.rs`. The other files are created in the phase that calls those APIs.
- `main.rs` stays without unsafe. `policy.rs`, `audit.rs`, `tree.rs`, `cache.rs`, `element.rs`, and the adapters stay safe.
- Each `unsafe` block gets a one-line `SAFETY` comment naming the pointer lifetime and the framework precondition.
- `uiautomation`, `atspi`, `ashpd`, `x11rb`, `arboard`, and `chromiumoxide` are called through their safe APIs.

## Module layout
`cfg(target_os)` selects the backend. Every command exists in the clap tree on every OS. Off-platform execution returns `unsupported` with the OS name in the message, so `tests/cli.rs` can assert help text on all three CI runners.

```
src/
  lib.rs                      deny(unsafe_code). Re-exports.
  main.rs                     clap. Global flags. No unsafe.
  error.rs                    New codes below.
  types.rs                    Geometry, buttons, the coordinate space enum.
  protocol.rs                 Action, batch, dispatch. Strips id and session fields.
  state.rs                    Snapshot. Tree text joins the existing JSON.
  keys.rs                     Combos, plus xdotool names in P8.
  image_util.rs               Encode, max-width, canvas fit, remap math.
  target.rs                   AppTarget and WindowTarget resolution.
  tree.rs                     Numbered text, diff, truncation, redaction. Pure.
  doctor.rs                   Permission report assembly. Pure. P8.
  element.rs                  Revalidation, allowlist, secure refusal. Pure. P3.
  cache.rs                    Per-session revision files.
  policy.rs                   Built-in deny in P1. TOML, admin, URLs in P6.
  audit.rs                    JSONL writer. P6.
  adapters/
    mod.rs
    openai.rs                 OpenAI computer tool actions.
    anthropic.rs              computer_20251124 actions.
  browser/
    mod.rs                    Stub unless feature cdp.
    cdp.rs
  vision/
    mod.rs                    Set-of-marks drawing in safe image code.
    macos.rs
    windows.rs
  backend/
    mod.rs                    DesktopBackend, create_desktop_backend.
    capture.rs                xcap full-desktop path. Stays.
    input.rs                  enigo foreground path. Stays.
    command.rs                run_timeout. Stays for osascript fallback and shortcuts.
    macos/
      mod.rs
      ax.rs                   AXUIElement, AXObserver, Electron flags.
      apps.rs                 NSWorkspace, CGWindowList.
      capture.rs              ScreenCaptureKit.
      input.rs                CGEventPostToPid.
      overlay.rs              Click-through cursor window.
      menu.rs                 AXMenuBar.
      focus.rs                SkyLight. feature skylight.
      doctor.rs
    windows/
      mod.rs
      uia.rs
      apps.rs
      capture.rs
    linux/
      mod.rs
      atspi.rs
      portal.rs               feature portal.
      x11.rs
  test_backend.rs             StubBackend grows with the trait.
```

`DesktopBackend` gains methods rather than a second trait. Default methods in the trait return `ComputerError::Unsupported` so the stub and the unfinished platforms keep compiling. P1 adds `list_apps`, `list_windows`, `launch`, `activate`, `quit`, and `doctor`. Later phases add the rest. `dispatch` remains the only entry the CLI uses.

## Shared contracts
### Global flags
Added as clap global args, and as sibling fields of `action` in JSON. `parse_request` already removes `id` before `Action` is decoded. Extend that strip list so these keys never reach the action enum: `app`, `window`, `element`, `diff`, `full`, `return_state`, `include_screenshot`, `coord`, `session`, `tier`, `adapter`, `display`, `require_visible`, `allow_foreground`, `require_background`, `no_overlay`.

| Flag | JSON | Default | Meaning |
| --- | --- | --- | --- |
| `--app <name\|bundle\|pid>` | `app` | frontmost after P2 | Target application. |
| `--window <title\|index\|id>` | `window` | key window of that app | `index` is z-order, 0 is front. |
| `--element <n>` | `element` | none | Index in the current revision. |
| `--diff` / `--full` | `diff` / `full` | diff after a baseline exists | Tree shape. `--full` replaces the baseline. |
| `--state` | `return_state` | false | Attach post-action tree. |
| `--screenshot` | `include_screenshot` | false | With `--state`, also attach a window image. |
| `--coord screen\|image` | `coord` | `screen` on the CLI, `image` inside adapters | How to read `x`/`y`. |
| `--session <id>` | `session` | process id | Cache namespace. |
| `--tier ax\|event\|synthetic\|activate` | `tier` | auto | Input tier. `synthetic` needs the `skylight` feature and `--synthetic-focus` is accepted as an alias of `--tier synthetic`. |
| `--synthetic-focus` | sets `tier` to `synthetic` | off | Alias of `--tier synthetic`. Conflicts with `--tier`. |
| `--allow-foreground` | `allow_foreground` | false | Skip `foreground.lock` for tier `activate`. |
| `--require-background` | `require_background` | false | Do not fall back to `activate`. Cursor motion is `execution_failed`. |
| `--no-overlay` | `no_overlay` | false | Do not send the agent point to the overlay helper. |
| `--adapter native\|openai\|anthropic` | `adapter` | `native` | Schema translator for `exec` and stdin. |
| `--display <id\|virtual>` | `display` | all | P14. |
| `--require-visible` | `require_visible` | false | Fail when the element is outside the window and scroll-into-view fails or is missing. |
| `--policy <path>` | none | `~/.config/computer-use/policy.toml` | P6. |
| `--max-width <n>` | `max_width` | 1440 | Absent scales a screenshot down to 1440. `0` does not scale. |

`--pretty` still pretty-prints one-shot JSON only. `serve` stays one object per line.

### Error codes
Keep the four existing codes. Add these. `ComputerError::code()` is the stable string.

| Code | When |
| --- | --- |
| `app_not_found` | Name, bundle id, or pid matched nothing running, and launch was not requested. |
| `window_not_found` | App resolved, window did not. |
| `element_not_found` | Index missing in the cached or fresh tree, or there is no session cache. |
| `stale_element` | The path's role, identifier, or frame (beyond 2 points) changed, or the cached pid, window, or revision header does not match. Action not performed. Body includes the new tree. |
| `permission_required` | Accessibility or Screen Recording is off. Message names the Settings pane. |
| `policy_denied` | Built-in or configured deny. |
| `confirm_required` | Sensitive action and no confirm hook is configured. |
| `confirm_denied` | Hook exited non-zero or timed out. |
| `blocked_url` | Browser URL matched the block list. |
| `paused` | Local user input tripped the kill switch. |
| `cdp_unavailable` | Feature off, or Chrome has no debugging port. |
| `display_unavailable` | The screen is locked, the display is asleep, or no display or shareable window is available. Checked before `window_not_found`. |

`execution_failed` remains the bucket for framework errors that are not in this list. `unsupported` remains the off-platform and feature-off bucket. `display_unavailable` does not fall back to an xcap crop.

### Paths
| Item | Path |
| --- | --- |
| Session cache | macOS and Linux: `${XDG_CACHE_HOME:-~/.cache}/computer-use/sessions/<session>/`. Windows: `%LOCALAPPDATA%\computer-use\cache\sessions\<session>\`. |
| User policy | `~/.config/computer-use/policy.toml` on every OS, including macOS. |
| Admin policy | `/etc/computer-use/policy.toml`, or `%ProgramData%\computer-use\policy.toml`. |
| Audit log | `${XDG_STATE_HOME:-~/.local/state}/computer-use/audit.jsonl`, or `%LOCALAPPDATA%\computer-use\audit.jsonl`. |
| Foreground lock | `<cache>/foreground.lock`. |
| Overlay socket | `<cache>/overlay.sock`. |

Cache files are JSON, mode `0600` on Unix. They hold the last revision, the rendered tree, the structural nodes, and the last screenshot map. They do not hold secure-field values.

Each session directory has `owner.json` (`pid`, `created_unix_ms`). `screenshot-map.json` is the last screenshot map and is not a tree snapshot. `session prune` removes an empty session directory (nothing but `owner.json`) even when the owner pid is alive. A directory that still has content is kept while that pid is alive, including when it is older than the TTL. Content whose owner is dead or unknown is removed once it is at least 24 hours old (`--ttl` seconds, default 86400). A live pid's content is never deleted. `session end` also drops empty session directories. Tests set `XDG_CACHE_HOME` to a temp directory. A unit test that reaches the cache without that variable panics instead of writing `~/.cache/computer-use`.

### Tree text
One node per line. Indent is two spaces per depth. The number in brackets is the element index.

```
[0] AXWindow "Notes" @0,0 800x600
  [1] AXButton "New Note" @12,10 88x24 actions=AXPress
  [2] AXTextArea @12,40 776x500 value="hello"
    … +12
```

Rules implemented in `tree.rs` and covered by unit tests:

- Preorder numbering starts at 0 for the window.
- Attributes written when present: role, title or description in quotes, frame as `@x,y wxh` in screen points, `value=` truncated to 120 chars, `actions=` from `AXUIElementCopyActionNames`.
- Child cap 40. Extra children collapse to `… +N`. Depth cap 12. Line cap 400. A trailing line says `truncated=true` when any cap fired.
- `AXSecureTextField`, or a subrole containing `Secure`, prints `value="••••"`. The raw characters are dropped before the string is built.
- A value that starts with `http://` or `https://` keeps scheme, host, and path. The query string is removed. The path is cut at 60 characters.
- Diff against the cached tree uses a signature `(role, identifier-or-title, child-count)` and an LCS within each parent. Lines start with `+`, `-`, or `~`. Example: `~ [2] AXTextArea value: "a" -> "ab"`.
- `--full` prints the whole tree and stores it as the new baseline.

### Element identity
```rust
struct BoundElement {
    pid: i32,
    window_id: u32,
    revision: u64,
    index: u32,
}
```

`revision` is a hash of the structural signatures, stored in the session cache under `<bundle>-<pid>-<window>.json`. Before any element action the backend copies the tree again and walks the cached child-index path. The action runs only when that node still has the same role, the same `AXIdentifier` or title, and a frame within 2 points on each edge. Two points is the tolerance P2 uses to join an AX window to a CG window. A larger frame shift is `stale_element`. A role or identifier change is `stale_element` too: the body includes the fresh tree and nothing is performed. A cached revision header, pid, or window id that does not match the stored signature is the same error. A structural change in some other node does not by itself stale this element. Element actions write the fresh signature to `observed` and leave `root` for `tree --diff`. `x`/`y` without `--element` hit-tests (`AXUIElementCopyElementAtPosition`) and then uses the coordinate click path. A hit-test failure is a warning and the click still runs. If hit-testing finds nothing, `hit` is null and the coordinate tiers in P7 run.

### Post-action state
When `return_state` is true the success object gains:

```json
{
  "state": {
    "app": "com.apple.Notes",
    "pid": 412,
    "window_id": 8831,
    "revision": 18,
    "tree": "[0] AXWindow ...",
    "diff": "~ [2] AXTextArea value: \"a\" -> \"ab\"",
    "truncated": false
  }
}
```

`include_screenshot` adds `state.screenshot` with the same fields as the `screenshot` action (`path`, `width`, `height`, `scale`, `origin`, `map`, optional `data`). `tree` in this object is the full numbered text. `diff` is null when the snapshot created the baseline. The snapshot uses the same path as `tree`, so a returned state replaces the `tree --diff` baseline and advances the revision. Without `return_state`, element actions still update `observed` only and leave that baseline. Screenshot alone does not refresh it.

`include_screenshot` does nothing unless `return_state` is set. The `screenshot` action is the exception: with both flags, `state.screenshot` is the capture it just made, not a second shot. A capture failure does not roll back the action. `ok` stays true, `state.screenshot` is null, and `warnings` explains. A tree failure on a successful action sets the tree fields to null and adds a warning. A failed action includes `state` only when the tree could be read afterward.

Read-only actions ignore both flags: `tree`, `get_state`, `list_apps`, `list_windows`, `displays`, `doctor`, `wait`, `cursor_position`, `session_end`, and `session_prune`.

### Screenshot map
Native CLI coordinates stay logical screen points. Adapters speak in pixels of the last image for that session.

```json
{
  "width": 1440,
  "height": 900,
  "scale": 2.0,
  "logical_width": 800,
  "logical_height": 500,
  "origin": {"x": 120, "y": 40},
  "fit": "width",
  "pad": {"x": 0, "y": 0},
  "content_width": 1440,
  "content_height": 900,
  "map": {
    "image_to_screen": "screen = origin + (image - pad) * logical_size / image_content_size",
    "screen_to_image": "image = pad + (screen - origin) * image_content_size / logical_size"
  }
}
```

`content_width` and `content_height` are the drawn image inside any letterbox. `pad` is only the top-left inset, so a leftover odd pixel stays on the right or bottom and the formula stays exact. `image_content_size` in the formula is that content size.

`image_util::image_to_screen` and `screen_to_image` are pure functions with tests for a 2x Retina window, a non-zero origin, a width cap, and a letterboxed `--canvas 1440x900`. Default fit is width 1440, aspect preserved. The image is not scaled up. `--canvas 1440x900` letterboxes and records `pad`. When both `--canvas` and `--max-width` are set, the canvas wins. `scale` stays the capture backing scale, not the downscale ratio.

### Input tiers
Accessibility actions (`press`, `set_value`, `focus`, `perform`, `select_text`, `scroll_to`, and `type --element` when the value is settable) stay on `ax` and do not fall back.

Coordinate click, move, scroll, drag, mouse down and up, `type` without an element, `type --element` when the value is not settable, `key`, and `hold_key` use `event`, then `activate` when `event` returns `execution_failed`, the caller did not set `--require-background`, and another session does not hold `foreground.lock`. `synthetic` runs only when requested. `--tier ax` on those actions is `unsupported` and tells the caller to pass `--element` for an accessibility action. The event poster maps mouse-dragged types, including other-mouse button numbers for back and forward.

On Linux P13a adds a real `event` tier on plain X11 (XTest `fake_input` through `x11rb`); the auto plan keeps `activate` first and only falls back to `event` when the caller names it. The auto plan stays `activate`-only on Wayland and Windows, where explicit `--tier event` returns `unsupported` and names the OS. Explicit `--tier synthetic` remains `unsupported` outside macOS. Explicit `--tier activate` is enigo on Linux and Windows. macOS `activate` also takes the lock and restores the previous frontmost app.

`CGEventPostToPid` returns void, so a posted event is treated as sent. Failure is `permission_required` when posting is not allowed, `execution_failed` when no target window can be resolved, or `execution_failed` when the cursor moved by more than one point and `--require-background` is set.

| Tier | Flag | Cursor and focus | Mechanism |
| --- | --- | --- | --- |
| `ax` | `--tier ax` | Untouched | `AXUIElementPerformAction` / set attribute. |
| `event` | `--tier event` | Untouched | `CGEventPostToPid` with window id and window-local point. |
| `synthetic` | `--tier synthetic` | Untouched if the private call works | SkyLight/CPS front-process illusion. Feature `skylight`. |
| `activate` | `--tier activate` | Moves, then restored | `NSRunningApplication::activateWithOptions` plus enigo, then reactivate the previous app. |

Tier `activate` takes `foreground.lock`. A second process that asks for `activate` receives `execution_failed` naming the holder pid. `--allow-foreground` skips the lock. Parallel agents use `ax` and `event`.

### Batch
```json
{
  "id": 1,
  "action": "batch",
  "stop_on_error": true,
  "return_state": true,
  "actions": [
    {"action": "press", "element": 12},
    {"action": "set_value", "element": 4, "value": "hello"}
  ]
}
```

`stop_on_error` defaults to true. The response `results` array has one object per attempt. `ok` is false if any attempt failed. `stopped_at` is the index of the first failure when `stop_on_error` skips the rest. It is omitted when `stop_on_error` is false. `error.code` is that first failure's code.

Each step may set `element`, `app`, `window`, and the other shared flags. Those overlay the outer request. `return_state` and `include_screenshot` on a step are ignored. The outer `return_state` runs once after the last attempt, or after the failure that stops the batch, and uses the outer target. A nested `batch` is `invalid_arguments`. The steps share one session and one cache revision sequence. Each step re-validates.

### Adapters
`exec` and stdin consult `adapter` before `parse_request`. `native` is today's schema. Unknown adapter actions return `invalid_arguments` and name the field they did not understand. Fixture files live in `tests/fixtures/openai/` and `tests/fixtures/anthropic/`. Coordinates in both adapters are image pixels of the session's last screenshot and go through `image_to_screen`. A missing map returns `invalid_arguments` telling the caller to screenshot first.

OpenAI `computer` tool (the action object, or that object nested under `action`):

| `type` | Maps to |
| --- | --- |
| `screenshot` | `screenshot` of the target window |
| `click` | `press` or coordinate click. `button` left/right/middle/back/forward. `wheel` is middle. |
| `double_click` | click count 2 |
| `scroll` | `scroll` with `scroll_x` / `scroll_y` |
| `keypress` | `key` from the `keys` array |
| `type` | `type` |
| `move` | `move` |
| `drag` | `drag` along `path` |
| `wait` | `wait` |

Anthropic `computer_20251124`:

| `action` | Maps to |
| --- | --- |
| `screenshot` | window screenshot |
| `left_click`, `right_click`, `middle_click` | click at `coordinate: [x, y]` |
| `double_click`, `triple_click` | click count 2 or 3 |
| `mouse_move` | `move` |
| `left_click_drag` | `drag` from `start_coordinate` to `coordinate` |
| `type` | `type` |
| `key` | `key` from `text` (`ctrl+s`) |
| `hold_key` | `hold_key` for `duration` seconds |
| `scroll` | `scroll` using `scroll_direction` and `scroll_amount` |
| `wait` | `wait` |
| `zoom` | `zoom` of `region: [x1, y1, x2, y2]` in image pixels |
| `cursor_position` | read-only `cursor_position` |
| `left_mouse_down`, `left_mouse_up` | `mouse_down` / `mouse_up` at `coordinate` |

If a published patch renames a field, accept the alias in the adapter and add a fixture. Do not guess a second schema in the dispatcher.

OpenAI `wheel` is `middle`. OpenAI `back` and `forward` are mouse buttons 4 and 5. OpenAI `wait` with no duration sleeps 0 ms, because that schema has no duration. Anthropic `hold_key` converts `duration` seconds to milliseconds and refuses a value above 30 seconds. Anthropic `zoom` stays in image pixels on the translated action. Dispatch remaps it with the stored map. The far edge of a region may sit on the image width or height. A point may not.

Adapters force `coord` to `image`, including when the request also sent `coord`. A missing `sessions/<session>/screenshot-map.json` is `invalid_arguments` with the message `take a screenshot first`. A coordinate outside the image is `invalid_arguments`. The response adds `adapter` for `openai` and `anthropic` and omits it for native. There is no detection without `--adapter` or the request field. An action with no coordinates does not need a map.

### Policy shape (P6)
```toml
version = 1
default = "deny"          # or "allow"
pause_on_user_input = true

[[allow]]
bundle_id = "com.apple.Notes"

[[allow]]
bundle_id = "com.apple.Safari"
path = "/System/Volumes/Preboot/Cryptexes/App/System/Applications/Safari.app"

[[deny]]
bundle_id = "com.apple.Terminal"

[confirm]
command = "/usr/local/bin/computer-use-confirm"
timeout_secs = 30

[browser]
blocked_urls = ["https://accounts.example.com/*", "file://*"]
```

Resolution order: built-in deny, then admin policy, then user policy. Deny wins over allow. Admin policy can only add denies and raise confirmation. It cannot allow an app the built-in list refuses. Admin `allow` entries are ignored and do not grant access. `default = "deny"` means a named app that is not listed is `policy_denied`. A desktop screenshot that names no app is not denied by that default. An unscoped mutating action under `default = "deny"` is `policy_denied`. Ship the example file as `docs/policy.example.toml` in P6 with `default = "allow"` commented, so a missing file does not lock the user out. A missing file uses `default = "allow"` plus the built-in deny list. `pause_on_user_input` is false when the key is absent, so the first action does not install a tap. It becomes true when either loaded file sets it. An admin true stays true. The user file cannot turn it off. An explicit `--policy` path that is missing is `invalid_arguments`. `COMPUTER_USE_ADMIN_POLICY` set to a path that is not a file means there is no admin file. A missing admin file is not an error. A present admin file that fails to parse is `policy_denied` and fails closed. A whitespace-only file is `default = "allow"`.

Built-in deny, matched on bundle id and on the canonical executable path:

- Terminals: `com.apple.Terminal`, `com.googlecode.iterm2`, `dev.warp.Warp-Stable`, `org.alacritty`, `net.kovidgoyal.kitty`, `com.mitchellh.ghostty`, `com.github.wez.wezterm`, `co.zeit.hyper`, `app.crynta.terax`, Windows Terminal (`Microsoft.WindowsTerminal`), `powershell.exe`, `cmd.exe`, `wt.exe`, `gnome-terminal`, `konsole`, `xterm`, `wezterm`, `kitty`, `alacritty`, Hyper.
- Password managers: 1Password, Bitwarden, KeePassXC, Dashlane, Enpass, LastPass (`com.lastpass.LastPass`, `com.lastpass.lastpassmacdesktop`), Apple Passwords (`com.apple.Passwords`).
- `com.apple.keychainaccess`.
- System Settings (`com.apple.systempreferences` and `com.apple.Settings`) when the window title or AX identifier contains `Privacy` or `Security`. Those bundles are not on the hard-deny list. Other System Settings windows stay reachable unless the user denies the bundle. Activate with no window still succeeds when a privacy window exists.

Identity is the canonical executable path from `NSRunningApplication.executableURL` (macOS), `QueryFullProcessImageName` (Windows), or `/proc/<pid>/exe` (Linux), after symlink resolution. A bundle id match with a path that does not match an `allow.path` is `policy_denied`. Comparison is on the canonical path, not the argv the process was started with.

### Confirm hook
Stdin is one JSON object: `action`, `app.bundle_id`, `app.path`, `pid`, `window_id`, `element`, `reason`. Stdout is ignored. Exit 0 allows. Any other exit or a timeout yields `confirm_denied`. Sensitive set:

- Always `policy_denied`, hook or not: `set_value` or `type` into a secure text field. Key events aimed at a secure field.
- Confirm: `quit`, `clipboard_set`, click or `AXPress` whose title matches `(?i)\b(delete|erase|pay|purchase|send|submit)\b`, key combos `cmd+q` and `alt+f4`.
- No hook configured: those confirmed actions return `confirm_required` and do not run.

The hook is `--confirm-cmd`, or `COMPUTER_USE_CONFIRM_CMD`, or `confirm.command` in the policy. The flag wins, then the environment, then the user file, then the admin file. `policy check` returns `allow` or `deny` only. Confirm is action-specific. Quit of an allowed app always confirms, including `--force`. Protected quit (this process, loginwindow, WindowServer) is `policy_denied` before confirm. A key event without `--element` cannot see a secure focused field, so that case is not refused as a secure-field key. `cmd+q` is Command+q. Meta or super+q does not confirm. `clipboard_set` is classified here and implemented in P8. `quit --force` uses `forceTerminate()` on macOS. Linux and Windows still return `unsupported` after a successful confirm.

### Audit line
One JSON object per line, append-only:

```json
{"ts":"2026-10-18T12:00:00Z","session":"9","action":"press","bundle_id":"com.apple.Notes","pid":412,"window_id":88,"element":12,"code":"ok","decision":"allow","source":"default","tier":"ax"}
```

Never write `text`, `value`, clipboard bytes, or URLs that were blocked. `code` is the outcome (`ok` or the error code), not a constant. `decision` is `allow`, `deny`, or `confirm`. `source` is `builtin`, `admin`, `user`, `default`, or another policy source. `policy check` can return `ok: true` with `decision: "deny"`; that audit line keeps `code: "ok"` and records `decision: "deny"`.

### Kill switch
On the first action of a process, if `pause_on_user_input` is true, install a listen-only `CGEventTap` (`kCGSessionEventTap`, `kCGHeadInsertEventTap`, `kCGEventTapOptionListenOnly`) for key and mouse events. The tap lives in `src/backend/macos/input.rs`. P7 stamps synthetic events with `CGEventSource::set_user_data` and `CGEvent::set_integer_value_field` on `CGEventField::EventSourceUserData` (field 42, constant `0x00C0_5EC0_0DE1`, `policy::SYNTHETIC_EVENT_USER_DATA`). Field 43 is `kCGEventSourceUserID` and is not the marker. The tap ignores events carrying that marker. Any other real user event sets `paused`. The `paused` file next to the audit log is the cross-process source of truth. `computer-use pause` and `resume`, and the JSON actions, set and clear that file. While paused, mutating actions return `paused` after deny, so a denied tree or screenshot stays `policy_denied`. Read-only actions are not pause-blocked. If the tap cannot be created, `doctor` reports `input_monitor: false` and actions still run. The warning `user-input pause unavailable` is added only when `pause_on_user_input` is true and the tap is unavailable. Other platforms use that same warning until P12 and P13. Tests never create a real event tap.

## Test strategy used by every phase
- Unit tests call pure functions or `StubBackend`. They do not look at the real desktop. Extend `StubBackend` in the same phase that extends the trait.
- Tree, diff, policy, remap, adapter, and key tests live next to the module.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test --all-targets --all-features` pass on Ubuntu, macOS, and Windows. P1 updates CI to pass `--all-features`.
- Linux CI gains `libpipewire-0.3-dev` only on a separate job that runs `cargo check --features portal`. The default job stays free of PipeWire until that job exists (P13).
- Live smoke is opt-in: `COMPUTER_USE_LIVE=1 cargo test --test live -- --ignored` on a Mac that has Accessibility and Screen Recording for the terminal. It is not part of GitHub Actions. The live test launches TextEdit, types via `set_value`, and quits TextEdit. Document the permission prerequisite at the top of `tests/live.rs`.
- Off-platform commands are tested on every OS: the new subcommand is in `--help`, and invoking it on the wrong OS returns `unsupported` where the backend is missing. `doctor` is the exception: it runs on every OS and reports which probes that OS has.

---

## P1 — Targeting, inventory, and doctor
Shared infrastructure. Later phases call `target::resolve` and the built-in deny list.

### Design
Add `src/target.rs`, `src/policy.rs` (built-in deny only), `src/backend/macos/apps.rs`, `src/backend/macos/doctor.rs`, and empty `mod.rs` files for `macos/`, `windows/`, `linux/` that re-export the current backend types. Move `macos.rs`, `linux.rs`, and `windows.rs` into those directories without behavior changes other than the new methods.

`target::resolve(app, window)` returns `ResolvedTarget { pid, bundle_id, display_name, exe_path, window_id, window_title, bounds }`.

macOS apps (`objc2-app-kit` 0.3.2):

- `NSWorkspace::sharedWorkspace` then `runningApplications`.
- Keep `NSApplicationActivationPolicyRegular`. `--all` adds accessory and prohibited.
- Read `localizedName`, `bundleIdentifier`, `processIdentifier`, `executableURL`, `isActive`, `isHidden`.
- Canonicalize `executableURL` with `std::fs::canonicalize`.
- Launch with `openApplicationAtURL:configuration:completionHandler:` (macOS 11). Resolve a bundle id via `URLForApplicationWithBundleIdentifier:`. Resolve a `.app` name by scanning `/Applications`, `~/Applications`, and `/System/Applications`.
- Activate with `activateWithOptions:` and `NSApplicationActivateIgnoringOtherApps`.
- Quit with `terminate`. Refuse `forceTerminate` until P6 wires confirm. Refuse to quit `loginwindow`, `WindowServer`, and the current process.

macOS windows (`objc2-core-graphics` 0.3.2, feature `CGWindow`):

- `CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements, kCGNullWindowID)`.
- Array order is z-order, index 0 in front.
- Read `kCGWindowNumber`, `kCGWindowOwnerPID`, `kCGWindowOwnerName`, `kCGWindowName`, `kCGWindowBounds`, `kCGWindowLayer`.
- Match an AX window, when permission allows, by pid plus bounds within 2 points. Store `ax_matched: true|false`. P1 may skip the AX join and set `ax_matched: false` with a note. P2 fills it in.

`doctor` (`accessibility-sys` 0.2.0 and `objc2-core-graphics`):

- `AXIsProcessTrustedWithOptions`. Default prompt flag false. `--request` passes `kAXTrustedCheckOptionPrompt` true.
- `CGPreflightScreenCaptureAccess`. `--request` also calls `CGRequestScreenCaptureAccess`.
- Do not prompt on a normal `doctor` call. Agents must not block on a dialog unless `--request` was passed.
- Linux and Windows `doctor` return `ok: true` with `screen_capture: null` and `note` explaining that the capture probe lands in P12b / P13. P12a already reports `accessibility = UIAutomation::new` succeeded for Windows; Linux accessibility lands in P13.

Built-in deny runs inside `launch`, `activate`, and `quit` immediately. A missing policy file does not deny ordinary apps.

### CLI
```
computer-use list-apps [--all]
computer-use list-windows [--app <sel>]
computer-use launch <app>
computer-use activate --app <sel> [--window <sel>]
computer-use quit --app <sel>
computer-use doctor [--request]
```

`--app` and `--window` are global from this phase. Existing commands accept them and ignore them except `get-state`, which uses `--app` as the JXA process name it already embeds. P2 makes every command honor them.

### JSON
```json
{"action":"list_apps","all":false}
{"action":"list_windows","app":"Notes"}
{"action":"launch","app":"com.apple.TextEdit"}
{"action":"activate","app":"TextEdit","window":0}
{"action":"quit","app":412}
{"action":"doctor","request":false}
```

`list_apps` result: `{"apps":[{"name","bundle_id","pid","path","active","hidden"}]}`.
`list_windows` result: `{"windows":[{"id","pid","app","title","z","bounds":{"x","y","width","height"},"layer","on_screen","ax_matched"}]}`.
`doctor` result: `{"accessibility":true,"screen_capture":false,"input_monitor":null,"fixes":["System Settings → Privacy & Security → Screen Recording → enable computer-use"]}`.

### Failure modes
`app_not_found`, `window_not_found`, `policy_denied` for the built-in list, `permission_required` when a window list needs Screen Recording and the call fails, `unsupported` off macOS for launch/activate/quit.

### Tests
- Canonical-path join and bundle-id parser, including a name with a quote, against a fake app table in `target.rs`.
- Built-in deny matches Terminal by bundle id and by a fixture path.
- z-order is the CG array index. Feed a fake window-info list. Do not call `CGWindowListCopyWindowInfo` in unit tests.
- CLI help on all three OS includes the new commands. `doctor` returns JSON on all three OS.
- `tests/cli.rs` version assertion stays `0.1.0`.

### Implementation notes

- A bare `--window` number is the z-order index. A window id uses the `id:` prefix (`ID:` and `index:` are accepted too) so a pid-sized window number is not read as an index.
- App search scans `/Applications`, `~/Applications`, `/System/Applications`, and `/System/Applications/Utilities`, depth 2. Utilities is both its own root and a child of `/System/Applications`, so Terminal.app is findable without walking into a `.app` bundle. Directory symlinks are not followed.
- `activate` brings the application forward with `NSApplicationActivateIgnoringOtherApps`. It does not raise one accessibility window. That raise lands with the AX engine.
- `list-windows` sets `ax_matched: false` and notes that the CG-to-AX join is P2. A null window list while Screen Recording is off is `permission_required`. Empty titles with a non-null list are still success.
- Parent walk uses `proc_pidinfo` flavor `PROC_PIDT_SHORTBSDINFO`. The parent field on that struct is `pbsi_ppid`. Agent hosts such as `grok` are reparented to launchd, so that walk can stop before a `.app`. When no ancestor bundle exists, `doctor` calls `responsibility_get_pid_responsible_for_pid` in libSystem. That call returns the pid TCC checks, or `-1`. The sibling `responsibility_get_responsible_for_pid` takes four arguments `(pid, *pid_out, *u64_out, *extra)`. A two-argument call leaves the last register uninitialized; the entry loads through it and either faults or returns `-1`.
- `doctor` on Linux returns the P13 note. Windows `doctor` reports `accessibility` from `UIAutomation::new` and a note that screen-capture lands in P12b. `--request` does not prompt on those platforms.

## P2 — Native macOS accessibility tree
Replaces the JXA tree for observations. `osascript` remains a fallback when `AXUIElementCreateApplication` returns an error that is not a permission error, and the response `note` says `source=osascript`.

### Design
`src/backend/macos/ax.rs` and pure `src/tree.rs` plus `src/cache.rs`.

Calls, all through `objc2-application-services` 0.3.2 feature `AXUIElement`:

- `AXUIElementCreateApplication(pid)`.
- `AXUIElementSetMessagingTimeout(elem, 2.0)` on every element before a copy, so a wedged app cannot hang the process. The JXA path used an 8s process kill. Keep a 2s per-element timeout and an 8s budget for the whole tree.
- `AXUIElementCopyAttributeValue` for `AXRole`, `AXSubrole`, `AXRoleDescription`, `AXTitle`, `AXDescription`, `AXValue`, `AXPlaceholderValue`, `AXHelp`, `AXIdentifier`, `AXPosition`, `AXSize`, `AXChildren`, `AXFocused`, `AXEnabled`, `AXURL`, `AXSelectedText`, `AXParent`.
- `AXUIElementCopyAttributeValues` for children when the child array is large, with an index range, so a huge `AXChildren` is not materialized past the cap.
- `AXUIElementCopyActionNames`.
- `AXUIElementGetPid` to detect hosted content.

Electron and Chromium, on first attach to a pid, before the walk:

- Read `AXManualAccessibility` and `AXEnhancedUserInterface`.
- Set both to true when they are false or missing.
- Remember the previous values in the session cache.
- Restore them from a `Drop` guard on the session and from `computer-use session end`. A crash can leak the flag until the target app quits. Document that. Do not restore a value we did not change.

Hosted content: if the key window's children are empty, or a child's pid differs from the app pid, call `AXUIElementCreateApplication` on the child pid and walk that tree under the same window node. Discover helper pids from the child element first. If that yields nothing, scan `runningApplications` for processes whose bundle id contains the parent name and whose parent pid equals the target (`proc_pidinfo` / `PROC_PIDT_SHORTBSDINFO.pbsi_ppid`). Cap helper walks at 3 pids. The parent field is `pbsi_ppid`; `pbi_ppid` belongs to the longer `proc_bsdinfo` struct, which this walk does not use.

A crash, or a library caller that never calls `shutdown_sessions`, skips the restore. A flag this process turned on can stay set until the target app quits. `computer-use session end` in a later process reads the session cache and restores it. The restore never writes an attribute this process did not change.

Key window: the AX window whose `AXMain` is true, else the first AX window. `--window` overrides this. Join to the CG window from P1 by bounds.

`AXObserverCreate` plus `AXObserverAddNotification` for `AXFocusedWindowChanged` and `AXValueChanged` is started lazily and only used to mark the cache dirty. The CLI is still request/response. The observer runs on a background thread with a run-loop that `session end` stops. If the observer fails, every call re-fetches, which is already the validation rule.

Render with `tree.rs`. Store the snapshot in the session cache. `--diff` emits the diff when a baseline exists and the full tree otherwise. `--full` always emits the full tree and replaces the baseline.

`get-state` grows `tree`, `revision`, `window_id`, and `pid`. The old JXA JSON moves to `accessibility_legacy` for one release when the fallback runs, and is `null` when the native tree exists.

### CLI
```
computer-use tree [--app <sel>] [--window <sel>] [--diff | --full]
computer-use get-state --app Notes --window 0 --diff
```

`tree` is the observation command. It does not click.

### JSON
```json
{"action":"tree","app":"Notes","window":0,"diff":true}
```

```json
{"ok":true,"pid":412,"window_id":88,"revision":3,"source":"ax","truncated":false,"tree":"[0] AXWindow \"Notes\" @0,0 800x600\n"}
```

### Failure modes
`permission_required`, `app_not_found`, `window_not_found`, `execution_failed` on `AXError` other than the trusted-process error, timeout as `execution_failed` with `messaging timeout`. A SwiftUI tree that used to crash Codex is just data: the walker never assumes a child array is non-null, and a copy error on one node becomes `role=unavailable` for that node only.

### Tests
- Fixture nodes in `tree.rs`: numbering, child cap, depth cap, URL strip, secure redaction, diff LCS, `--full` baseline replacement.
- Cache round-trip under a temp `XDG_CACHE_HOME`.
- Electron flag helper: given a fake attribute map, set-and-restore returns the previous values.
- Hosted-pid walker: a window with one foreign child pid is included once.
- CLI `tree` on Linux is real as of P13a through AT-SPI (see the Linux parity rows); P12a implements the same tree path on Windows through `uiautomation`.
- No live AX call in `cargo test`.

## P3 — Element actions
### Design
Actions go through the bound element from P2. Each one re-validates, then tries `AXScrollToVisible` (from `CopyActionNames`) when the element frame sits outside the window frame. Scroll failure is a warning, not a hard error, unless `--require-visible` is set.

| Action | API |
| --- | --- |
| `press` | `AXUIElementPerformAction(elem, AXPress)`. `--count` 2 or 3 repeats the action. |
| `focus` | `AXUIElementSetAttributeValue(elem, AXFocused, true)`. |
| `set_value` | `AXUIElementSetAttributeValue(elem, AXValue, string)`. Refuse secure fields. |
| `select_text` | `AXUIElementSetAttributeValue(elem, AXSelectedTextRange, CFRange)`. `AXSelectedTextRange` is an `AXValue` of type `kAXValueCFRangeType`. |
| `perform_action` | `PerformAction` with an explicit name. Allowlist: `AXPress`, `AXShowMenu`, `AXIncrement`, `AXDecrement`, `AXConfirm`, `AXCancel`, `AXRaise`, `AXScrollToVisible`, `AXScrollUpByPage`, `AXScrollDownByPage`, `AXScrollLeftByPage`, `AXScrollRightByPage`. Other names are `invalid_arguments`. |
| `scroll_to` | `AXScrollToVisible` only. |

`click --element N` is an alias of `press`. Success then uses `action: "press"`. `--double` with `--element` is press count 2. `click --x --y` stays the coordinate path and, on macOS after this phase, hit-tests first. The result includes `hit` (`role` and `title`, or null). A hit-test error is a warning and the click still runs. Passing both an element and coordinates is `invalid_arguments`.

`type` prefers `set_value` when `--element` is set and `AXValue` is settable (`AXUIElementIsAttributeSettable`). That success uses `tier: "ax"` and `via: "set_value"`. Otherwise it focuses the element and posts the text to that process (`tier: "event"`, `via: "event"`) and does not activate the app. Without `--element`, `type` uses the P7 tier plan. P3 used enigo for those two paths. P7 replaced that so the event tier does not move the cursor. The window id on a non-settable element post is 0: the event still carries the pid and the synthetic marker.

Frame revalidation allows 2 points on each edge. Past that, or when the role or identifier changed, the result is `stale_element` and the action is not performed. The fresh tree is a sibling `tree` field. Element actions update `observed` in the session cache and do not replace the `tree --diff` baseline. `scroll-to-visible` is an alias of `scroll_to`. Secure fields (`AXSecureTextField` or subrole containing `Secure` on macOS; `IsPassword` on Windows) refuse `set_value`, `select_text`, and `type --element` with `policy_denied`. The value is never read. The built-in deny list runs before any accessibility call on the target. P12a implements the same stale_element contract on Windows through the `uiautomation` control view walker and a revalidation gate. Linux implements the same revalidation gate over AT-SPI as of P13a.

### CLI
```
computer-use press --app Notes --element 12 [--count 2]
computer-use set-value --element 4 --value "hello"
computer-use select-text --element 4 --start 0 --end 5
computer-use perform-action --element 9 --name AXShowMenu
computer-use scroll-to --element 9
computer-use focus --element 4
```

### JSON
```json
{"action":"press","app":"Notes","element":12,"count":1}
{"action":"set_value","element":4,"value":"hello"}
{"action":"select_text","element":4,"start":0,"end":5}
{"action":"perform_action","element":9,"name":"AXShowMenu"}
{"action":"scroll_to","element":9}
{"action":"focus","element":4}
```

Success includes `tier: "ax"`, the `element` index, and `revision` after the action.

### Failure modes
`stale_element`, `element_not_found`, `policy_denied` on secure `set_value`, `invalid_arguments` on a name outside the allowlist, `execution_failed` when the element has no such action (`AXUIElementCopyActionNames` is checked first so the message lists the actions the element does have).

### Tests
- Revalidation table: same role allows, changed role, identifier, or frame returns `stale_element` and records zero performs on the stub.
- Secure node refuses `set_value`, `select_text`, and `type`.
- Action allowlist rejects `AXDelete`.
- `scroll_to` is attempted when the fixture frame is outside the window. `perform_action AXScrollToVisible` does not scroll twice.
- Stub records the action name and the index.
- A deny-listed app returns `policy_denied` and records zero performs.
- CLI parsing covers each new command. JSON `exec` covers the action names, including hyphen aliases. Off macOS the commands return `unsupported`. No live AX call in `cargo test`.

## P4 — Per-window capture and coordinate remap
### Design
`src/backend/macos/capture.rs` uses `objc2-screen-capture-kit` 0.3.2.

1. `SCShareableContent` excluding desktop windows, on-screen windows only, then a second query that includes off-screen windows when the CG window id was not in the first list. Popups and occluded windows are the point of the second query.
2. Build `SCContentFilter` with `initWithDesktopIndependentWindow:` for that `SCWindow`. Desktop-independent capture shows the window even when another window covers it.
3. If the AX window has an attached sheet (`AXSheet` or `AXModal`), capture a filter for the sheet's window as well and composite the sheet above the parent in an image the size of the union. The map's origin is the union's top-left in screen points.
4. `SCStreamConfiguration`: pixel `width` and `height` equal the window size in points times `NSScreen.backingScaleFactor` for the screen that contains the window origin. `showsCursor = false`. `ignoreShadows = true`. `capturesAudio = false`.
5. `SCScreenshotManager.captureImageWithFilter:configuration:completionHandler:` on macOS 14 and newer. On older systems, or when that call fails, start an `SCStream`, take one `CMSampleBuffer`, and stop the stream.
6. Fallback: crop the xcap desktop image to the window bounds. Set `capture_source: "xcap"` and `occluded: true` in the JSON so the agent knows covered pixels may be wrong. `capture_source: "sck"` is the success path.

Encode through `image_util`. Default `--max-width` becomes 1440 in this phase. A caller who needs full pixels passes `--max-width 0`, which means "do not scale" and is a new meaning that `invalid_arguments` used to cover. Document it in the changelog in P15. Retina: a 800×500 point window at scale 2 is a 1600×1000 buffer, then scaled to 1440×900 if max-width is 1440.

Attach `origin`, `scale`, `logical_width`, `logical_height`, `fit`, `pad`, and `map` to every screenshot result. Store the map in the session cache.

`screenshot` without `--window` keeps today's full-desktop xcap behavior and still applies the new default max-width and the map (origin 0,0 of the virtual desktop, or of the region).

### CLI
```
computer-use screenshot --app Notes --window 0 --max-width 1440
computer-use screenshot --app Notes --canvas 1440x900 --base64
```

### JSON
The success object is the map object from [Screenshot map](#screenshot-map) plus `path`, `format`, `capture_source`.

### Failure modes
`permission_required` when Screen Recording is off (`CGPreflightScreenCaptureAccess` is false or SCK returns the TCC error). `window_not_found` when the CG id is absent from `SCShareableContent` and a display is actually available. A locked session (`CGSSessionScreenIsLocked`), an asleep display (`CGDisplayIsAsleep`), ScreenCaptureKit returning no displays or no windows, or an empty xcap monitor list is `display_unavailable`. That check runs before `window_not_found` and before the xcap crop. A window-targeted screenshot asks the same question before it resolves the window, so a locked session is not reported as a missing window index. Fallback to xcap on other SCK errors, with `capture_source: "xcap"`.

### Tests
- Remap tests in `image_util.rs` for scale 2, origin (120, 40), max-width 1440, and a 1440×900 canvas with padding.
- A fake RGBA frame runs through encode and the map, and a known pixel round-trips within one logical point.
- Default max-width is applied by `dispatch` when the field is absent. Explicit `max_width: 0` keeps the source width.
- CLI test does not capture. It checks that `screenshot --help` mentions `--canvas`.

### Implementation notes
`dispatch` applies max-width 1440 when `max_width` is absent. `--max-width 0` and JSON `max_width: 0` keep the source width. That used to be `invalid_arguments`. The changelog entry for the new meaning lands in P15.

`SCStreamConfiguration` has no `ignoreShadows` setter. Window capture sets `ignoreShadowsSingleWindow`. `captureImageWithFilter` takes `SCStreamConfiguration`, not `SCScreenshotConfiguration`. Pixel width and height are the window's point size times the backing scale of the screen that contains the window origin. Completion handlers wait at most 5 seconds and pump the main run loop so a main-queue callback cannot hang the CLI. A timeout leaks the block instead of freeing it under a late callback.

The map file is `sessions/<session>/screenshot-map.json`, mode `0600`. It is separate from `<bundle>-<pid>-<window>.json`. Screenshot does not refresh the `tree --diff` baseline. Element actions update `observed` and leave the baseline. P5 `return_state` calls the tree snapshot, and that call replaces the baseline.

Full-desktop origin is the top-left of the stitched virtual desktop in screen points (the minimum monitor origin). That is `(0, 0)` when the primary display starts there. A region uses the region origin. A single monitor uses that monitor's origin. Mixed-scale monitors keep one primary scale on the stitched image; the map still divides logical size by content size.

This crate does not call `CGWindowListCreateImage`. xcap remains the desktop path and the window fallback (`capture_source: "xcap"`, `occluded: true`). Success is `capture_source: "sck"`. The stub backend reports `capture_source: "stub"`.

Sheets are read from Accessibility and composited above the parent. Accessibility off, or a sheet with no matching `SCWindow`, skips the sheet and can add a warning. The parent capture still succeeds. Windows has no window list yet, so `screenshot --app` returns `unsupported` there. Linux still has none either — P13b built the capture path but left `list_windows` `unsupported`, so the portal is not reachable from the CLI yet. A crop helper exists for when bounds are known.

`--path` is an alias of `--output`.

## P5 — Post-action state, batch, and schema adapters
### Design
`dispatch_inner` wraps every successful mutating action. When `return_state` is set it calls the same snapshot path as `tree` and, if `include_screenshot` is set, the P4 capture. Read-only actions (`tree`, `list_apps`, `doctor`, `displays`, `get_state`) ignore `include_screenshot` unless the action is `screenshot` itself.

`Action::Batch` holds a `Vec<Action>` and `stop_on_error`. The batch shares one resolved target and one cache revision sequence. Each step re-validates.

`src/adapters/openai.rs` and `src/adapters/anthropic.rs` translate into `Action` plus the shared flags. They set `coord = image`. They do not talk to the backend.

`exec` and `serve` gain `--adapter`. A request may override with `"adapter":"openai"`. Detection without the flag stays off, so a native payload cannot be misread.

### CLI
```
computer-use press --element 1 --state --screenshot
computer-use exec --adapter openai '{"type":"click","x":10,"y":20,"button":"left"}'
computer-use exec --adapter anthropic '{"action":"left_click","coordinate":[10,20]}'
computer-use session prune --dry-run
computer-use session prune --ttl 86400
```

`--adapter` applies to `exec`, `serve`, and `--json`. A desktop subcommand with `--adapter openai` or `--adapter anthropic` is `invalid_arguments`. Batch is JSON-only. A clap subcommand for batch is not worth a second syntax.

```json
{"action":"session_prune","dry_run":true,"ttl_secs":86400}
```

### JSON
See [Post-action state](#post-action-state) and [Batch](#batch). Adapter output is a normal `Response`. Extra field `adapter` echoes the name.

### Failure modes
Batch: first error sets `ok: false` and `stopped_at`. Adapter: missing screenshot map, unknown action name, coordinate outside the image. `include_screenshot` failure does not roll back the action. The action result stays, `state.screenshot` is null, and `warnings` explains the capture error.

### Tests
- Each fixture in `tests/fixtures/openai/*.json` and `tests/fixtures/anthropic/*.json` translates to the expected `Action`. Drag, click back, triple-click, left-click drag, mouse down/up, and hold key expect the translated action. A fixture that sets `error_code` expects that error.
- Image coordinate (100, 50) on a stored map becomes the expected screen point: origin `(120, 40)`, logical `800×500`, content `1440×900`, pad `0` rounds to `(176, 68)`.
- Batch stops on the second action when the stub fails the first and `stop_on_error` is true (`stopped_at` is 0 and the second click is not attempted). With `stop_on_error` false the stub records both attempts.
- `return_state` on the stub includes the numbered tree from `backend.tree` (the same accessibility query list `inspect_accessibility` records). That diff becomes the new `tree --diff` baseline. Without `return_state` the baseline stays put.
- `include_screenshot` failure keeps the action result, sets `state.screenshot` to null, and adds a warning.
- `display_unavailable` is a pure function of lock, sleep, and display counts. The unit test uses fake inputs and does not call Core Graphics.
- `session prune` on a temp directory removes empty and stale sessions and keeps a live owner's content.

### Implementation notes
Post-action `tree` is `TreeReport.full_text`. The `tree` command's `tree` field is still the chosen full-or-diff text. `cursor_position` is a small read-only action because the backend already exposes the pointer. P8 part A implements drag, triple-click, mouse down and up, `hold_key`, and OpenAI back/forward. OpenAI `wait` without a duration is 0 ms so a fixture does not sleep. Batch applies `return_state` once at the end. A step's own `return_state` is ignored so the batch does not snapshot between steps.

## P6 — Safety
### Design
Extend `src/policy.rs` to load TOML. Add `src/audit.rs`. Wire `policy::decide(target, action) -> Allow | Deny | Confirm` at the start of `dispatch_inner`, before the backend call. `wait`, `doctor`, `displays`, `list_apps`, and `list_windows` are always allowed. `tree` and `screenshot` of a denied app are denied too. Reading a password manager is not a feature.

Admin file is read first and merged. A user `allow` cannot override an admin `deny` or a built-in deny. Parse errors in the user file are `invalid_arguments` and name the line. Parse errors in the admin file are `policy_denied` with the path, so a broken admin file fails closed.

Blocked URLs: when the target bundle is a browser (Safari, Chrome, Chromium, Edge, Brave, Arc, Firefox) read `AXURL` or the window title's URL. Match the user and admin `blocked_urls` as globs (`*` only). A match returns `blocked_url` and does not press. CDP navigation in P10 uses the same function.

Confirm hook: spawn the command with the JSON document on stdin, `run_timeout` at `timeout_secs` (default 30, cap 120). See [Confirm hook](#confirm-hook).

Audit every allowed, denied, and failed action. Open the file with append and `O_NOFOLLOW` where the OS supports it.

Kill switch: implement the tap in `src/backend/macos/input.rs` even though the rest of that file lands in P7. The tap is listen-only. `computer-use pause` and `resume` write a `paused` file next to the audit log so the flag survives a new process. The in-process atomic is only a cache.

Windows and Linux honor the same TOML. Path matching uses their identity APIs. The tap is macOS-only in this phase. Other platforms add `warnings: ["user-input pause unavailable"]` when `pause_on_user_input` is true, until P12 and P13.

`quit` gains `--force` (JSON `force`, default false). macOS calls `terminate()` or `NSRunningApplication::forceTerminate()`.

### Design corrections
- The marker field is `kCGEventSourceUserData` (42), not field 43. Field 43 is `kCGEventSourceUserID`.
- `pause_on_user_input` defaults to false. A missing policy must not install a `CGEventTap` on the first action.
- `com.apple.Settings` uses the same privacy-pane rule as `com.apple.systempreferences`. It is not a hard deny.
- `toml` 0.8.23 is the dependency because `Error::span()` names the line. 0.9 was unnecessary.

### CLI
```
computer-use policy check --app "Terminal"
computer-use pause
computer-use resume
```

`--policy`, `--confirm-cmd`, and `COMPUTER_USE_CONFIRM_CMD` are global.

### JSON
```json
{"action":"policy_check","app":"com.apple.Terminal"}
{"action":"pause"}
{"action":"resume"}
```

`policy_check` returns `{"decision":"deny","source":"builtin","bundle_id":"com.apple.Terminal"}`.

### Failure modes
`policy_denied`, `confirm_required`, `confirm_denied`, `blocked_url`, `paused`, `invalid_arguments` for a bad TOML file. A missing user file is not an error.

### Tests
- TOML fixtures: default deny, admin deny beats user allow, built-in Terminal beats user allow, glob `https://accounts.example.com/*` blocks that host and allows `https://example.com/`.
- Canonical path: a symlink to Terminal still denies.
- Confirm hook tests use a shell fixture that exits 0 or 1. No network.
- Audit writer redacts a fixture action that contains `value`.
- Pause flag short-circuits `dispatch_with` before the stub records a click.

## P7 — Background input and the agent cursor
### Design
`src/backend/macos/input.rs`.

Tier `ax` is P3. This phase adds `event`, `synthetic`, and `activate`, and the auto fallback.

`event`:

- `CGEventSourceCreate(kCGEventSourceStatePrivate)` and `CGEventSourceSetUserData` to the kill-switch marker.
- `CGEventCreateMouseEvent` for down, drag, and up. Location is the window-local point: screen point minus window origin.
- `CGEventSetIntegerValueField` for the window number. Use `kCGMouseEventWindowUnderMousePointer` (field 91) because that is the public field Codex-style posters set so the event is routed inside the window. Also set `kCGEventTargetUnixProcessID` when the header exposes it.
- `CGEventPostToPid(pid, event)`.
- Keyboard uses `CGEventCreateKeyboardEvent` and the same post. Modifier flags go on the event.
- After the post, read `CGEventSource.location` of the combined session source, or `enigo`'s cursor read, and return `cursor_moved: true` if it changed by more than 1 point. The success case is `cursor_moved: false`.

`synthetic` (`src/backend/macos/focus.rs`, feature `skylight` only):

- `dlopen` `/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight` with `RTLD_LAZY`.
- `dlsym` the front-process symbols that this macOS actually exports. Look up candidates `SLPSSetFrontProcessWithOptions`, `SLSSetFrontProcessWithOptions`, and `CPSSetFrontProcess`. Use the first that resolves.
- A null symbol returns `unsupported` with the symbol name. Do not abort.
- The runtime flag `--tier synthetic` is required even when the feature is compiled.
- Document in `docs/SKYLIGHT.md` (this phase creates it) that the calls are private, version-skewed, unsigned, and unsupported across macOS updates. The file lists the symbols found on the OS where it was last tried, or "not yet run" if the author could not check.

`activate`:

- Record `NSWorkspace.sharedWorkspace.frontmostApplication`.
- Take `foreground.lock`.
- Activate the target, run the enigo action, activate the previous bundle again.
- Release the lock. On error, still restore and release.

Overlay (`src/backend/macos/overlay.rs`):

- Borderless `NSWindow`, clear background, `ignoresMouseEvents = true`, `opaque = false`, level `NSStatusWindowLevel`, collection behavior `canJoinAllSpaces | stationary | fullScreenAuxiliary | ignoresCycle`.
- Draw a 16×16 ring in a per-session color at the last agent point. This is not the system cursor.
- One-shot CLI processes cannot keep a window. `computer-use overlay serve` is a helper that reads newline-delimited `{x,y,session}` from the Unix socket and draws. The main CLI connects, sends, and moves on. If the helper is down, the action still succeeds and `warnings` includes `overlay helper not running`.
- The helper is click-through, so it never receives the events it draws under.

Auto tier: try `ax` for presses and value changes. For coordinate clicks try `event`. On `execution_failed` from `event`, try `activate` unless another session holds the lock. Never try `synthetic` unless asked.

### CLI
```
computer-use click --app Notes --x 10 --y 20 --tier event
computer-use overlay serve
computer-use click --app Notes --element 3 --tier synthetic
```

### JSON
```json
{"action":"click","app":"Notes","x":10,"y":20,"tier":"event","coord":"screen"}
```

Result adds `tier`, `cursor_moved`, and `focus_restored`.

### Failure modes
`unsupported` for `synthetic` without the feature, before any hit-test. `CGEventPostToPid` returns void, so it is not itself an error. `permission_required` when event posting is not allowed. `execution_failed` when no target window can be resolved, when the cursor moved by more than one point and `--require-background` is set, or when `foreground.lock` is held on an explicit `activate`. An `execution_failed` from `event` does not fall back to `activate` when the lock is held or `--require-background` is set; the event error is returned. `--no-overlay` skips the helper. A missing helper adds the warning `overlay helper not running` and the action still succeeds.

### Tests
- Tier selection table with a stub that fails `ax` and records which tier ran.
- User-data marker constant is the one the tap compares.
- Window-local point math: screen (200, 150) and origin (100, 100) produce (100, 50).
- Foreground lock: a second stub call with `activate` fails while the lock file is held.
- Overlay protocol parser accepts a JSON line and rejects a binary blob. Do not open an `NSWindow` in unit tests.

## P8 — More pointer and keyboard actions, clipboard, bundle
### Design
Extend `src/keys.rs` and enigo / `CGEvent` posting.

- `drag`: `path` (alias `points`) is `[{x,y}, ...]` or a flat `x,y,x,y` string, at least 2 points. Down at the first, dragged events through every later point, up at the last. Tier `event` posts mouse-dragged events to the pid. Tier `activate` uses enigo button down, move, up. Fewer than 2 points is `invalid_arguments`.
- `mouse_down` and `mouse_up` share a button. A down without a later up stays down for the rest of a `serve` loop or a `batch`. The input session releases it when the last session on that thread drops, which for a one-shot CLI command is the end of `dispatch` (the process is about to exit). A separate `mouse-down` invocation does not stay down for a later process. Say so in `--help`.
- Buttons `back` and `forward` are mouse buttons 4 and 5. Tier `event` posts `kCGEventOtherMouse` with button numbers 3 and 4. Tier `activate` uses enigo `Button::Back` and `Button::Forward` on Linux and Windows. enigo has no back or forward button on macOS, so `--tier activate` for those buttons is `unsupported` there and names the event tier. Auto on macOS still tries `event` first.
- Click `--count 3` and `triple-click` are a triple click. The event tier sets `kCGMouseEventClickState` to 1, then 2, then 3 on each down/up pair. `--button right` on `press` runs `AXShowMenu` once when the element advertises it. Otherwise, and for middle, back, and forward, the click is an event-tier (then activate) click at the element center. A missing frame is `execution_failed`.
- `hold_key`: one combination, key down, sleep `--ms`, key up. `ms` above 30000 is `invalid_arguments`. A space-separated sequence is refused. `0` is a press and release with no sleep. The stub records the duration and does not sleep.
- xdotool names: `super`, `ctrl`/`control`, `alt`/`option`, `shift`, `cmd`/`command`/`meta`, `Return`/`Enter`, `BackSpace`, `Delete`, `Escape`, `Tab`, `space`, `Up`/`Down`/`Left`/`Right`, `Home`, `End`, `Page_Up`/`Prior`, `Page_Down`/`Next`, `Insert`, `F1`–`F24`, `plus`, `minus`, and single characters. Combos use `+`. `key` also accepts a space-separated sequence (`ctrl+a Delete`). Unknown words are `invalid_arguments`. `super` is `Modifier::Meta`. F13–F20 have macOS virtual keycodes. F21–F24 are known names and return `unsupported` on macOS (enigo's macOS `Key` enum stops at F20, and they are not posted as keycode 0). Other platforms accept F21–F24 through enigo. `insert` uses the macOS Help key (virtual keycode `0x72`) because enigo has no `Insert` variant there.
- Clipboard: `arboard` 3.7.0. Linux builds enable `image-data` and not `wayland-data-control`. `clipboard_get` returns text, or `kind: "image"` plus a mode-0600 PNG in the session cache, or `kind: "empty"`. Text wins over an image. `clipboard_set` goes through `policy::decide` and is confirmed. Empty text is `invalid_arguments`. The audit stores `chars` and never the text. A macOS arboard error whose text mentions the pasteboard and permission, denied, or not allowed is `permission_required`. Tests use the in-memory stub and never touch the pasteboard.
- `zoom` captures a region through the screenshot path and returns the same payload and map. With no window it is a screen region. With `--app` or `--window` the region is window-local logical points (the far edge may equal the window width or height) and is cropped after `capture_window`. `screenshot` still refuses a region together with a window target. Anthropic `zoom` translates to this action, still in image pixels, and dispatch remaps it.
- `doctor` grows `input_monitor` when the event tap from P6 was created during this process. It stays null when this process has not tried to install the tap. The JSON gains `bundle_id` of the running binary so the user can see whether TCC is attached to a stable id. The full report is specified under [Doctor and packaging corrections](#doctor-and-packaging-corrections).

Helper app, `packaging/macos/`:

- `Info.plist` with `CFBundleIdentifier` `ai.lindoelio.computer-use`, `CFBundleExecutable` `computer-use`, `LSMinimumSystemVersion` 13.0, `NSAppleEventsUsageDescription`, and a usage string for accessibility. TCC still requires the user to toggle the switch. The plist only gives the prompt a name.
- `scripts/package-macos-app.sh` copies `target/release/computer-use` into `Computer Use.app/Contents/MacOS/computer-use` and zips it.
- Release workflow (edited in P15, script added now) runs the script on `macos-latest`. Signing runs only when `MACOS_CERTIFICATE_P12` (base64), `MACOS_CERTIFICATE_PASSWORD`, and `APPLE_TEAM_ID` secrets exist. Notarization also needs `APPLE_ID` and `APPLE_APP_SPECIFIC_PASSWORD` and uses `xcrun notarytool submit --wait`. When the secrets are absent, the artifact is named `computer-use-macos-arm64-app-unsigned.zip` and the release notes say so. Document the local `codesign --deep --force --options runtime --sign "Developer ID Application: ..."` command in `docs/PACKAGING.md`.

### CLI
```
computer-use drag --app Notes --path 10,20,200,40
computer-use mouse-down --x 10 --y 20 --button left
computer-use mouse-up --x 10 --y 20
computer-use hold-key --keys shift --ms 400
computer-use clipboard get
computer-use clipboard set --text "hello"
computer-use key --keys "super+c"
```

### JSON
```json
{"action":"drag","path":[{"x":10,"y":20},{"x":200,"y":40}],"tier":"event"}
{"action":"mouse_down","x":10,"y":20,"button":"left"}
{"action":"mouse_up","x":10,"y":20,"button":"left"}
{"action":"hold_key","keys":"shift","ms":400}
{"action":"clipboard_get"}
{"action":"clipboard_set","text":"hello"}
{"action":"zoom","region":"0,0,400,300"}
```

### Failure modes
`confirm_required` on clipboard set when no confirm hook is configured. `confirm_denied` when the hook fails. `invalid_arguments` on a one-point drag, a hold over 30s, an unknown keysym, an empty clipboard, or a zoom region past the window. `unsupported` for `--tier ax` on coordinate and keyboard actions, for F21–F24 on macOS, and for back/forward on the macOS activate tier. `permission_required` when arboard reports the macOS pasteboard permission error.

### Tests
- Keysym table: `super+c`, `Return`, `ctrl+alt+Delete`, `plus`.
- Drag rejects fewer than 2 points and records N moves on the stub.
- Clipboard stub does not touch the real pasteboard. The trait method is stubbed.
- `package-macos-app.sh` is syntax-checked on every OS (`bash -n`, and `shellcheck` when the runner has it). The macOS CI job also runs the script against the debug binary and checks the bundle layout, the ad-hoc signature, and the plists. The release workflow runs it on the arm64 release binary. See [Doctor and packaging corrections](#doctor-and-packaging-corrections).

### Doctor and packaging corrections
Part B fills in the doctor and the helper bundle. A few P1 and P8 details were wrong or incomplete:

- `--prompt` is the opt-in flag that may show dialogs. `--request` stays an alias so P1 JSON (`"request": true`) still prompts. The default calls `AXIsProcessTrusted`, `CGPreflightScreenCaptureAccess`, `CGPreflightPostEventAccess`, and `CGPreflightListenEventAccess`. It does not call the request variants and it does not open Settings.
- The response envelope's `ok` means the report was produced. The process exits 0 in that case, including when a permission is missing. `permissions_ok` is the aggregate of required grants, and `missing` lists the ids that are not `Some(true)`. Reusing `ok` for grants would make a missing permission a failed command, which contradicts the exit-code rule and the P1 tests that treat a denied Screen Recording bit as a successful report.
- `screen_capture` keeps the P1 name. `screen_recording` is the same boolean.
- `input_monitoring` is `CGPreflightListenEventAccess`. `input_monitor` stays the P6 tap flag.
- Post Event and Accessibility are the same System Settings pane. Apple maps both TCC services to Accessibility, so both fixes use `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`. Screen Recording uses `Privacy_ScreenCapture`. Input Monitoring uses `Privacy_ListenEvent`.
- Each missing permission has a `fix` object (`permission`, `url`, `target`, `instruction`) and the same instruction string in `fixes`. `doctor --open-settings` runs `open` on the first URL. The one-shot command also writes the `text` summary to stderr. `exec` and `serve` do not.
- `identity` carries the executable path, the bundle id (`null` for a bare binary), whether the process is inside `Computer Use.app`, `codesign -dv` status (`signed`, `unsigned`, `adhoc`, or `unknown`) and team id, and the TCC responsible app already reported as `responsible_app`. When the binary is not the bundle, the note tells the user to symlink `Computer Use.app/Contents/MacOS/computer-use` into `~/.local/bin`.
- Linux and Windows keep the null permission probes and the P13 / P12b note. P12a already reports `accessibility` for Windows (the `UIAutomation::new` probe) and still has `screen_capture: null`. Linux adds `session` (`x11`, `wayland`, or `x11+wayland`) and `atspi_bus` (address file or `AT_SPI_BUS` only; no bus connection) when P13 lands. Windows `uiaccess` and `integrity` come from a best-effort token probe. Those fields are not required grants. `--prompt` and `--open-settings` do not prompt on those platforms.
- The macOS CI job builds the ad-hoc app. P8 originally deferred that zip to P15. The layout check belongs with the script. P15 still bumps the version, rewrites the README, and publishes.
- The release workflow step lands here. Signing uses `MACOS_CERTIFICATE_P12`, `MACOS_CERTIFICATE_PASSWORD`, and `APPLE_TEAM_ID`. Notarization also needs `APPLE_ID` and `APPLE_APP_SPECIFIC_PASSWORD`. Without those secrets the artifact is `computer-use-macos-arm64-app-unsigned.zip` and the release notes say so. `docs/PACKAGING.md` has the local commands.

## P9 — Menu bar, Apple events, Shortcuts, Dock, Spaces
### Design
Menu bar (`src/backend/macos/menu.rs`): from the application element, copy `AXMenuBar`. Walk `AXMenuBarItem`, then `AXMenu`, then `AXMenuItem`. Match a path `File > Save` case-insensitively on `AXTitle`. Invoke with `AXPress` on the item. This uses tier `ax` and does not open the menu visually when `AXPress` succeeds. If the item is disabled (`AXEnabled` false), return `execution_failed`.

Apple events: `computer-use apple-event --app Notes --script '...'` runs `osascript -e` against that application with the P1 timeout helper. This is an explicit command. It is not the AX fallback. The script is denied when the target app is denied. JXA is `--language javascript`.

Shortcuts: `shortcuts run <name>` via `command::run_timeout` on the `shortcuts` binary. Timeout 60s. The name is an argument, not a shell string.

Dock: resolve `com.apple.dock`, walk its AX list of dock items, click by title with `AXPress`. Listing is `dock list`.

Spaces: public APIs do not move windows between Spaces reliably. This phase ships:

- `spaces` returns the count of `NSScreen` screens and the windows from P1 grouped by the screen that contains their center. That is the supported report.
- Moving a window to a Space requires private SkyLight (`CGSMoveWorkspaceWindowList` or the current successor). That call lives behind feature `skylight`, returns `unsupported` when the symbol is missing, and is documented in `docs/SKYLIGHT.md` as best-effort. Do not claim Mission Control control works.

### CLI
```
computer-use menu list --app Notes
computer-use menu click --app Notes --path "File > Save"
computer-use apple-event --app Notes --script 'get name'
computer-use shortcuts run "My Shortcut"
computer-use dock list
computer-use dock click --title "Notes"
computer-use spaces
```

### JSON
```json
{"action":"menu_click","app":"Notes","path":["File","Save"]}
{"action":"apple_event","app":"Notes","script":"get name","language":"applescript"}
{"action":"shortcuts_run","name":"My Shortcut"}
{"action":"dock_list"}
{"action":"spaces"}
```

`menu list` returns nested `{title, enabled, children}` with a depth cap of 4.

### Failure modes
`element_not_found` when the path misses. `policy_denied` when the app is denied. `unsupported` for Space moves without the feature. `execution_failed` when `shortcuts` is not on `PATH`.

### Tests
- Path matcher on a fixture menu: `File > Save` hits, `File > Missing` misses, disabled items fail.
- Apple event argument builder does not pass the script through a shell. Assert the argv vector.
- `spaces` JSON shape from a fake screen list.

## P10 — Chrome DevTools Protocol
Feature `cdp` only. Off by default.

### Design
`src/browser/cdp.rs` uses `chromiumoxide` 0.9.1 on a current-thread `tokio` runtime created for the call and dropped after it. `DesktopBackend` stays sync. The runtime is created on the CLI thread, never inside a tokio worker.

Attach only. **Never spawn a new Chrome and never call `Browser::new_page`.** Discover an existing port by reading the target pid's command line for `--remote-debugging-port=N` (`/proc` on Linux, `sysctl` KERN_PROCARGS2 on macOS, `Win32` command-line query on Windows). Otherwise require `--cdp-url ws://127.0.0.1:9222`. The command also requires `--cdp` so a binary built with the feature still does nothing until the flag is present.

When the caller does not pass `--target` and no `page` target exists in the attached browser, every operation returns `cdp_unavailable` with the stable phrasing `no page target found ... computer-use is attach-only and will not create new pages`. A new tab must be opened in Chrome by hand before retrying.

Operations: list targets, activate a target, `Page.navigate`, CSS `querySelector`, click by selector (see *Click path* below), `innerText` of a selector, type into a selector (or the page body), and `Runtime.evaluate` only when `--eval` is also set. `evaluate` is off by default because it is an arbitrary script channel.

**URL checks** go through `policy::url_blocked` twice:

1. The *requested* URL is checked before any websocket is opened. `browser navigate --url <blocked>` returns `blocked_url` and never opens the websocket.
2. The *current* page URL is read after attach and checked against the same list. This blocks `click`, `text`, `type`, `eval`, and `activate` against a tab that has already landed on a denied URL, so a follow-up op never sends data into a blocked page.

### Read vs type (`browser text` and `browser type`)

`browser text --selector S` reads the first element's `innerText` and returns it in the result; it does not type. The selector is required. A selector that matches nothing returns `element_not_found` (mapped from `chromiumoxide`'s `CdpError::Chrome` / `ChromeMessage` messages that contain `no node` or `node with given id ... not found`).

Typing lives in a separate `browser type --selector S --text T` / `Action::BrowserType` action (no selector means the page body). It is classified like `Action::Type` for audit purposes: the text body is never stored, only its character count appears in the audit row. `BrowserType` is `Mutate`; `BrowserText` is `Observe` (read-only).

### Click path

Spec asked for the element box centre to flow through the same path as a screen-space click (window bounds + `window.screenX/Y` + `outerHeight-innerHeight` + devicePixelRatio). We **kept the CDP `Input.dispatchMouseEvent` tier instead** because:

- Chrome owns its own window; a separate process would have to read the bounds via the Accessibility API or window list, then map CSS px -> device px, then map window-relative -> screen via display configuration. Each link is OS-specific and has no public CDP equivalent.
- The result of CDP click already lands in Chrome's own gesture/IME path; layering screen-space mouse simulation on top would race with Chrome's input pipeline.
- The CDP path is reliable enough for an automation attach, and the JSON result already names the tier.

The deviation is documented here and in `src/browser/cdp.rs::click`. A future revision can return a JSON `tier: "cdp"` field if a caller needs to know which path ran.

### CLI
```
computer-use browser tabs --cdp --app "Google Chrome"
computer-use browser navigate --cdp --url https://example.com
computer-use browser click --cdp --selector "button.save"
computer-use browser text --cdp --selector "h1"
computer-use browser type --cdp --selector "#name" --text "hello"
```

### JSON
```json
{"action":"browser_tabs","cdp":true,"app":"Google Chrome"}
{"action":"browser_navigate","cdp":true,"url":"https://example.com"}
{"action":"browser_click","cdp":true,"selector":"button.save"}
{"action":"browser_text","cdp":true,"selector":"h1"}
{"action":"browser_type","cdp":true,"text":"hello","selector":"#name"}
```

### Failure modes
`unsupported` when the feature is off. `cdp_unavailable` when no port and no `--cdp-url`, when the websocket cannot connect, or when no `page` target exists. `blocked_url` for `navigate` before the websocket opens *and* for `click` / `text` / `type` / `eval` / `activate` against a tab whose current URL matches `[browser].blocked_urls`. `element_not_found` when the selector matches nothing. `policy_denied` when the browser bundle is denied.

### Tests
- Adapter from a fixture WebSocket is not required. Test URL blocking, command-line port parsing, error mapping (`map_cdp_error`), the "no page target" message, and the "feature off" error with `#[cfg(not(feature = "cdp"))]`.
- With the feature, a unit test builds the request types. Do not launch Chrome in CI.
- A CLI integration test writes a temp `policy.toml`, runs `browser navigate --cdp --cdp-url ws://127.0.0.1:1 --url <blocked>` and asserts `blocked_url` *without* connecting (the connect would otherwise report `cdp_unavailable`).
- Another CLI integration test confirms `browser tabs --cdp --app "Google Chrome"` returns `cdp_unavailable` when no Chrome is running.
- `--all-features` clippy is the compile check on all three OS.

## P11 — Vision fallback and set-of-marks
### Design
Use vision when the tree is empty or the caller passes `--ocr`. It does not replace the tree.

macOS: `objc2-vision` 0.3.2 feature `VNRecognizeTextRequest`. Build a `VNRecognizeTextRequest`, recognition level accurate, language correction on, and run it on the window image from P4. Each observation yields text plus a bounding box in image pixels, converted through the screenshot map to screen points.

Windows: `Windows.Media.Ocr.OcrEngine` via the `windows` crate feature `Media_Ocr`, using the user-profile language. This code is `cfg(target_os = "windows")` and can land as a stub that returns `unsupported` until P12 adds the `windows` dependency. Prefer landing the real call in P12 if pulling `windows` forward slows P11. The macOS path must not wait for that.

Linux: no OCR engine in 0.2.0. `ocr` returns `unsupported` with `note` "no OCR backend on Linux in 0.2.0".

Set-of-marks is pure `image` code in `src/vision/marks.rs`. Draw the tree's element frames, or the OCR boxes, as numbered rectangles on a copy of the screenshot. Numbers match the tree indices when the source is the tree, or `o0`, `o1`, … when the source is OCR. `screenshot --marks` writes that image and returns the same map.

### CLI
```
computer-use ocr --app Notes
computer-use screenshot --app Notes --marks
```

### JSON
```json
{"action":"ocr","app":"Notes"}
```

```json
{"ok":true,"blocks":[{"text":"Save","x":10,"y":20,"width":40,"height":16,"screen":{"x":130,"y":60}}]}
```

### Failure modes
`permission_required` when the screenshot cannot be taken. `unsupported` on Linux and on Windows until P12b adds the `windows-capture` path. Empty `blocks` is success.

### Tests
- Marks renderer draws on a 20×20 fixture and leaves a non-white pixel inside box 0.
- OCR box conversion uses the screenshot map.
- A redaction pass drops blocks whose source rectangle sits inside a secure element's frame.

## P12 — Windows UI Automation
### Design
`src/backend/windows/uia.rs` uses `uiautomation` 0.25.1. `src/backend/windows/capture.rs` uses `windows-capture` 2.0.1. `src/backend/windows/apps.rs` replaces the PowerShell foreground script with `EnumWindows` and `GetWindowText` / `GetWindowThreadProcessId` through the `windows` crate. Keep the PowerShell path out. The current script in `windows.rs` goes away once `apps.rs` is in.

Targeting rules, written because Codex has hit the wrong window:

- Never call `ElementFromHandle` or a cache builder on `GetForegroundWindow()` when the caller passed `--app` or `--window`.
- Resolve the HWND from the pid and the title or z-order first. Build the UIA tree from that HWND only.
- **Real** 2 s timeout on every UIA call: run the call on a dedicated worker thread (which calls `UIAutomation::new()` and `CoInitializeEx(COINIT_MULTITHREADED)` itself) and wait with `recv_timeout`. On timeout return `execution_failed("UI Automation call timed out after Ns")` and detach the worker — the CLI never blocks on a stuck target. The closure returns only `Send`-able values (`UiNode` / `ElementOutcome` / `()`) so `UIElement` / `UIAutomation` never cross thread boundaries. The wrapper is unit-tested with a sleeping closure.
- There is no background walk of unrelated HWNDs.

Tree: walk control view, not raw view. Render with the same `tree.rs` format. Role names are UIA control types (`Button`, `Edit`, `Window`). Indices and diffs use the same cache. Patterns:

| Need | Pattern |
| --- | --- |
| Press | `InvokePattern::invoke` |
| Set value | `ValuePattern::set_value`. Refuse `IsPassword` controls. |
| Select text | `TextPattern` document range, then `TextPatternRange::select`. |
| Scroll into view | `ScrollItemPattern::scroll_into_view` |
| Expand | `ExpandCollapsePattern` |
| Secondary | Invoke when that is the only pattern. Do not invent a click. |

Per-window capture: `windows-capture` 2.0.1 from the HWND (`Windows.Graphics.Capture`). One frame. Map pixels to screen points with the window rect and the DPI scale from `GetDpiForWindow`. Fallback: crop the xcap image, with `capture_source: "xcap"`. *(P12b.)*

**P12b correction to the capture map.** The design sentence above reads as "logical size = frame pixels / DPI scale". That is wrong, and shipping it would break every click. Windows has exactly one screen-point space in this crate: `apps.rs` reports `GetWindowRect` bounds, `uia.rs` reports UIA bounding rectangles, and the enigo path reports `SetCursorPos` coordinates. Only the capture buffer is in device pixels, and it differs from that space whenever the process is DPI-unaware (Windows virtualizes coordinates) or on a mixed-DPI desktop. So `logical_*` is the window **rect**, and `GetDpiForWindow` is recorded as the capture backing `scale`; the screenshot map already absorbs the leftover ratio because `image_to_screen` divides by `content_*`. Process DPI awareness is deliberately not switched in `capture.rs`: `SetProcessDpiAwarenessContext` only succeeds before the first window exists, and flipping it mid-process would desynchronize the bounds `apps.rs` already read for the same capture. A frame whose pixel size disagrees with `rect * scale` is not an error — the map absorbs it — but it returns a `captured frame is WxH px but the window rect implies AxB px at scale S` warning, because that mismatch means the window resized between resolving it and the frame arriving.

**P12b capture timeout.** `Windows.Graphics.Capture` only pushes a frame while the window repaints, so a window that never repaints would hang the CLI. The capture waits 5 s for the first frame, then gives up and takes the xcap crop. That is the only place a Windows capture is time-bounded.

Background: UIA patterns do not need foreground. That is the Windows equivalent of tier `ax`. If a pattern is missing, tier `activate` uses `SetForegroundWindow` and enigo, under the same `foreground.lock`. Codex stays foreground-only with an allowlist. This phase is past that, and the policy allowlist from P6 still applies. Document that some Win32 apps ignore Invoke unless they are foreground. The warning `foreground required` is returned when Invoke fails once and the retry with `activate` is what worked.

**P12b scope of the `activate` retry.** *Which* ops may fall back is a deliberate narrowing of "if a pattern is missing". Only `press` and `type` are reproducible through enigo without inventing an action the caller did not ask for:

| Op | Falls back? | Why |
| --- | --- | --- |
| `press` | Yes | `SetForegroundWindow`, enigo click at the gated element centre. Needs a frame; without one there is nothing to aim at. |
| `type --element` | Yes | `SetForegroundWindow`, UIA `set_focus` on the element (a background-safe UIA call), then enigo typing. This is the P7 documented path for a non-settable element. |
| `set_value` | No | The P3 parity contract is *value, not keystrokes*. Rewriting it as keystrokes would also break `IsPassword`, which is refused before any pattern runs. |
| `select_text` | No | Needs the `TextPattern` document range. There is no keystroke sequence that is reliably the same action. |
| `scroll_to` | No | Needs `ScrollItemPattern`. An enigo scroll is a guess at an amount, not the caller's action. |
| `focus` | No | Already background-safe through UIA; there is nothing to retry. |
| `perform-action` | No | Already `execution_failed` on Windows. |

Refusal order in `activate::plan` (pure, unit-tested): `--require-background` first, then a busy `foreground.lock`, then the op table, then the missing-frame check for `press`. `--allow-foreground` skips the lock and still falls back.

**P12b `focus_restored`.** `element::outcome` reports `focus_restored: Some(true)` for any `activate` via, which is true on macOS because that tier puts the previous app back. Windows is different, so the fallback captures `GetForegroundWindow` first, restores it after, verifies the restore, and reports what actually happened through the new `element::activate_outcome`. Nothing claims a restore that did not happen.

`doctor` on Windows reports whether `UIAutomation::new` succeeded. `uiaccess` and `integrity` come from the `windows` crate (no PowerShell): `OpenProcessToken(GetCurrentProcess, TOKEN_QUERY, …) -> GetTokenInformation(TokenUIAccess) -> u32 0/1`. The integrity RID is the last sub-authority of the `TOKEN_MANDATORY_LABEL.Sid`, mapped to `low / medium / high / system / protected / untrusted`. Failures stay `null` with a `note`. The `whoami` / PowerShell token scripts are gone.

### CLI
The same commands as macOS. `list-apps`, `list-windows`, `tree`, `press`, `set-value`, `screenshot --window` work on Windows after this phase. `menu`, `dock`, `spaces`, and `apple-event` stay `unsupported`.

### JSON
Same action names. `source` is `"uia"`. `doctor.accessibility` is true when UIA initializes.

### Failure modes
`app_not_found`, `window_not_found`, `permission_required` is rare on Windows. A UIAccess or integrity mismatch is `execution_failed` with the HRESULT. `policy_denied` uses the exe path.

P12b adds: `execution_failed("Windows.Graphics.Capture produced no frame within 5s")` before the xcap fallback; `execution_failed("SetForegroundWindow did not bring hwnd N to the front; Windows refused the foreground change")` — `SetForegroundWindow` is verified with `GetForegroundWindow`, not trusted from its return value, because Windows applies a foreground-lock timeout; `execution_failed("Windows.Media.Ocr has no engine for the user-profile languages")` when no recognizer language pack is installed; `execution_failed("Windows.Media.Ocr did not finish within 10s")` on the OCR worker budget. `ocr` never returns `unsupported` on Windows any more — that was the P11 stub.

### Tests
- A fake tree of control types renders with indices and redacts `IsPassword`.
- Diff reuse: the macOS fixtures and a Windows fixture share `tree.rs` tests.
- Dispatcher test: a requested pid is the one passed to the stub. There is no foreground fallback in the stub when `app` is set.
- `resolve_target_rejects_unmatched_pid` asserts `AppNotFound` (per ROADMAP, an unmatched pid is `app_not_found`).
- `run_uia_with_timeout_*` exercises the worker wrapper: a sleeping closure yields `execution_failed("UI Automation call timed out after Ns")`; a fast closure returns its value; a closure error is propagated unchanged.
- `integrity_rid_mapping_covers_required_buckets` covers every required integrity-level label.
- `doctor_uses_the_shared_shape` is gated on Windows; it stays green even when `UIAutomation::new` returns `permission_required` (no display / no integrity elevation).
- Off-macOS CLI tests skip Windows where the underlying command is now real: `tree_command_is_unsupported_off_macos_and_windows`, `p9_commands_are_unsupported_off_macos`, `list_apps_returns_json`. P12b renames `element_commands_are_unsupported_off_macos` to `element_commands_are_unsupported_off_macos_and_windows`: P12a wrote the UIA dispatcher but never wired `element_action` into `impl_platform_backend!`, so every Windows element command still answered `unsupported`. P12b wires it, which is what makes the `activate` retry reachable.
- P12b pure-math tests run on **every** runner, not only the Windows job, so CI-only code is not the only place the arithmetic is checked: `image_util::tests` covers `window_placement`, `backing_scale`, `size_mismatch_warning`, the 150 %-scale round trip through `image_to_screen` / `screen_to_image`, and `screen_rect_to_image` (origin, downscale, letterbox pad, off-canvas, never-empty). `vision::ocr::tests` covers `ocr_box_to_image` (top-left origin preserved, rounding, clamping, never-empty, and the round trip back through `ScreenshotMap::to_screen`). `backend::windows::activate::tests` covers the fallback decision matrix and the warning.
- `windows_media_ocr_runs_and_never_returns_unsupported` runs the real engine on the Windows runner against a blank image: zero blocks is success, a missing language pack is an `execution_failed` that names the engine, and `Unsupported` is now a test failure.
- CI compile on `windows-latest` with `--all-features`. Live UIA, live capture, and live OCR against a real window are **not** run on the runner. CI is the only Windows verification.

## P13 — Linux AT-SPI, portals, and X11
### Design
`DesktopBackend` stays sync. Linux methods `block_on` a process-wide current-thread runtime created on first use from the CLI thread.

AT-SPI (`atspi` 0.30.0), always compiled on Linux:

- `AccessibilityConnection::new`.
- Find the application on the desktop by pid.
- Walk children. Render with `tree.rs`. Roles are AT-SPI roles (`Role::name()`).
- Element actions on P13a: `Action::do_action` for `press` and `perform`; `Component::grab_focus` for `focus`; `EditableText::set_text_contents` (or `Value::set_current_value` for numeric) for `set_value` / `type`; `Text::set_selection` / `add_selection` for `select_text`; `Component::scroll_to` for `scroll_to`. Each per-call request is bounded by `tokio::time::timeout` (3 s); a missing interface returns `execution_failed` instead of inventing a click. The same `crate::element::gate` stale-element revalidation macOS / Windows use runs first.
- Frames: `Component::get_extents(CoordType::Screen)` per node when the `Component` interface is present — kept as `None` on P13a because the round-trip-per-node cost does not pay back without part B's per-window background. Part B keeps the same call shape and turns the frame on.
- Secure text: **only `Role::PasswordText` redacts the value.** The earlier `State::Sensitive` (without `State::Enabled`) rule was removed: AT-SPI `Sensitive` means "can receive focus / is user-actionable", not "secret", and mirroring `AXSensitive` to a meaningless flag dropped values of merely disabled controls. `tree::node_is_secure` still answers the prompt's redaction question, the renderer keeps `••••` for any redacted node, and the unit tests pin `Role::PasswordText -> secure=true` plus every non-password role -> `secure=false`.

Screen capture, feature `portal`:

- `ashpd` 0.13.13 `ScreenCast`. Start a session, prefer a window source when the portal offers one, else the monitor that contains the window, and crop.
- Open the PipeWire remote with `pipewire` 0.9 and pull one video frame into RGBA.
- The portal will show a system dialog the first time. Document that the agent cannot click that dialog for itself. `permission_required` is the error when the user cancels.

Source selection is decided in `portal::plan_source`, a pure function over a `StreamInfo` list, so the preference order is unit-tested without a portal daemon. A window stream wins outright; failing that the monitor whose logical rect contains the window centre is chosen and cropped, with the crop clamped to the monitor so a partly off-screen window crops to the visible part. A monitor with no reported geometry is kept only as a last resort — without an extent there is no correct crop, and silently returning the wrong pixels is worse than a `window_not_found`. Frames arrive in device pixels while portal geometry is logical, so `crop_to_stream` runs in logical points and `crop_pixels` applies `frame_scale(logical_width, pixel_width)` and clamps to the buffer.

Capture fallbacks follow the P4 rule: a **cancelled dialog is `permission_required` and never falls back**, because a fallback would report a capture the user deliberately withheld. Only technical failures — no portal daemon (`unsupported`), no frame within 5 s, an unsupported pixel format — fall back to the xcap desktop crop, which sets `occluded: true` and carries a warning naming the cause.

PipeWire is driven directly: the portal's `OpenPipeWireRemote` fd is handed to `ContextRc::connect_fd_rc`, so no PipeWire socket path is involved and nothing outside the session is reachable. The stream is linked to the portal's node with the `target.object` property rather than the `TARGET_OBJECT` constant, which would pull the `v0_3_44` PipeWire feature. `param_changed` records the negotiated `VideoInfoRaw` and `process` copies the first buffer, swizzling the BGRA/BGRx/xBGR/ABGR formats a screen cast actually uses; anything else falls back rather than returning wrongly-ordered pixels. A 5 s timer quits the main loop so a locked session cannot hang the CLI.

Input:

- Tier `ax` is the AT-SPI action.
- Tier `event` on Wayland is `ashpd` `RemoteDesktop` (pointer and keyboard) when the session is granted. If the portal returns an EIS socket, a `reis` 0.7 client may send relative motion. `reis` is incomplete: a failed handshake falls through and does not panic.

Routing is decided by `event_route`, a pure three-way `EventRoute` (`X11Test` / `Portal` / `Unsupported`) keyed on the session and the feature. `portal::perform_event_tier` selects pointer + keyboard, attaches a monitor ScreenCast source to the same portal session for the coordinate space `NotifyPointerMotionAbsolute` needs, and refuses `--require-background` because the portal moves the real pointer. Pointer buttons use **evdev** codes (`BTN_LEFT` = 0x110), not X11 button numbers, and keys use X11 keysyms rather than XTest keycodes; an unmapped key name is `unsupported`, never a silently dropped keystroke. `drag`, button hold, hold-key and free-text typing are not sent through this tier and return `unsupported` rather than a partial event.

The EIS probe asks the already-granted session for a `ConnectToEIS` socket and runs the `reis` handshake. **Every** failure — no socket, a refused versioned call, a failed handshake — returns "not offered", the action still completes on the portal's absolute motion, and a warning says so. `reis` 0.7 exposes no public API to create an `ei_device`, so relative motion is never dispatched; the handshake is used to report availability honestly rather than to pretend an EIS path exists.
- Tier `event` on X11 (`DISPLAY` set and `WAYLAND_DISPLAY` unset, or `--x11`) uses `x11rb` 0.14 feature `xtest`: `xtest::fake_input` for motion, button, and key. This does not include a background-per-window path. Say so in the response with `tier: "x11"`.
- Tier `activate` remains enigo, which already uses libxdo on X11. On Wayland, enigo is not a supported tier. Return `unsupported` rather than sending the event nowhere.

`doctor` reports whether the AT-SPI bus connected, whether `WAYLAND_DISPLAY` or `DISPLAY` is set, and whether the `portal` feature is compiled.

X11 fallback capture stays xcap, which already uses XCB.

### CLI
Same commands. `computer-use doctor` on Linux fills `accessibility` from the bus probe. `--x11` forces the XTest path.

### JSON
`source` is `"atspi"`. Capture `capture_source` is `"portal"` or `"xcap"`.

### Failure modes
`permission_required` when the portal dialog is denied. `unsupported` for Wayland coordinate input without `portal` or without a granted RemoteDesktop session. `execution_failed` when the AT-SPI bus is absent (install `at-spi2-core`). `policy_denied` when the action targets a `Role::PasswordText` node for `set_value`, `select_text`, or `type --element`; the value never reaches the bus, the cache, or the renderer. `cdp` is independent of this phase.

### Tests
- Role and password redaction on fixture AT-SPI nodes through `tree.rs`. P13a pins the rule to `Role::PasswordText` only — `state_is_sensitive_only` is kept as a dead helper to mark the old rule obsolete.
- Element-action dispatch is unit-tested for: (i) `ElementAction::routes_to_atspi` (the Linux dispatcher no longer answers `unsupported`); (ii) `proxy_at` signature stability; (iii) redaction-by-role lock. The live paths run through AT-SPI on Linux CI.
- XTest request encoding is tested by inspecting the `x11rb` request bytes built for a motion, without a display. P13a adds `perform_event_tier_wires_xtest_fake_input` and `perform_activate_tier_is_enigo_on_linux` as dispatcher-level tests; the former proves `tier = event` routes through `xtest::fake_input` (the connection step fails `DisplayUnavailable`/`ExecutionFailed`, never a generic `unsupported`), the latter pins `TierName::Activate -> perform_activate_tier`.
- Portal is not contacted in CI. `cargo check --features portal --all-targets` on Ubuntu with `libpipewire-0.3-dev` (plus `libclang-dev`/`clang`, because `pipewire-sys` and `libspa-sys` run bindgen) is the compile check. The three crates are Linux-gated optional dependencies, so `--all-features` on macOS and Windows still compiles without them.
- Default `cargo test` on Ubuntu still passes without PipeWire. The portal unit tests live in `backend::linux::portal` and need no daemon and no PipeWire: crop and remap math (`crop_to_stream`, `frame_scale`, `crop_pixels` including the clamping and degenerate cases), source selection (`plan_source` preferring a window stream, choosing and cropping the containing monitor, refusing a window off every monitor, and the unknown-geometry last resort), error mapping (`map_failure`, plus `is_terminal` proving a cancelled dialog is never downgraded to a fallback), and `crop_image` byte-exact extraction.
- Under `--all-features` the same module also pins `evdev_button` codes, the `keysym_for` table, and `doctor`'s `portal` field against `portal::compiled()`.

## P14 — Isolation
Honest scope. This phase ships the parts that are real and writes down the parts that are not.

### What ships
`--display <id>` restricts capture and coordinate conversion to one monitor from `list_displays`. Points are still virtual-desktop points. The screenshot map's origin is that monitor's origin. A click whose screen point lies outside the monitor is `invalid_arguments`.

Linux virtual display, the one that is actually a separate seat:

- `computer-use --display virtual` on Linux prints a recipe and can exec a prepared environment. The supported recipe is Xvfb: `Xvfb :99 -screen 0 1440x900x24` and then this binary with `DISPLAY=:99`. Document the exact command in `docs/ISOLATION.md`.
- When `DISPLAY` is already a separate server, the existing X11 backend is the isolation. No new protocol.
- A nested Wayland compositor (cage or labwc) on a private `WAYLAND_DISPLAY` is documented as a manual setup. The portal dialog still appears inside that session. Do not automate the host compositor.

### What is designed and marked experimental
macOS `CGVirtualDisplay` (private, macOS 14+) behind feature `virtual-display`:

- Resolve `CGVirtualDisplayDescriptorCreate` and `CGVirtualDisplayCreate` with `dlsym` from CoreGraphics. Null symbols return `unsupported`.
- The display is system-visible. The user sees it. It is not a per-process sandbox. WindowServer still composites it.
- Document that TCC, the menu bar, and input focus are shared with the host session. Do not describe this as isolation.

### What is infeasible in 0.2.0
- A macOS VM per agent inside this crate. Feasible as an operator setup: a UTM or `Virtualization.framework` guest that runs the same binary, with the host piping JSON lines over SSH or a virtio socket. The crate does not grow a hypervisor dependency. `docs/ISOLATION.md` describes that proxy in one page: the host process runs `computer-use serve` nowhere, it forwards stdin. Out of scope to implement.
- Sharing one host app into a guest. Infeasible.
- Hiding a virtual display from the macOS user while still capturing it with ScreenCaptureKit. Infeasible with public APIs.
- Windows virtual display. Infeasible here. The documented equivalent is a Windows VM or Windows Sandbox running the same binary. UI Automation against the host from a sandbox is a different trust boundary and is not implemented.
- Nested Wayland input without a portal grant. Infeasible by design of the portal.

### CLI
```
computer-use displays
computer-use screenshot --display 1
computer-use isolation
```

`isolation` prints the JSON matrix: `{platform, display_filter: true, xvfb: "linux only", cg_virtual_display: "experimental|unsupported", vm: "documented, not shipped"}`.

### Tests
- `--display` rejects a point outside the chosen monitor using a fake display list.
- `isolation` JSON contains the infeasible keys so the documentation and the command cannot drift. A snapshot test of that object is enough.

## P15 — Docs and 0.2.0 release
### Design
- Bump `version` to `0.2.0`. Update `tests/cli.rs` to expect `0.2.0`.
- Set `rust-version = "1.91"`.
- Rewrite the README usage section from this document: new commands, the default max-width, the policy path, the feature flags, and the permission section. Keep the 0.1.0 examples that still work.
- CHANGELOG `0.2.0` lists every phase in the keep-a-changelog style already used.
- Add `docs/` and `packaging/` and `scripts/package-macos-app.sh` to the crate `include` list only if they should ship in the crate tarball. The roadmap should ship. The `.app` script should ship.
- Release workflow: P8 part B already runs `scripts/package-macos-app.sh` on the macOS release job, uploads the zip, and signs or notarizes when the secrets in `docs/PACKAGING.md` exist. P15 checks that this still matches the 0.2.0 tag. Linux release builds with `--features portal`. Windows release builds with the default target dependencies (UIA and `windows-capture` are target deps, not features).
- Confirm `cargo publish --dry-run` includes the README and this roadmap.
- Update the parity table statuses in this file to match what merged. A row that did not land stays `TODO` and is listed under "Cut from 0.2.0" in the changelog.

### Tests
The full CI matrix. P8 part B already runs `bash -n scripts/package-macos-app.sh` and a macOS package check that the bundle id is `ai.lindoelio.computer-use`. P15 repeats that check on the release artifact after the version bump.

---

## Parity checklist
`Codex` describes OpenAI Codex Computer Use as publicly documented and as described by the reverse-engineering notes this plan is based on. `Ours (target)` is the 0.2.0 goal. `Status` is this repository today.

`Verification` is how that row was checked on the development Mac during P15:

- **live** — exercised on a real window on this Mac (with Screen Recording granted and the screen unlocked; Accessibility was off for this process, so anything that needs AX was either denied with a structured error or skipped).
- **CI** — wired and unit/integration-tested in CI on Ubuntu/macOS/Windows; never run end-to-end against a real app window on this Mac.

The same word covers the three OS rows: a row that is `CI` is CI on every platform that the row mentions.

- **compile-only** — code compiled in CI but the code path was never invoked by a test or by a CLI invocation; the surface is real, the runtime was not exercised.

The development Mac has Accessibility denied for `com.anysphere.sand` (the responsible app), Screen Recording granted, Input Monitoring denied, and Post Events denied. Almost nothing on macOS AX / event / activate / SkyLight / synthetic was exercised against a real app window. The live paths we did exercise on this Mac are `doctor`, `list-apps`, `list-windows`, `policy check`, `displays`, full-desktop `screenshot`, the macOS Vision unit test (`cargo test --release vision_recognizes_drawn_text -- --ignored --nocapture`), and `scripts/package-macos-app.sh` ad-hoc packaging. Everything else on macOS is `CI`. Windows and Linux are `CI` for the whole 0.2.0 surface.

| Codex CU feature | Codex | Ours (target) | Status | Verification | Notes |
| --- | --- | --- | --- | --- | --- |
| Key-window AX tree as compact numbered text on each observation | Yes | Yes, `tree` and `get-state` | DONE | CI | P2 on macOS: numbered text from the native AX tree. Live `tree --app Finder` returned `permission_required` for `com.anysphere.sand`; the path is wired, AX was off for this process. P12a on Windows: numbered text from the `uiautomation` control view walker, same `tree.rs` rendering, `source=uia`. P13a on Linux: numbered text from AT-SPI through `AccessibilityConnection`, same `tree.rs` rendering, `source=atspi`; `Role::PasswordText` is the only redaction signal (the older `State::Sensitive && !State::Enabled` rule was removed because AT-SPI `Sensitive` means 'user-actionable', not 'secret'). |
| Window screenshot with the tree | Yes | Yes, target window only | DONE | CI (macOS: live desktop screenshot only) | P4 on macOS captures the target window with ScreenCaptureKit (`capture_source: sck`). Occluded windows use a desktop-independent filter. Other ScreenCaptureKit failures crop the xcap desktop image and set `capture_source: xcap` and `occluded: true`. Attached sheets are composited when Accessibility can see them. Windows `screenshot --app` returns `unsupported`. Linux P13b implements the capture path, but Linux `list_windows` is still `unsupported`, so `screenshot --app` returns `unsupported` there too and cannot yet reach the portal. P5 `include_screenshot` attaches that capture after a successful mutating action. A capture failure keeps the action result, sets `state.screenshot` to null, and adds a warning. Live on this Mac: desktop `screenshot` worked (Screen Recording was granted); `screenshot --app Finder` returned `window_not_found` because Finder had no on-screen non-desktop window at that moment. |
| `element_index` targeting | Yes | `--element` / `element` | DONE | CI | P3 on macOS: `press`, `focus`, `set-value`, `select-text`, `perform-action`, `scroll-to`, and `click --element` use the index. P12a on Windows dispatches the same operations through UIA control patterns (`Invoke`, `Value`, `Text`, `ScrollItem`, `ExpandCollapse`); P12b wires that dispatcher into `impl_platform_backend!` — until P12b the function existed but the dispatcher never called it, so Windows element commands answered `unsupported`. P12b adds the tier `activate` retry (`SetForegroundWindow` + enigo under `foreground.lock`) for `press` and `type` when a pattern is missing or fails, with the `foreground required` warning. Linux implements the same operations through AT-SPI as of P13a; P13b does not change element targeting. No element action was exercised against a real app window on this Mac. |
| `x`/`y` fallback when no index | Yes | Hit-test, then event tier | DONE | CI | P3 hit-tests on macOS (`AXUIElementCopyElementAtPosition`) and reports `hit.role` and `hit.title`. A hit-test error is a warning and the click still runs. P7 posts that click on tier `event` (`CGEventPostToPid`) and does not move the cursor. `execution_failed` from `event` falls back to `activate` unless `foreground.lock` is held or `--require-background` is set. Explicit `--tier synthetic` on Linux and Windows is `unsupported`. On Windows explicit `--tier event` is `unsupported`. On Linux P13b makes tier `event` real: X11 through XTest, Wayland through the RemoteDesktop portal when `portal` is compiled in, otherwise `unsupported`; auto picks `event` then `activate` there because `input_support` reports `event: true`. `--tier activate` stays enigo on both and does move the cursor. Live event posting was not run on this Mac. |
| Ids bound to pid + window + revision | Yes | `BoundElement` in the session cache | DONE | CI | P2 stores `BoundElement` and the structural revision. P3 acts on the index after revalidation. Unit-tested only. |
| Re-fetch and validate before acting | Yes | Yes, mismatch is `stale_element` | DONE | CI | P3 walks the cached child-index path and returns `stale_element` when the role, identifier, or frame (beyond 2 points) changed, or when the cached pid, window, or revision header does not match. The fresh tree is in the body. Nothing is performed. Element actions update `observed`. The `tree --diff` baseline stays put unless `return_state` is set. Unit-tested only. |
| Scroll into view before the action | Yes | `AXScrollToVisible` or platform equivalent | PARTIAL | CI | P3 on macOS calls `AXScrollToVisible` when the element frame is outside the window and the action is supported. Failure is a warning unless `--require-visible`. P12a on Windows uses UIA `ScrollItem` (`scroll_into_view`). P13a on Linux uses `Component::scroll_to(ScrollType::TopLeft)`; missing `Component` interface returns `execution_failed`. **Partial because the implementation depends on the platform interface being advertised** — a window that does not expose `ScrollItemPattern` (Windows) or `Component` (Linux) is `execution_failed`, not a no-op. |
| `AXManualAccessibility` and `AXEnhancedUserInterface` for Electron | Yes | Set on attach, restore on teardown | DONE | CI | P2 sets both on first attach and restores them from `session end` and process exit. A crash, or a caller that never calls `shutdown_sessions`, can leave the flag set until the target app quits. A later `session end` restores from the cache and does not write an attribute this process did not change. |
| Content hosted in another process | Yes | Walk child pids, cap 3 helpers | DONE | CI | P2. Foreign child pids are walked once, then a parent-pid scan. Cap is 3. |
| Set text through the value, not keystrokes | Yes | `set_value` via `AXValue` | DONE | CI | P3 sets `AXValue` for `set_value` and for `type --element` when the attribute is settable (`tier: ax`). A non-settable element is focused and posted to that pid (`tier: event`, `via: event`) with window id 0. `type` without `--element` uses the P7 plan: `event`, then `activate` on `execution_failed` when the lock is free. P12a on Windows dispatches the UIA `Value` pattern and refuses `IsPassword`; P12b keeps `set_value` off the `activate` retry precisely because this row requires the value path, not keystrokes. Linux auto is `activate`. |
| `set_value` | Yes | `set_value` | DONE | CI | P3 on macOS via `AXValue`. Secure fields are `policy_denied` and the value is never read. P12a on Windows uses the UIA `Value` pattern and refuses `IsPassword` (the property is checked before `set_value` runs and the value never reaches the renderer or the cache); P12b does not add an `activate` retry for it, so a missing `Value` pattern stays `execution_failed`. P13a on Linux: `EditableText::set_text_contents` wins over `Value::set_current_value` when both are advertised; `Role::PasswordText` is refused before the bus call. |
| `select_text` via selected range | Yes | `AXSelectedTextRange` / UIA Text / AT-SPI Text | PARTIAL | CI | P3 on macOS writes `AXSelectedTextRange` as an `AXValue` of type `kAXValueCFRangeType`. `start` must be less than or equal to `end`, and `end` must sit inside `AXNumberOfCharacters` when that attribute exists. Secure fields are refused. P12a on Windows uses the UIA `Text` pattern (`document_range` + `move_endpoint_by_unit` + `select`); missing pattern is `execution_failed`, and P12b deliberately adds no `activate` retry because no keystroke sequence is reliably the same action. P13a on Linux uses `Text::set_selection(0, start, end)` and falls back to `Text::add_selection(start, end)` when the first call returns false; missing `Text` interface returns `execution_failed`. **Partial because the implementation depends on the platform interface being advertised** — a window that does not expose `TextPattern` (Windows) or `Text` (Linux) is `execution_failed`. |
| Secondary actions (`AXShowMenu`, increment, confirm) | Yes | `perform_action` allowlist | DONE | CI | P3 macOS allowlist: `AXPress`, `AXShowMenu`, `AXIncrement`, `AXDecrement`, `AXConfirm`, `AXCancel`, `AXRaise`, `AXScrollToVisible`, and the four page-scroll actions. P12a on Windows: `ExpandCollapse` covers `expand` and `collapse`; `Press` maps to UIA `Invoke`. Other `perform_action` names are `execution_failed` and list `CopyActionNames`; P12b keeps that refusal and adds no retry. P13a on Linux matches `Action::get_actions()` by name (case-insensitive), defaulting to the first action (`click` / `press`) when the lookup misses. |
| Tree diff between turns | Yes | `--diff` and cache baseline | DONE | CI | P2. `--diff` is the default once a baseline exists. The signature is role, identifier-or-title, and child count, with an LCS inside each parent. |
| Force-full flag | Yes | `--full` | DONE | CI | P2. `--full` prints the whole tree and replaces the baseline. |
| Truncation of long child lists | Yes | 40 children, depth 12, 400 lines | DONE | CI | P2 caps live in `tree.rs`: 40 children (`… +N`), depth 12, 400 lines, then `truncated=true`. Secure values and URL queries are not stored. |
| URL shortening | Yes | Scheme, host, path. Query stripped. | DONE | CI | P2. `http` and `https` values keep scheme, host, and at most 60 characters of the path. The query is dropped. |
| Capture only the target window | Yes | SCK desktop-independent window, HWND, portal | DONE | CI | P4 on macOS uses `SCContentFilter` `initWithDesktopIndependentWindow`. A CG window id missing from both shareable-content queries is `window_not_found` and is not cropped. A locked or asleep session, or ScreenCaptureKit returning no displays or windows, is `display_unavailable` before `window_not_found`. A window-targeted screenshot asks that before it resolves the window. Screen Recording off is `permission_required`. P12b on Windows captures through `windows-capture` 2.0.1: `Settings::new(Window::from_raw_hwnd(...), WithoutCursor, WithoutBorder, Exclude, MinimumUpdateIntervalSettings::Default, DirtyRegionSettings::Default, ColorFormat::Rgba8, sink)`, `start_free_threaded`, the first `FrameBuffer` is copied out, then the session is stopped and joined. The shared slot rides in `Settings::flags` because the callback is constructed on the capture thread. The pixel-to-screen map is `logical_* = window rect` with `scale = GetDpiForWindow(hwnd) / 96`; the map absorbs the device-pixel ratio. Capture waits 5 s for a first frame — the API only pushes one when the window repaints — then falls back to the xcap crop with `capture_source: "xcap"`, `occluded: true`, and a warning naming the graphics-capture error. A frame whose size disagrees with `rect * scale` is a warning, not an error. Process DPI awareness is not switched mid-process; see the P12b capture-map correction in the design.  P13b on Linux: with the `portal` feature the window frame comes from the XDG ScreenCast portal (prefer a window source, else the containing monitor cropped to the window rect) and reports `capture_source: portal`; a cancelled dialog is `permission_required` and does **not** fall back, and any other portal failure falls back to the xcap crop with `occluded: true` and a warning naming the cause. Frames arrive in device pixels while portal geometry is logical, so the crop runs in logical points and the scale comes from `frame_scale`. **Not reachable yet:** Linux `list_windows` is `unsupported`, so `screenshot --app` cannot resolve a target window and never calls `capture_window`; `capture_source: portal` therefore does not appear from any CLI command today. |
| Every action can return the new state | Yes | `--state` plus optional screenshot | DONE | CI | P5. Successful mutating actions return `state.tree` as numbered full text, `state.diff`, and the new revision. That diff becomes the `tree --diff` baseline. Read-only actions ignore `--state` and `--screenshot`. A tree or screenshot failure keeps the action result and adds `warnings`. |
| Events posted to the target pid | Yes | `CGEventPostToPid` | DONE | CI | P7. Private event source, user data `policy::SYNTHETIC_EVENT_USER_DATA` (field 42). `CGEventPostToPid` returns void, so a post is not itself an error. `permission_required` when posting is not allowed. No target window is `execution_failed`. Live posting was not run on this Mac (Accessibility / Post Events denied). Windows explicit `event` is `unsupported`. P13b on Linux routes tier `event` by session: X11 keeps XTest (`tier: "x11"`), Wayland uses the RemoteDesktop portal when `portal` is compiled in, and Wayland without the feature is `unsupported`. The portal path covers pointer motion, buttons (evdev codes), scroll and key combinations (X11 keysyms); `--require-background` refuses it because it moves the real pointer. |
| Window number and window-local location on those events | Yes | Field 91 and local point | DONE | CI | P7. The window-local point is the screen point minus the window origin. Fields 91 and 92 carry the `CGWindowID`. Field 40 is `kCGEventTargetUnixProcessID`. A non-settable `type --element` posts with window id 0. Untested live. |
| Synthetic focus enforcer (SkyLight / CPS) | Yes, private | Feature `skylight`, fail closed | **PARTIAL** | **compile-only** | P7. Off unless the binary has `--features skylight` and the caller passes `--tier synthetic` or `--synthetic-focus`. Without the feature the error is `unsupported` ("built without skylight feature") before any hit-test. A null symbol is `unsupported` and does not abort. The default release binary is built **without** `--features skylight`, so on this Mac (and on CI) the symbol is never called end-to-end. A read-only `dlsym` on macOS 27.0.1 (26A434) found `SLPSSetFrontProcessWithOptions` and `CPSSetFrontProcess`; `SLSSetFrontProcessWithOptions` was absent. **The symbols were not called on this machine.** Synthetic focus does not restore the previous app. Partial because the feature compiles and resolves symbols, but no live invocation exercises the call path on the development Mac or in CI. |
| Agent cursor overlay | Yes | Click-through `NSWindow`, helper process | DONE | CI | P7. `computer-use overlay serve` draws a 16×16 ring at the last agent point. The main CLI sends `{x,y,session}` on `<cache>/overlay.sock` after a successful positional action. A missing helper adds `overlay helper not running` and the action still succeeds. `--no-overlay` skips the socket. `overlay serve` is `unsupported` off macOS. The helper was not started live. |
| Parallel agents | Yes | Per-session cache, no shared cursor on ax/event tiers | DONE | CI | P7. `ax` and `event` do not take `foreground.lock` and do not move the shared cursor. `activate` takes `<cache>/foreground.lock`. A second session gets `execution_failed` naming the holder pid. `--allow-foreground` skips the lock. On Windows the lock is one byte at offset 4096 so the pid at the start of the file stays readable. |
| Does not steal the cursor or the focus | Yes on the background path | Tiers `ax` and `event` | DONE | CI | P7. `ax` does not activate. `event` posts to the pid and sets `cursor_moved` when the pointer moves by more than 1 point. `--require-background` turns that into `execution_failed` and blocks the `activate` fallback. `activate` moves the cursor, raises the target, then restores the previous frontmost app (`focus_restored`). `synthetic` may change the front process and does not restore it. These tiers were not exercised live. |
| `list_apps` | Yes | `list-apps` with bundle id, pid, path | DONE | live (macOS); CI (Windows); unsupported (Linux) | P1 on macOS via NSWorkspace. P12a on Windows: `windows::Win32::UI::WindowsAndMessaging::EnumWindows` drives `list_apps`; the executable path comes from `QueryFullProcessImageNameW`. Linux still returns `unsupported`: P13b's scope is capture and input, so window and app enumeration were not part of it. That is the one thing still blocking the portal capture from being reachable. Live on this Mac: `list-apps` returns the running apps (Finder, Chrome, ChatGPT, MiniMax Code, Grok Bot, …). |
| Window inventory | Yes | `list-windows` joined to CG z-order | DONE | live (macOS); CI (Windows); unsupported (Linux) | P1 lists on-screen CG windows with z-order, bounds, layer, and window number. P2 sets `ax_matched` when an AX window shares those bounds within 2 points. The flag stays false, with a note, when Accessibility is off. Live on this Mac: `list-windows` returns the on-screen window stack with `ax_matched: false` and a `note: "ax_matched is false; Accessibility permission is off"`. |
| Launch apps | Yes | `launch` via `NSWorkspace` | DONE | CI (macOS unit-tested only); unsupported (Linux/Windows) | P1 on macOS, with `activate` and `quit`. P6 adds `quit --force` (`forceTerminate()` on macOS). A built-in deny returns `policy_denied` before the backend, on every OS. Off macOS, launch, activate, and quit of an allowed app return `unsupported`. No app was launched live during P15. |
| z-order | Yes | CGWindow list order, index 0 in front | DONE | live (macOS) | P1. An `--app` filter renumbers so 0 is that app's front window. A window id uses the `id:` prefix. Live on this Mac: `list-windows` returns z-order with index 0 in front. |
| Per-app approval / allowlist | Yes | `policy.toml` allow and deny | DONE | live (macOS `policy check`); CI (everything else) | P6. User file `~/.config/computer-use/policy.toml`, override `--policy`. A missing file is default allow plus the built-in deny list. `default = "deny"` refuses a named app that is not allowed. An unscoped mutating action under that default is `policy_denied`. A desktop screenshot that names no app is not. P7 audit lines record `decision` (`allow`, `deny`, or `confirm`), `source` (`builtin`, `admin`, `user`, `default`, or another policy source), and the real `code`. `policy check` can return `ok: true` with `decision: "deny"`; that line keeps `code: "ok"`. Live on this Mac: `policy check --app Finder` → `decision: "allow", source: "default"`; Terminal and `app.crynta.terax` are `policy_denied` via `builtin`. |
| Admin policy | Yes | `/etc/computer-use/policy.toml`, deny wins | DONE | CI | P6. Admin is read first. Deny wins. Admin allow entries do not grant. A broken admin file is `policy_denied` and fails closed. A user parse error is `invalid_arguments` and names the line. Windows uses `%ProgramData%\computer-use\policy.toml`. Unit-tested via `XDG_CONFIG_HOME` / `XDG_STATE_HOME` overrides. |
| Sensitive-action confirmation | Yes | Confirm hook, secure fields always refused | DONE | CI | P6. `--confirm-cmd`, `COMPUTER_USE_CONFIRM_CMD`, or `confirm.command`. Exit 0 proceeds. A nonzero exit, a timeout, or a spawn failure is `confirm_denied`. No hook is `confirm_required`. Quit of an allowed app confirms, including `--force`. Secure `set_value`, `type`, and a key aimed at a secure field are `policy_denied` with no hook. A key without `--element` cannot see a secure focused field. `clipboard_set` is classified here and implemented in P8. |
| Refuses terminals | Yes | Built-in deny list | DONE | CI (live on this Mac: Terminal denies via `policy check`) | P6. Bundle id, canonical executable, `.app` component, and display name. Terminals include Terminal, iTerm2, Warp, Alacritty, kitty, WezTerm, Ghostty, Hyper, Terax (`app.crynta.terax`), Windows Terminal, powershell, cmd, wt, gnome-terminal, konsole, and xterm. Password managers, Apple Passwords, and Keychain Access are on the same list. System Settings (`com.apple.systempreferences` and `com.apple.Settings`) is denied only when the window title or AX identifier contains Privacy or Security. |
| Blocked-URL error | Yes | `blocked_url` for browsers and for CDP | DONE | CI (live on this Mac: `browser navigate --url <blocked>` returned `blocked_url` without opening the websocket) | P6 for Safari, Chrome, Chromium, Edge, Brave, Arc, and Firefox. `url_blocked` matches `*` globs. A pattern ending in `/*` also matches the bare origin. The error names the pattern, not the URL. P10 CDP navigation calls the same function. |
| Path-resolved app identity | Yes | Canonical executable path | DONE | CI | P6. Allow and deny compare the canonical path. A symlink to Terminal still denies. An `allow.path` that does not match the canonical executable is `policy_denied`. |
| Relock / pause on local input | Yes | Listen-only `CGEventTap`, ignore our marker | DONE | CI (macOS); unsupported elsewhere | P6. Listen-only session tap. The marker is `CGEventField::EventSourceUserData` (field 42), constant `0x00C0_5EC0_0DE1`. Field 43 is `kCGEventSourceUserID` and is not the marker. The `paused` file next to the audit log is the cross-process source of truth. Mutating actions return `paused` after deny. The tap is macOS-only. Other platforms warn `user-input pause unavailable` when `pause_on_user_input` is true. A missing policy does not install a tap. Tests and the live smoke did not create one. P8 `doctor` reports `input_monitoring` from `CGPreflightListenEventAccess` and keeps `input_monitor` as this tap flag. It stays null until this process tries to install the tap. Live `input_monitoring` was false for Grok Bot and `input_monitor` was null. |
| Windows foreground-only plus allowlist | Yes, foreground-only | UIA patterns without foreground, policy still applies | DONE | CI (Windows only — macOS mock backend in cross-platform unit tests) | P12a ships UIA patterns without foreground (`Invoke`, `Value`, `Text`, `ScrollItem`, `ExpandCollapse`). P12b wires the documented fallback: a missing pattern or a failed pattern call is retried as tier `activate` = `SetForegroundWindow` + enigo under the same `foreground.lock`, and the result carries the `foreground required` warning because that retry is what worked. `--allow-foreground` skips the lock and still falls back; a busy lock names the holder pid; `--require-background` refuses the fallback outright. P12b also wires `element_action` into `impl_platform_backend!` for Windows — P12a wrote the dispatcher but never reached it, so every Windows element command still answered `unsupported` until now. The retry is deliberately limited to `press` and `type`: `set_value` must go through `ValuePattern` (the value-not-keystrokes parity contract, and `IsPassword` is refused before any pattern runs), `select_text` needs the `TextPattern` range, `scroll_to` needs `ScrollItemPattern`, and `focus` / `perform-action` are already correct or already refused. `focus_restored` reports what the verified `GetForegroundWindow` restore actually did rather than assuming true. P6 allowlist still applies. **Not live-tested on a Windows host.** |
| Linux | Yes (foreground-only with app allowlist per OpenAI docs) | AT-SPI, ScreenCast, RemoteDesktop or libei, XTest fallback | PARTIAL | CI | P13a wires AT-SPI (tree, element actions, doctor), X11 XTest (`tier event`), and `tier activate` (enigo on X11). P13b adds the `portal` feature: ScreenCast capture (prefer a window source, else crop the containing monitor), a PipeWire frame pull, and RemoteDesktop pointer/keyboard input on Wayland, with an optional `reis` EIS handshake. **Still partial, and not because of the portal:** Linux `list_windows` / `list_apps` / `launch` / `activate` / `quit` remain `unsupported` (P13a left them out and P13b's scope is capture and input), so `screenshot --app` cannot resolve a window target and therefore cannot yet reach `capture_window`. The portal capture is implemented and its source-selection, crop, remap and error-mapping logic is unit-tested, but no CLI command drives it end to end yet. Input *is* reachable: `--tier event` with explicit `--x/--y` needs no window enumeration. `reis` 0.7 exposes no public API to create an `ei_device`, so the EIS handshake reports availability and never dispatches relative motion. `drag`, button hold, hold-key and free-text typing are `unsupported` on the portal tier. **No live Wayland session was available on the development machine**, so nothing here has been exercised against a real compositor; CI compiles and unit-tests it only. Per OpenAI docs, Codex ships a Linux backend as well (foreground-only with an app allowlist, same shape as its Windows backend). Our advantage on Linux is the AT-SPI element paths and the XTest event tier; both are CI-only verified. |
| Batched `actions[]` | Yes | `action: "batch"` | DONE | CI | P5. One resolved target and one cache revision sequence. Each step re-validates. `stop_on_error` defaults true and sets `stopped_at`. Nested batch is `invalid_arguments`. `return_state` runs once at the end. A step's own `return_state` is ignored. |
| OpenAI `computer` tool adapter | No | `--adapter openai` | DONE | CI | P5. Translates click, double_click, scroll, type, keypress, move, wait, and screenshot. `wheel` is middle. `back`, `forward`, and `drag` were unsupported in P5; translated since P8 part A (8bd92b9). Image coordinates need a stored screenshot map. No auto-detection. |
| Anthropic `computer_20251124` adapter | No | `--adapter anthropic` | DONE | CI | P5. Translates screenshot, left/right/middle click, double_click, mouse_move, scroll, type, key, wait, zoom, and cursor_position. triple_click, left_click_drag, mouse down/up, and hold_key were unsupported in P5; translated since P8 part A (8bd92b9). |
| Screenshot coordinate remap, about 1440×900 | Yes | Default max-width 1440, map in every image, optional canvas | DONE | CI | P4. Absent `max_width` scales a screenshot down to 1440. `0` does not scale. `--canvas WxH` letterboxes (`fit: canvas`, `pad` is the top-left inset, `content_width` and `content_height` are the drawn size). `scale` stays the backing scale. `image_to_screen` and `screen_to_image` remap image pixels and screen points. The map is stored at `sessions/<session>/screenshot-map.json` mode 0600 and does not change the tree baseline. Mixed-scale monitors share one scale on a stitched desktop image. Linux and Windows desktop screenshots use the same map; their window capture is P13 and P12. |
| MCP server | No | No | DONE | live (by absence) | Out of scope on purpose. `serve` stays stdin/stdout. `--version` and `--help` confirm there is no listener; CI is the gate. |
| Permission doctor with fix text | Limited | `doctor` JSON, `--request` optional | DONE | live (macOS); CI (Windows/Linux) | P8. macOS probes Accessibility (`AXIsProcessTrusted`), Screen Recording, Post Event, and Input Monitoring. The default does not prompt and does not open Settings. `--prompt` (alias `--request`) calls the request APIs. `--open-settings` opens the first missing pane with `open`. `permissions_ok` and `missing` are the grant aggregate. The envelope `ok` means the report was produced, so the process exits 0 when a permission is missing. Each missing permission has a `fix` object (`permission`, `url`, `target`, `instruction`). Post Event uses the Accessibility pane (`Privacy_Accessibility`). Screen Recording uses `Privacy_ScreenCapture`. Input Monitoring uses `Privacy_ListenEvent`. `input_monitor` is the P6 tap and stays null when the tap was not attempted. `identity` reports the path, bundle id, whether the process is `Computer Use.app`, `codesign -dv` status, team id, and the responsible app. A bare binary's note recommends symlinking the bundled executable into `~/.local/bin`. Linux adds `session` and `atspi_bus` (address presence only; the bus connection is P13). Windows adds best-effort `uiaccess` and `integrity` from the `windows` crate (P12a replaces the earlier PowerShell probe): `OpenProcessToken` + `GetTokenInformation(TokenUIAccess)` for the UIA flag, plus `TokenIntegrityLevel` with a SID-to-RID mapping (`low / medium / high / system / protected / untrusted`). The `accessibility` field comes from `UIAutomation::new`. Both are probe fields, not required grants. Live on this Mac: `doctor` returned `permissions_ok: false`, `missing: [accessibility, post_events, input_monitoring]`, `screen_recording: true`, `responsible: Grok Bot`. `--prompt` and `--open-settings` were not run. |
| Clipboard get/set | Unknown | `arboard` 3.7.0, set requires confirm | DONE | CI | P8 part A (`8bd92b997b2db63e950c3105c82789ab11dc0d51`). `clipboard_get` returns text, or `kind: "image"` plus a mode-0600 PNG in the session cache, or `kind: "empty"`. Text wins over an image. `clipboard_set` goes through `policy::decide` and requires confirm. Empty text is `invalid_arguments`. The audit stores `chars` and never the text. Linux builds enable `image-data` and not `wayland-data-control`. A macOS arboard error that mentions the pasteboard and permission, denied, or not allowed is `permission_required`. Tests use the in-memory stub and do not touch the pasteboard. |
| Drag, mouse down/up, triple-click, hold key, xdotool names | Partial | Those actions on the same tiers | DONE | CI | P8 part A (`8bd92b997b2db63e950c3105c82789ab11dc0d51`). `drag` needs at least two points. Tier `event` posts mouse-dragged events to the pid. Tier `activate` uses enigo button down, move, and up. `mouse_down` stays down for the rest of a `serve` loop or a `batch` and is released when that process drops the input session. A later process is not held down. Triple-click is `click --count 3`. The event tier sets the click state to 1, then 2, then 3. `hold_key` is one combination for at most 30000 ms. A space-separated sequence is refused. xdotool names cover modifiers, editing keys, arrows, page keys, `Insert`, `F1`–`F24`, `plus`, and `minus`. `super` is Meta. F21–F24 are known names and `unsupported` on macOS because enigo stops at F20. `insert` uses the macOS Help key (virtual keycode `0x72`). Back and forward are mouse buttons 4 and 5. Tier `event` posts other-mouse button numbers 3 and 4. Tier `activate` uses enigo on Linux and Windows. macOS `--tier activate` for those buttons is `unsupported` and names the event tier. Auto on Linux and Windows is `activate` only. Explicit `--tier event` and `--tier synthetic` there are `unsupported`. These actions were not run live because Accessibility is off. |
| Menu bar traversal | Yes for AX menus | `menu list` / `menu click` | DONE | CI (live smoke on this Mac: `permission_required` for Accessibility) | P9. macOS walks `AXMenuBar` -> `AXMenuBarItem` -> `AXMenu` -> `AXMenuItem` case-insensitively on `AXTitle`. `--depth` defaults to 4 and caps at 8. A miss is `element_not_found`; a disabled item is `execution_failed`. The click uses `AXPress` and tier `ax`. Linux and Windows return `unsupported`. Pure path parser and stub round-trip tests. |
| Apple Events, Shortcuts, Dock | Partial | Explicit commands | DONE | CI | P9. `apple-event --app X --script '...'` runs `osascript` via argv (`-e` for AppleScript, `-l JavaScript -e` for JXA) with the P1 timeout (30s). The script body is never logged verbatim; audit records `script_bytes` and a stable FNV-1a 64-bit hash. `shortcuts run <name>` invokes the `shortcuts` binary as `["shortcuts", "run", name]` with a 60s timeout. `dock list` enumerates `com.apple.dock`'s AX children by title; `dock click --title X` is case-insensitive and uses `AXPress`. Apple events are not classified as sensitive for confirm (no `policy_deny` beyond the built-in deny list). Linux and Windows return `unsupported` from the dispatcher. Missing-binary off macOS becomes `unsupported`, not `execution_failed`, on `shortcuts run`. |
| Spaces | Limited | Screen grouping shipped. Moves are private and optional. | DONE | CI | P9. `spaces` reads `NSScreen` and the existing CG window list and groups on-screen windows by the screen that contains each window's center. A window whose center is outside every screen falls into the first screen. Mission Control moves are not attempted. The skylight feature, when active, keeps the report best-effort; the action's `note` field names that. Linux and Windows return `unsupported`. |
| CDP for Chrome | Yes, attach only | `chromiumoxide` behind feature `cdp` and `--cdp` | DONE | CI (live on this Mac: `cdp_unavailable` for `browser tabs` against the running Chrome; `unsupported` without `--cdp`; `blocked_url` for a denied `--cdp-url` without opening the websocket) | P10. `chromiumoxide` 0.9.1 + a minimal `tokio` (rt, current-thread). The CLI thread stays sync: every browser command builds a fresh `tokio::runtime::Builder::new_current_thread()` runtime, `block_on`s the async CDP request, and drops the runtime on the way out. The chromiumoxide handler is spawned on that runtime so it is also dropped. The user owns the Chrome instance; we never spawn, restart, or quit Chrome — attach-only is enforced, with no fallback to `Browser::new_page` (no page target → `cdp_unavailable`). Port discovery reads the `--app` pid's argv: macOS `sysctl KERN_PROCARGS2`, Linux `/proc/<pid>/cmdline` (NUL-separated UTF-8), **Windows best-effort `unsupported` (pass `--cdp-url`)**. CLI flags: `--cdp` gates the whole subcommand tree; `--cdp-url ws://host:port` (also `http(s)://`) overrides argv discovery. Subcommands: `browser tabs|activate|navigate|click|text|type|eval`. `browser text --selector S` reads the first element's `innerText` and returns it; `browser type --selector S --text T` types into the matched element (or the page body). **`browser_click` resolves a CSS selector and clicks the element centre via chromiumoxide's CDP `Input.dispatchMouseEvent`; this is the CDP click path, not the screen-space transform.** The deviation is documented in the P10 design notes and at the function. Actions `BrowserTabs/Activate/Navigate/Click/Text/Type/Eval` are wired through `dispatch_inner` and the audit log. `[browser].blocked_urls` is loaded from the policy file at dispatch and `policy::url_blocked` runs before `Page.navigate` (request URL) *and* against the current page URL before `click`, `text`, `type`, `eval`, and `activate`; the websocket is never opened for a denied request URL and never queried for a blocked-current-page operation. `browser eval` returns `script_chars` + a stable `DefaultHasher` hash in addition to `result`; `browser type` returns `chars`. The audit row records `chars` for `browser_type` and the script length + hash for `browser eval`; the bodies themselves never appear in any log. Errors: `unsupported` (feature off), `cdp_unavailable` (no debug port / connection refused / no page target), `blocked_url` (policy refused before send, or current URL matches `[browser].blocked_urls`), `element_not_found` (selector miss), `policy_denied` (denied bundle id). Default build (no feature) refuses every browser command. |
| OCR and set-of-marks | Screenshot pixels | Vision on macOS, Media OCR on Windows, numbered boxes | DONE | CI (macOS: live Vision unit test renders a bitmap-font `HELLO` and asserts Vision returns it; end-to-end `ocr` against a real window was not run on this Mac because the host was screen-locked during the smoke run) | P11 macOS path is a real `VNRecognizeTextRequest` call, not a stub. The RGBA capture is PNG-encoded into `NSData`, fed to `VNImageRequestHandler::initWithData_options:` with `VNRequestTextRecognitionLevel::Accurate` and `setUsesLanguageCorrection(true)`. Each `VNRecognizedTextObservation` becomes one `OcrBlock` with its normalized bounding box flipped from Vision's bottom-left origin to image pixels and redacted against secure-field frames. A macOS-only test renders a tiny bitmap-font `HELLO` into an RGBA buffer and asserts Vision returns a block containing it; passes locally with `cargo test --release vision_recognizes_drawn_text -- --ignored --nocapture`. **Linux: `unsupported` ("no OCR backend on Linux in 0.2.0").** No Tesseract, no `asyc-tesseract` glue, no plugin. Windows since P12b: a real `Windows.Media.Ocr.OcrEngine` call, not a stub. The RGBA capture is swizzled to BGRA8 premultiplied (the engine's only accepted format; a screenshot is opaque so straight and premultiplied alpha are the same bytes), copied into a WinRT `IBuffer` via `CryptographicBuffer::CreateFromByteArray`, wrapped in a `SoftwareBitmap`, and handed to `OcrEngine::TryCreateFromUserProfileLanguages()`. One `OcrBlock` per `OcrLine`, whose box is the union of its words' `BoundingRect`s. `OcrWord::BoundingRect` is already image pixels with a **top-left** origin, so — unlike Vision — there is no flip; `vision::ocr::ocr_box_to_image` only clamps and rounds. The engine reports no confidence, so `confidence` stays 0 like the macOS path when Vision cannot say. The pass runs on a dedicated worker that calls `RoInitialize(RO_INIT_MULTITHREADED)` itself (Media OCR refuses an uninitialized thread) and is joined with a 10 s `recv_timeout`, so a wedged engine cannot hang the CLI; the worker returns only `Send`-able values, matching the P12a UIA worker. `note` carries the recognizer language (`Media OCR recognizer language: en-US`). No recognizer language pack is `execution_failed` naming Settings → Apps → Optional features, not `unsupported`. Verified on the Windows CI runner: `windows_media_ocr_runs_and_never_returns_unsupported` returns zero blocks for a blank image with `en-US`.  Set-of-marks: tree indices on a window target, `oN` OCR indices otherwise. Audit row stores action name + decision only — recognized text never leaves the JSON response body. Limitations: end-to-end OCR needs Screen Recording plus an unlocked display; the host was screen-locked during the on-machine smoke run, so the binary returned `display_unavailable`. |
| Signed helper `.app` so TCC sticks | Yes, a helper binary | `Computer Use.app`, signing optional in CI | DONE | live (ad-hoc); CI (Developer ID + notarization, only when secrets exist) | P8. Bundle id `ai.lindoelio.computer-use`, `LSUIElement`, usage strings, and an empty hardened-runtime entitlements file. `scripts/package-macos-app.sh` ad-hoc signs when `MACOS_SIGN_IDENTITY` is unset and names the zip `computer-use-macos-arm64-app-unsigned.zip`. Developer ID (`codesign --deep --force --options runtime --timestamp --entitlements`) and notarization run only when the secrets in `docs/PACKAGING.md` exist. Those secrets are not configured. This phase did not notarize or create a tag. A bare binary does not get the bundle id. Symlink `Computer Use.app/Contents/MacOS/computer-use` into `~/.local/bin`. Live ad-hoc packaging of the release binary passed `codesign --verify --verbose` and `plutil -lint`. The bundled `doctor` reported `bundle_id` `ai.lindoelio.computer-use`. The crate tarball does not include `packaging/` or `scripts/` until P15. |
| Virtual display / isolation | No | `--display`, Xvfb recipe, private CGVirtualDisplay marked experimental | **PARTIAL** | **CI** (filter + recipe: live on macOS via `displays` + `isolation`; **CGVirtualDisplay probe only on this Mac — symbols are absent, no display is ever created**) | P14. `--display <id>` resolves a monitor from `displays` and gates every screen point (`click`, `double-click`, `triple-click`, `move`, `scroll`, `drag`, `mouse-down`, `mouse-up`) plus `--region`/`--window` bounds; anything outside is `invalid_arguments`, and an unknown id is `invalid_arguments` naming the ids that do exist. Points stay virtual-desktop points — the filter narrows what may be addressed, it does not rebase. A filtered screen capture passes the monitor rectangle to the shared xcap path, so the screenshot map origin is the monitor origin on all three platforms. `--display virtual` prints the Xvfb recipe (`Xvfb :99 -screen 0 1440x900x24`) and, with `--exec -- <cmd...>`, execs that command with `DISPLAY=:99` set; **it never starts Xvfb**, because an agent must not leave a stray X server behind, and on macOS/Windows it is `unsupported`. The `virtual-display` feature resolves `CGVirtualDisplayDescriptorCreate` and `CGVirtualDisplayCreate` with `dlsym` against an explicitly `dlopen`ed CoreGraphics (the global namespace cannot see an unopened framework) and **stops there**: no display is ever created, because the descriptor is an Objective-C object needing private class setup this build does not do. `isolation` reports `experimental` only when both symbols resolved and `unsupported` otherwise — on this macOS 27 machine both are absent, so it honestly reports `unsupported`. The VM proxy and the five infeasible items are documented in `docs/ISOLATION.md` and mirrored in the `isolation` JSON, with a snapshot test binding the two together. **Partial because the macOS path is probe-only and never creates a display**, the Linux path is a recipe (no Xvfb), and there is no live sandbox on this Mac. |

**Tally — 0.2.0 parity table, rechecked for `docs(P15): honest parity audit`.**

- Status counts across the 53 rows: **48 DONE / 5 PARTIAL / 0 TODO**.
- Verification counts across the 53 rows:
  - **live** on this Mac (exercised on a real window on the development Mac): 4 rows — `list_apps`, `Window inventory`, `z-order`, and `Permission doctor with fix text`. The macOS portion of `Per-app approval / allowlist` was also exercised live via `policy check --app Finder` → `decision: "allow", source: "default"`. `Signed helper .app` was live for ad-hoc packaging only. `Blocked-URL error` was exercised live via `browser navigate --url <blocked>` → `blocked_url` without opening the websocket. `MCP server` was live by absence (`--version` and `--help` confirm there is no listener).
  - **CI**: 48 rows — every row that compiled and unit/integration-tested in CI on Ubuntu/macOS/Windows but was not exercised against a real window on this Mac. This includes most of the macOS AX / event / synthetic rows, all of Windows, all of Linux, the OpenAI / Anthropic adapters, the batched-action adapter, the screenshot remap, the clipboard path, the drag / triple-click / hold-key / xdotool names, the menu / apple-event / shortcuts / dock / spaces commands, the CDP attach path, the OCR / set-of-marks row, and the `Virtual display / isolation` row's `CGVirtualDisplay*` symbol probe path.
  - **compile-only**: 1 row — `Synthetic focus enforcer`. The default release binary is built **without** `--features skylight`, so the symbol call path is never invoked on the development Mac or in CI; the row is wired and compiles behind the feature flag.

**The five PARTIAL rows and why.**

- **Scroll into view before the action** — implemented per platform (`AXScrollToVisible` on macOS, UIA `ScrollItem` on Windows, AT-SPI `Component::scroll_to` on Linux), but the implementation depends on the platform interface being advertised. A window that does not expose `ScrollItemPattern` (Windows) or `Component` (Linux) is `execution_failed`, not a no-op.
- **select_text** via selected range — implemented per platform (`AXSelectedTextRange` on macOS, UIA `Text` pattern on Windows, AT-SPI `Text::set_selection` on Linux), but a window without `TextPattern` (Windows) or `Text` (Linux) is `execution_failed`. P12b deliberately adds no `activate` retry because no keystroke sequence is reliably the same action.
- **Synthetic focus enforcer (SkyLight / CPS)** — wired behind `--features skylight` and the `focus::claim` call site, but on the default release binary (no skylight feature) the symbol call path is never invoked on this Mac or in CI; a read-only `dlsym` on macOS 27.0.1 found `SLPSSetFrontProcessWithOptions` and `CPSSetFrontProcess` but `SLSSetFrontProcessWithOptions` was absent, and the symbols were not called.
- **Linux** — `list_windows` / `list_apps` / `launch` / `activate` / `quit` are still `unsupported`, so `screenshot --app` cannot reach `capture_window` yet. The portal capture path is implemented and unit-tested, but no CLI command drives it end-to-end. Codex ships a Linux backend (foreground-only with an app allowlist per OpenAI's docs); we don't yet.
- **Virtual display / isolation** — the macOS `CGVirtualDisplay*` path is probe-only (no display is ever created), the Linux path is a recipe (no Xvfb is started), and there is no live sandbox on this Mac. The `--display <id>` filter and `displays` / `isolation` reporting did run live on this Mac; the `CGVirtualDisplay*` symbol call path did not.

### vs Codex — honest summary, not marketing

How this repository compares to OpenAI Codex Computer Use as it is publicly documented (model surface, not the harness). Each row says where we exceed, where we match, and where we fall short. There is no claim beyond what the code or the doc does. The Codex CLI is itself a separate harness; what we compare against here is the public model surface and what the Codex CLI documents for it (allowlisted apps, foreground-only on Windows/Linux, and per-target-window capture).

**Where we exceed the public Codex surface**

- **Stdin/stdout protocol, no MCP server.** `--adapter openai` and `--adapter anthropic` translate the public OpenAI `computer` tool shape and the Anthropic `computer_20251124` shape into the native protocol and back. The native protocol (`Action`, `CallContext`, `ComputerError`) is the same one Codex describes, plus extras the harness does not have (`pause`/`resume`, the policy subsystem, the OpenAI/Anthropic adapters themselves, batched actions, CDP attach).
- **Policy + sensitive-action confirms.** Built-in deny of terminals and password managers, user and admin policy files, `--confirm-cmd` hook, `policy_denied` / `confirm_required` / `confirm_denied` codes, and `paused` after local user input. Codex's public surface does not describe a policy subsystem; ours ships one and unit-tests it.
- **Background input on Windows and Linux.** Codex ships a foreground-only Windows backend with an app allowlist per OpenAI's docs (the same shape as their Linux backend). Our Windows advantage is UIA control patterns without foreground (`Invoke`, `Value`, `Text`, `ScrollItem`, `ExpandCollapse`) with a tier `activate` retry under the `foreground.lock` (P12a + P12b). On Linux our advantage is AT-SPI element paths and an XTest event tier (P13a + P13b), with RemoteDesktop portal for Wayland (P13b). **Both Windows and Linux advantages are CI-only verified; the development Mac did not exercise either backend live.**
- **Cross-platform CDP attach.** `chromiumoxide` 0.9.1 behind `--features cdp` and `--cdp-url`. The user owns the Chrome instance; attach-only is enforced, no fallback to `Browser::new_page`. The click path is CDP `Input.dispatchMouseEvent`, in page coordinates — not a screen-space transform. Codex's harness does not document an attach-only CDP path; ours does.
- **Isolation primitives.** `--display` filter, `--display virtual` (Xvfb recipe, never starts Xvfb), `--synthetic-focus` behind `--features skylight`, click-through overlay, parallel agents via `foreground.lock`. None of these are part of the public Codex contract.

**Where we match Codex**

- Numbered key-window AX tree per observation, with truncation, URL shortening, and secure-field redaction.
- `--element` targeting with re-fetch + revalidate + `stale_element`.
- Tier `event` posting via `CGEventPostToPid` (macOS only; field 42 marker, field 91 window id, field 40 target pid).
- Tier `ax` accessibility actions and values, including `AXScrollToVisible`, `AXShowMenu`, the four page-scroll actions, and the increment/decrement family.
- Tree diff and force-full flag with a per-session cache revision sequence.
- 1440-default `max-width` with `image_to_screen` / `screen_to_image` in every image.
- Blocked-URL `url_blocked` for Safari, Chrome, Chromium, Edge, Brave, Arc, and Firefox, plus `blocked_url` for the CDP path.
- Per-target-window capture (Codex's harness captures the target window on macOS / Windows / Linux, just as `capture_window` does here).

**Where we fall short**

- **No end-to-end benchmark, minimal live testing.** OpenAI ships Codex with a model trained for computer use and reports GPT-5.5 at 78.7% on OSWorld-Verified; Codex is a product harness live-tested at scale. This repository has neither an end-to-end benchmark, nor a model, nor a product harness — it is a protocol, a CLI, and a backend. The Codex number is not apples-to-apples; it is one comparator on the model axis that we do not compete on.
- **Linux is CI-only, not live.** No Wayland compositor was available on the development machine. AT-SPI works on X11 with `DISPLAY`; RemoteDesktop portal works when `portal` is compiled in. `list_windows` / `list_apps` / `launch` / `activate` / `quit` are still `unsupported`, so `screenshot --app` cannot reach `capture_window` yet. `reis` 0.7 has no public `ei_device` API, so the EIS handshake reports availability only.
- **Accessibility was off for this process on the development Mac.** The full live path (press, value, set-value, perform-action, scroll-into-view, hit-test, event posting, AX press via SkyLight) was not exercised against a real window because the TCC grant is missing in this profile. `doctor` reports it. The code paths and unit tests pass; the live capture frame did not.
- **Windows is CI-only, not live.** UIA patterns, tier `activate`, and the per-window `windows-capture` path are wired and tested on the macOS harness with the mock backend, and CI compiles and unit-tests on `windows-latest`. No live Windows host ran in P15.
- **CDP click uses `Input.dispatchMouseEvent`.** No transform — the click is in the page coordinate system. The deviation is documented in P10 and at the function; it is not silently screen-space.
- **OCR is `unsupported` on Linux.** The macOS and Windows paths are real (`VNRecognizeTextRequest` and `Windows.Media.Ocr.OcrEngine` respectively, both unit-tested). The Linux path has no OCR backend in 0.2.0 — no Tesseract, no plugin. Codex's harness does not claim OCR coverage; this is a parity gap we name explicitly.
- **Windows CDP port discovery is best-effort `unsupported`.** `QueryFullProcessImageNameW` does not give us argv; pass `--cdp-url ws://host:port` on Windows.
- **Linux OCR is `unsupported`** in 0.2.0. No Tesseract, no `asyc-tesseract` glue, no plugin.
- **Private-API features are experimental, fail closed.** `--tier synthetic` / `--features skylight` and `--features virtual-display` use `dlsym` and never call the symbol on this machine. They are off unless the feature is compiled in and the caller passes the matching flag.
- **Unsigned `.app` unless the secrets exist.** `Computer Use.app` is built and ad-hoc signed by `scripts/package-macos-app.sh`; Developer ID signing and notarization only run when the secrets in `docs/PACKAGING.md` exist on CI. They are not configured today.
- **MCP server is intentionally absent.** `serve` is stdin's protocol, not a network one. There is no TCP listener in any release.

## Progress
Update this table at the end of a phase. `commit` is the full hash on `main` of the commit that finished that phase. The planning row is this commit, so it cannot name its own hash. Do not mark a phase `DONE` until CI on that commit is green for the three OS jobs.

| Phase | Status | Commit | Notes |
| --- | --- | --- | --- |
| P0 Planning | DONE | this commit | No code changes. Hash is `git rev-parse HEAD` after this commit lands. |
| P1 Targeting, inventory, doctor | DONE | `921e942947a1ce841b3026c0d33fdd2caecc701e` | `rust-version` 1.91. Built-in deny. macOS list-apps, list-windows, launch, activate, quit, and doctor. Feature commit `4266448035c78352c79b9025db01c445d8d0ff39`; this hash is the Windows clippy fix CI accepted. `ax_matched` stays false until P2. `activate` does not raise one AX window. `quit` is `terminate()` only. Linux and Windows inventory returns `unsupported`. Their doctor probes stay null (P13 / P12) and `--request` does not prompt there. `dirs` is deferred to P2. Live doctor on this machine saw Accessibility denied for Grok Bot and Screen Recording granted. |
| P2 macOS AX tree, diff, cache | DONE | `605069185333dd9a018d2e58a66c9bd7564ff6c9` | Native AX tree, numbered text, diff, and session cache. Feature commit `514f94783b8ef92ee8c2e8ab01a1af04dd7ff284`; this hash is the Windows clippy fix CI accepted. Linux and Windows `tree` returns `unsupported`. osascript is the fallback only when creating the application element fails for a reason other than permission or a messaging timeout (`source=osascript`). The observer only marks a pid dirty; every observation still re-fetches. A crash, or a caller that never calls `shutdown_sessions`, can leave Electron flags set until the target app quits. A later `session end` restores from the cache and does not write an attribute this process did not change. Revalidation data is stored; `stale_element` is P3. Helper walks are capped at 3. `ax_matched` stays false when Accessibility is off. `--app` name ties prefer Regular activation policy. Terax (`app.crynta.terax`) is on the built-in deny list. Live `tree --app Finder` returned `permission_required` for Grok Bot. |
| P3 Element actions | DONE | `b849daae9081dcf8e6748828182849ef3018bce5` | `press`, `focus`, `set-value`, `select-text`, `perform-action`, `scroll-to`, and `click --element`, with revalidation. Feature commit `93892935bf1c03da044cc5189eafc7ceeddd845b`; this hash is the off-macOS clippy fix CI accepted. A role, identifier, or frame change beyond 2 points, or a cached pid, window, or revision header that does not match, is `stale_element` and performs nothing. Element actions update `observed` and leave the `tree --diff` baseline. `type --element` uses `AXValue` when settable (`tier: ax`); otherwise it focuses and types with enigo (`tier: enigo`). `type` without `--element` stays enigo. Secure fields refuse `set_value`, `select_text`, and `type --element` with `policy_denied` and the value is never read. The built-in deny list runs before any AX call on the target. `click --x --y` hit-tests first; a hit-test error is a warning and the enigo click still runs. Event posting is P7. A failed post-action re-read is a warning and keeps the previous revision. Linux and Windows return `unsupported` (P13 / P12). Live `focus --app Finder --element 0` returned `permission_required` for Grok Bot. No live action was performed. |
| P4 Window capture and remap | DONE | `959495a2d3e8088019d8f98e3b3f8ed0e69e1b99` | Per-window ScreenCaptureKit capture and the screenshot map. Feature commit `7455d5f38a054d29ed08a378d143b2676db5f87a`; this hash is the off-macOS clippy fix CI accepted. Default max-width is 1440 when `max_width` is absent. `--max-width 0` keeps the source width; that used to be `invalid_arguments` (changelog in P15). macOS 14+ uses `SCScreenshotManager`; older systems and a manager failure take one `SCStream` frame. `ignoreShadows` is `ignoreShadowsSingleWindow`. Other ScreenCaptureKit errors crop xcap (`capture_source: xcap`, `occluded: true`). Permission and a window missing from both shareable queries do not fall back. The map file is separate from the tree snapshot. Mixed-scale monitors keep one scale on a stitched desktop image. This crate does not call `CGWindowListCreateImage`. Windows `screenshot --app` returns `unsupported`; on Linux P13b implements the portal capture path but `list_windows` is still `unsupported`, so `screenshot --app` returns `unsupported` there too. Live `screenshot --app Finder` returned `window_not_found`: Finder had no on-screen non-desktop window. Screen Recording was granted. Accessibility was denied, so sheet compositing was not exercised. No app was activated. The `sck` success path was not confirmed on a live window. |
| P5 State, batch, adapters | DONE | `5b39a5ea8482673e1d77351b935e1b02e576f428` | Post-action `return_state` (`--state`) snapshots `TreeReport.full_text` and refreshes the `tree --diff` baseline. Without it, `observed` updates and the baseline stays put. `include_screenshot` (`--screenshot`) uses the P4 capture path. A capture or tree failure leaves the action result, nulls that field, and adds `warnings`. Read-only actions ignore both flags. `screenshot` can return state and copies its body into `state.screenshot`. JSON `batch` shares one target and one revision sequence, re-validates each step, and applies `return_state` once at the end. `stop_on_error` defaults true and sets `stopped_at`. Nested batch is `invalid_arguments`. OpenAI `computer` and Anthropic `computer_20251124` adapters set `coord=image` and remap through `screenshot-map.json`. A missing map is `invalid_arguments` ("take a screenshot first"). Drag, triple-click, mouse down/up, `hold_key`, and OpenAI back/forward buttons were unsupported in P5; translated since P8 part A (8bd92b9). OpenAI `wheel` is middle. OpenAI `wait` with no duration is 0 ms. `cursor_position` is read-only. `display_unavailable` covers a locked or asleep session and an empty display or shareable-window list, including a window-targeted screenshot before window resolution. `session prune` removes empty dirs and stale dirs whose owner pid is dead and older than the TTL (default 24h). A live pid with content is kept. A pid above `i32::MAX` counts as not alive, because Linux `kill -0` would treat it as `-1`. Tests set `XDG_CACHE_HOME`. Feature commit `fe7c79bf599a8da6f2e56b7bdb428f4d8a3d8af9`. `6c4f784269b1d336627278f829cb1e61414b535d` limits the missing-window helper to macOS and tests. This hash is the Linux pid check CI accepted. Live smoke used a temp cache with the screen locked. Desktop screenshot, `screenshot --app Finder`, and both adapter screenshot actions returned `display_unavailable` ("the screen is locked"). `session prune --dry-run` returned ok. No click, type, activate, launch, or quit. Accessibility was not exercised. Pixel capture was not confirmed. |
| P6 Safety | DONE | `c874640e7849978a4386cce69c6274323eca86cd` | Policy, audit, confirm hook, kill switch, and blocked URLs. Feature commit `2452bcf30eadf9affd5eebcff6d06f621c9b0f25`. This hash is the CI fix: Terminal is `policy_denied` on every OS, and the Windows admin path has no needless return. `toml` 0.8.23. The marker field is `EventSourceUserData` (42), not field 43. `pause_on_user_input` defaults false, so a missing file does not install a tap. Admin allow entries do not grant. `com.apple.Settings` is privacy-pane only. Audit is append-only JSONL next to a `paused` file, mode 0600, and never stores typed text or blocked URLs. A key without `--element` cannot see a secure focused field. `clipboard_set` is classified and implemented in P8. Linux and Windows still return `unsupported` for launch, activate, and quit of an allowed app, after confirm when the action is sensitive. The tap is macOS-only. Other platforms warn `user-input pause unavailable` only when `pause_on_user_input` is true. The stub `input_monitor` stays null. Live smoke used temp `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, and `XDG_STATE_HOME`. `policy check` denied Terminal and `app.crynta.terax` (`builtin`), allowed Finder (`default`), and denied Finder from a user file (`user`). The audit JSONL had four `policy_check` lines. Terminal's bundle id was null because that name did not resolve to a running app. No click, type, activate, launch, quit, or event tap. Accessibility was not required. |
| P7 Background input and overlay | DONE | `91f3a40237560d07144da69ad37905b6f3c65c08` | Background tiers, SkyLight focus, and the agent cursor. Feature commit `bc40098d93ee85e956f36b9f330a4da59024d756`. CI fixes: the default input method is generic so Linux and Windows can call it (`dffd2906c621e1655409105717b35f57a756552e`); macOS-only helpers are allowed as dead on other platforms (`f345982830a6c21afdf01b6ed55c59451190ece1`); Windows locks one byte at offset 4096 so the holder pid stays readable (this hash). CI run `39231485609` is green on rustfmt, ubuntu, macos, and windows. Auto uses `ax` for presses and value changes, and `event` then `activate` for coordinates, type without an element, key, and scroll. Fallback runs only on `execution_failed` when `foreground.lock` is free and `--require-background` is unset. `synthetic` is never automatic. Without the `skylight` feature, `--tier synthetic` is `unsupported` before hit-test. `CGEventPostToPid` returns void. Events stamp field 42 with `policy::SYNTHETIC_EVENT_USER_DATA`, field 40 with the pid, and fields 91 and 92 with the window id. A non-settable `type --element` posts with window id 0. `activate` records the frontmost app, takes the lock, runs enigo, and restores (`focus_restored`). `ActivateIgnoringOtherApps` is deprecated and ignored on macOS 14+. `synthetic` may change the front process and does not restore it. Audit lines record `decision`, `source`, and the real `code`. Linux and Windows explicit `--tier event` and `--tier synthetic` are `unsupported`. Auto there is `activate` and moves the cursor. `overlay serve` is `unsupported` off macOS. A missing helper adds `overlay helper not running`. `--no-overlay` skips the socket. Drag stays P8. Live smoke on the default binary (no `skylight` feature): `doctor` was ok with Accessibility false for Grok Bot (`com.anysphere.sand`), Screen Recording true, and `input_monitor` null. `--help` lists `--tier`, `--synthetic-focus`, `--allow-foreground`, `--require-background`, `--no-overlay`, and `overlay serve`. `click --x 1 --y 1 --tier synthetic` returned `unsupported` ("built without skylight feature") and did not post. The overlay helper was not started. No live click, type, key, activate, or event post, so tiers `ax`, `event`, `activate`, and `synthetic` are untested on a real window. Read-only `dlsym` on macOS 27.0.1 (26A434) found `SLPSSetFrontProcessWithOptions` and `CPSSetFrontProcess`. `SLSSetFrontProcessWithOptions` was absent. Those symbols were not called. |
| P8 More actions, clipboard, `.app` | DONE | `9ade1ea89afc847dd0b7d6f1d61f1713668f6630` | Part A feature `8bd92b997b2db63e950c3105c82789ab11dc0d51`, CI fix `22aca877c9bd2bcd884cdb3bea8fa4c3f47a9236`: drag, mouse down/up, triple-click, hold key, xdotool keysyms, clipboard (arboard, set requires confirm), zoom, and the adapters. Part B feature `e28df3349f56f8cb24f5f0c94de80e57df74a629`. This hash allows the macOS doctor helpers as dead on other platforms. CI run `39294105796` is green on rustfmt, ubuntu, macos, and windows. The macOS job packaged the ad-hoc app and checked the bundle id. `doctor` reports Accessibility, Screen Recording (`screen_capture` and `screen_recording`), Post Event, and Input Monitoring. `permissions_ok` is the grant aggregate. Envelope `ok` means the report was produced, so exit code stays 0 when a permission is missing. `--prompt` (alias `--request`) is the only path that calls the request APIs. The default does not prompt and does not open Settings. `--open-settings` opens the first missing pane. Post Event uses the Accessibility pane. `input_monitor` stays the P6 tap flag and is null when this process has not tried to install the tap. `identity` carries path, bundle id, `Computer Use.app`, codesign status, team id, and the responsible app. A bare binary's note recommends the `~/.local/bin` symlink. Linux adds `session` and `atspi_bus` (presence only, no connection). Windows adds best-effort `uiaccess` and `integrity`. Those are not required grants. `Computer Use.app` is `ai.lindoelio.computer-use`, `LSUIElement`, version `0.1.0` from Cargo.toml. The packaging script ad-hoc signs when `MACOS_SIGN_IDENTITY` is unset (`computer-use-macos-arm64-app-unsigned.zip`) and Developer ID signs plus notarizes when the secrets in `docs/PACKAGING.md` are set. The release workflow was not triggered. Signing secrets are not configured. Entitlements are an empty hardened-runtime dict. The crate include list does not ship `packaging/` or `scripts/` (P15). Live `doctor` and `--json` did not prompt and exited 0. Accessibility, Post Event, and Input Monitoring were denied for Grok Bot (`com.anysphere.sand`). Screen Recording was granted. `bundle_id` was null and `input_monitor` was null. The release binary packaged under `/tmp` passed `codesign --verify --verbose` and `plutil -lint`. The bundled `doctor` reported `bundle_id` `ai.lindoelio.computer-use` and `identity.bundled` true. `--prompt`, `--open-settings`, clicks, typing, event posts, notarization, and tags were not run. |
| P9 Menu bar, Apple events, Dock, Spaces | DONE | `aed8cf3077d05a39495f4d07a3d5d8bff5dac7eb` | `menu list --app X` walks `AXMenuBar` -> `AXMenuBarItem` -> `AXMenu` with case-insensitive `AXTitle` match and a default depth of 4 (max 8). `menu click --app X --path "File > Save"` parses `>` (and `→`) as the separator, walks the same chain, and `AXPress`es the leaf. A miss is `element_not_found`; a disabled item is `execution_failed`. `apple-event --app X --script '…'` invokes `osascript` via argv only (`-l JavaScript` for JXA). The script body is never logged verbatim; the audit line records `script_bytes` and a stable FNV-1a 64-bit hash. P1 run-time timeout is `APPLE_EVENT_TIMEOUT_SECS` (30s). Apple events are mutating and denied when the `--app` selector matches the built-in deny list. Off macOS, every P9 command returns `unsupported`. `shortcuts run <name>` invokes `shortcuts run <name>` via argv with a 60s timeout. `dock list` enumerates `com.apple.dock`'s children by `AXTitle`; `dock click --title X` is case-insensitive and `AXPress`es the matching item. `spaces` reads `NSScreen` and the existing CG window list and groups on-screen windows by the screen that contains each window's center; a window outside every screen falls into the first screen. Mission Control moves are not attempted. The skylight feature keeps `spaces` best-effort: a one-line `note` is added when the feature is on. P6 sensitive-action policy does **not** gate `apple-event` or `shortcuts run` (no `policy_deny`), so the built-in deny list is the only filter. Linux and Windows `shortcuts_run` returns `unsupported` from the dispatcher before reaching the helper, so a missing `shortcuts` binary does not become `execution_failed` off macOS. CI run `39343088574` is green on rustfmt, ubuntu, macos, and windows. Local: `cargo fmt --all -- --check && cargo build && cargo test --all-targets && cargo clippy --all-targets --all-features -- -D warnings` all pass. 168 unit tests + 22 CLI tests cover menu path parsing (round-trip a Finder menu fixture, reject empty input, empty segments, and trailing separators), the osascript argv vector (empty / whitespace scripts rejected before the spawn, FNV-1a hash deterministic and 16 hex chars), the shortcuts helper (empty / whitespace names are `invalid_arguments`), the macOS-side map_attr_error translation, the window-groups algorithm (by center, orphan falls into the first screen, empty display list), the stub `menu_list` round trip with nested children, `menu_click` path recording and disabled rejection, `apple_event` only logging length and language (the script body never reaches the audit line), `apple_event` JXA flag and failure code, `dock_list` + case-insensitive `dock_click` and `element_not_found` miss, and `spaces` rendering a fake screen list. CLI integration tests cover `menu list/click`, `apple-event`, `shortcuts run`, `dock list/click`, and `spaces` returning `unsupported` on Linux and Windows with the platform name in the message, `--help` listing the new commands, and `exec` round-tripping each action. Live smoke on this Mac (no live menu/dock/shortcuts click per the design): `menu list --app Finder` returned `permission_required` (Accessibility denied for Grok Bot, expected — the structured permission error was confirmed). `dock list` returned the same `permission_required` (Accessibility denied). `spaces` returned an `ok` JSON with the built-in Retina Display and 14 grouped windows (Dock, Notification Center widgets, Chrome, Terax, ChatGPT, Grok Bot, the Accessibility Auth Warn dialog, Problem Reporter, Window Server Menubar, this MiniMax Code session, and WhatsApp). `apple-event --app Finder --script 'get name'` returned `execution_failed` with `osascript` stderr ("osascript: osascript: No such file or directory"); the binary path and argv (`osascript -e "get name"`) are correct, the failure is the runtime sandbox refusing to exec the spawned process, which is documented as `execution_failed` per the design (Apple events are not classified as sensitive and there is no confirm hook). No menu click, dock click, or shortcut run was attempted. |
| P10 CDP | DONE | `546045e0f1c87e856ec2b4e1b7744430b01d602f` | Feature `cdp`. `chromiumoxide` 0.9.1 + a minimal `tokio` (rt, current-thread). The CLI thread stays sync: every browser command builds a fresh `tokio::runtime::Builder::new_current_thread()` runtime, `block_on`s the async CDP request, and drops the runtime on the way out. The chromiumoxide handler is spawned on that runtime so it is also dropped. The user owns the Chrome instance; we never spawn, restart, or quit Chrome. Port discovery reads the `--app` pid's argv: macOS `sysctl KERN_PROCARGS2`, Linux `/proc/<pid>/cmdline` (NUL-separated UTF-8), Windows best-effort `unsupported` (pass `--cdp-url`). CLI flags: `--cdp` gates the whole subcommand tree; `--cdp-url ws://host:port` (also `http(s)://`) overrides argv discovery. Subcommands: `browser tabs|activate|navigate|click|text|type|eval`. `browser text --selector S` reads the first element's `innerText` and returns it in the result; it does not type. `browser type --selector S --text T` types into the matched element (or the page body when no selector is given). `browser_click` resolves a CSS selector and clicks the element centre via chromiumoxide's CDP `Input.dispatchMouseEvent` (see *Click path* in the P10 design notes for the documented deviation from the screen-space transform). Actions `BrowserTabs/Activate/Navigate/Click/Text/Type/Eval` are wired through `dispatch_inner` and the audit log. `[browser].blocked_urls` is loaded from the policy file at dispatch and `policy::url_blocked` runs before `Page.navigate` (request URL) *and* against the current page URL before `click`, `text`, `type`, `eval`, and `activate`; the websocket is never opened for a denied request URL and never queried for a blocked-current-page operation. `browser eval` returns `script_chars` + a stable `DefaultHasher` hash in addition to `result`; the audit row only records the action name (the script body never appears in any log). `browser type` records `chars` in the audit row; the text body never appears. Errors: `unsupported` (feature off), `cdp_unavailable` (no debug port / connection refused / no page target — attach-only never calls `new_page`), `blocked_url` (policy refused before send, or current URL matches `[browser].blocked_urls`), `element_not_found` (selector miss, mapped from chromiumoxide's `CdpError::Chrome`/`ChromeMessage` messages containing `no node` or `node with given id ... not found`), `policy_denied` (denied bundle id). Default build (no feature) refuses every browser command — `invalid_arguments` for the CLI subcommand gate, `unsupported` for the JSON request path. Tests: `parse_debug_port` for sysctl/`/proc`/Windows layouts, every `browser_*` returns `unsupported` when `cfg(not(feature = "cdp"))`, request-type deserialization round-trips all seven actions, `map_cdp_error` covers `ChromeMessage` `no node`, `ChromeMessage` `node with given id ... not found`, unrelated chrome message, `Io`, and `NotFound` cases, `no_page_target_error` is a stable `cdp_unavailable`, the dispatch path's `browser_navigate_blocks_url_before_connecting` and `browser_navigate_unblocked_url_still_attempts_connect` confirm the policy gate runs before the websocket, `browser_text_label_is_browser_text_and_class_is_observe` and `browser_type_label_is_browser_type_and_class_is_mutate` confirm the audit / policy class split, `browser_text_does_not_record_chars_in_audit` confirms reads are never typed-into. CLI tests `browser_text_in_exec_without_cdp_returns_unsupported` and `browser_type_in_exec_without_cdp_returns_unsupported` lock the JSON surface. CLI test `browser_navigate_blocks_url_before_connecting_with_temp_policy` writes a temp `policy.toml` and runs the live binary against `ws://127.0.0.1:1`, asserting `blocked_url` without connecting. CLI test `browser_tabs_against_missing_chrome_returns_cdp_unavailable` asserts `cdp_unavailable` for an unreachable websocket. Local on this Mac: `cargo fmt --all -- --check && cargo build && cargo test --all-targets && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --all-features -- -D warnings` all pass. Live smoke: `./target/debug/computer-use --cdp --cdp-url ws://127.0.0.1:1 --policy <temp/policy.toml> browser navigate --url https://blocked.example.com/page` → `blocked_url` (websocket never opened). `./target/debug/computer-use --cdp browser tabs` → `cdp_unavailable`. Limitations / deviations: `browser_click` uses chromiumoxide's CDP `Input.dispatchMouseEvent` instead of a screen-space transform. Chrome owns the window, and any screen-space transform would require reading window bounds via the Accessibility API + display configuration + devicePixelRatio, then racing with Chrome's own input pipeline. The CDP click is reliable enough for an attach, and the deviation is documented in both the P10 design notes and the function-level doc comment in `src/browser/cdp.rs`. A future revision can return a JSON `tier: "cdp"` field if a caller needs to know. Linux + Windows paths compile and pass CI but were not live-smoke-tested on this Mac. chromiumoxide_cdp pulls ~60K lines of generated Rust code on every `--features cdp` build; CI compile time for the linux job is ~30 s. Windows argv lookup is documented as `unsupported` for now (Win32 bindings would add ~10 crates for one path — pass `--cdp-url`). The Windows MSVC linker needed `/STACK:8388608` (`.cargo/config.toml`) to keep the binary from stack-overflowing at startup under `cargo test --all-features`. CI run `39363819574` is green on rustfmt, ubuntu, macos, and windows. |
| P11 Vision and set-of-marks | DONE | `4c6ac07e3981b8c2e85cb3a9461591ee96ef4198` | macOS OCR: the previous row claimed the Vision path was live while the source was a stub returning empty `OcrResult` plus a fake `note`. The follow-up `fix(P11): real Vision OCR on macOS` rewrote `src/vision/ocr.rs` to call the real `objc2-vision` 0.3.2 surface: PNG-encode the RGBA capture, hand the bytes to `VNImageRequestHandler::initWithData_options:` with `VNRequestTextRecognitionLevel::Accurate` and `setUsesLanguageCorrection(true)`, run via `performRequests_error`, walk `VNRecognizedTextObservation::topCandidates(1)`, get `.string()` + `.confidence()`, read the normalized bounding box through `VNDetectedObjectObservation` (the only `unsafe` call, via `Retained::cast_unchecked`), and flip Vision's bottom-left origin to image pixels with the now-routinely available `vision::ocr::vision_box_to_image`. Errors map to `ExecutionFailed` with the NSError `localizedDescription`. Features added honestly to `objc2-foundation` (`NSData`, `NSDictionary`, `NSObject`) and `objc2-vision` (`VNRequest`, `VNTypes`, `objc2-core-foundation`); no transitive-only feature lies. Tests: a new macOS-only `vision_recognizes_drawn_text` (marked `#[ignore]`, run locally via `cargo test --release vision_recognizes_drawn_text -- --ignored --nocapture`) renders a 5x7 bitmap-font `HELLO` into a white RGBA buffer and asserts the public `recognize` entry point returns a block containing `HELLO` — passes on this Mac; the pre-existing `vision_rect_flips_origin`, `redaction_*`, and platform-`unsupported` tests stay green. Set-of-marks: tree indices on a window target, `oN` OCR indices otherwise. Secure-field redaction: blocks whose image rect falls inside a secure element's frame get dropped by `vision::ocr::redact_blocks`. The `Tree` action surfaces `"ocr_fallback_available": true` whenever the tree is empty and a window target was given so the agent can decide to call `ocr`. Platform errors: `unsupported` on Linux (no engine in 0.2.0); on Windows the P11 `unsupported` stub was replaced by a real `Windows.Media.Ocr.OcrEngine` call in P12b, so a missing recognizer language pack is `execution_failed` naming the optional feature, not `unsupported`; `permission_required` when TCC denies Screen Recording on macOS, `display_unavailable` when the screen is locked. `recognize_text` returns `unsupported` on the mock backend so dispatch tests still see a non-OK payload. Local on this Mac: `cargo fmt --all -- --check && cargo build && cargo test --all-targets && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --all-features -- -D warnings` all pass; the ignored Vision test passes too. CI on this commit (`gh run 39369937481`) is green for rustfmt/ubuntu/windows/macos. Live smoke: the host is screen-locked, so `./target/release/computer-use ocr --app Finder` returns the structured `display_unavailable` ("the screen is locked"). `cargo doctor` confirms Screen Recording IS attached to the binary; the locked-screen state is the documented path. Limitations at P11: end-to-end OCR still needs an unlocked display and Screen Recording; Linux OCR remains an explicit 0.2.0 gap; Windows Media OCR landed in P12b. P12b closed the last item here: `tree_for_marks` and the `secure_ocr_frames` screen-image remap are real on both platforms, through the shared `image_util::screen_rect_to_image`, and `secure_ocr_frames` now takes the screenshot map. |
| P12 Windows UIA and capture | DONE | `7964902ba14fac2de07d11f6c0a758edd203b05d` | **Part A** (`c48fd11`, follow-ups `bba238b`, `c5a08f3c53e343211dc2bf721def674b9f24d039`) lands native `windows` 0.62 + `uiautomation` 0.25.1, both target-gated. `src/backend/windows/apps.rs` drives `EnumWindows` + `GetWindowTextW` + `GetWindowThreadProcessId` + `QueryFullProcessImageNameW`; the PowerShell foreground script is gone. `src/backend/windows/uia.rs` uses the `uiautomation` control view walker for a canonical `UiNode` tree (`role = UIA control type`, `IsPassword` redact), element actions dispatch through `Invoke` / `Value` / `Text` / `ScrollItem` / `ExpandCollapse`, `source: "uia"`, the same `crate::element::gate` stale-element contract, the HWND resolved by pid + z-order (`--app pid:NNN`, never `GetForegroundWindow` when `--app`/`--window` is set), every UIA call inside `run_uia_with_timeout` (2 s, worker thread, `execution_failed("UI Automation call timed out after 2s")`), and `doctor` reading `accessibility` + best-effort `uiaccess`/`integrity` from `OpenProcessToken` / `GetTokenInformation` — no PowerShell. macOS / Linux untouched and green. **Part B** — feature commit `b65801dbdb37c278a22fb72e31c7b67eda834c69`, CI follow-ups `ca05d563ab5f0c0c2c1b8c6fa3c774a60409ab86` (test the shared rect conversion instead of allowing dead code), `42a0c0b95c03274230183eb772203445403f553f` (`as_chunks` for the BGRA swizzle), `7964902ba14fac2de07d11f6c0a758edd203b05d` (assert the real OCR engine rather than the P11 stub) — adds the three remaining pieces. **1. Per-window capture** — new `src/backend/windows/capture.rs`: `windows-capture` 2.0.1 pulls exactly one `Windows.Graphics.Capture` frame from the resolved HWND. `Settings::new(Window::from_raw_hwnd(...), WithoutCursor, WithoutBorder, Exclude, MinimumUpdateInterval::Default, DirtyRegion::Default, ColorFormat::Rgba8, sink)`, `start_free_threaded`, the first `FrameBuffer` is copied out (row pitch dropped), then the session is stopped and joined. The shared slot rides in `Settings::flags` because the callback is constructed on the capture thread. The pixel-to-screen map is `logical_* = window rect` with `scale = GetDpiForWindow(hwnd)/96`; see the **P12b correction to the capture map** in the design above for why it is the rect and not `frame/scale`, and why process DPI awareness is deliberately not switched mid-process. A 5 s budget bounds the wait — `Windows.Graphics.Capture` only pushes a frame when the window repaints — and on expiry the module crops the xcap image with `capture_source: "xcap"`, `occluded: true`, and a warning naming the graphics-capture error. A frame whose size disagrees with `rect * scale` is a warning, not an error, because the map absorbs it. **2. Media OCR** — `src/vision/ocr.rs` replaces the P11 `unsupported` stub with a real `Windows.Media.Ocr.OcrEngine` call: RGBA→BGRA8 premultiplied swizzle, `CryptographicBuffer::CreateFromByteArray` → `IBuffer` → `SoftwareBitmap::CreateCopyWithAlphaFromBuffer`, `OcrEngine::TryCreateFromUserProfileLanguages()`, `RecognizeAsync` + `join()`. One `OcrBlock` per `OcrLine` (union of its words' `BoundingRect`s); `OcrWord::BoundingRect` is already top-left image pixels, so unlike Vision there is no flip. Confidence stays 0 because the engine reports none. The pass runs on a worker that calls `RoInitialize(RO_INIT_MULTITHREADED)` itself (Media OCR refuses an uninitialized thread) with a 10 s `recv_timeout` budget; only `Send`-able values cross back, matching the P12a UIA worker. `note` carries the recognizer language. A missing language pack is `execution_failed` naming Settings → Apps → Optional features — never `unsupported`. **3. Activate fallback** — new `src/backend/windows/activate.rs`: a missing pattern or a failed pattern call is retried as tier `activate` = `SetForegroundWindow` + enigo under the same `foreground.lock`, verified with `GetForegroundWindow` rather than trusting the return value. The warning `foreground required` is attached exactly when that retry is what worked. `--allow-foreground` skips the lock; a busy lock names the holder pid; `--require-background` refuses. The retry is limited to `press` and `type` (see the P12b op table) because `set_value` must go through `ValuePattern` per the P3 value-not-keystrokes contract, `select_text` needs the `TextPattern` range, and `scroll_to` needs `ScrollItemPattern`. `focus_restored` reports what the verified restore actually did, via the new `element::activate_outcome`, instead of `outcome`'s unconditional `Some(true)`. **Two gaps found and fixed in-scope.** (a) P12a wrote `windows::element_action` but never added it to `impl_platform_backend!`, so every Windows element command still answered `unsupported` — P12b wires it, which is what makes the activate retry reachable, and the CLI test is renamed `element_commands_are_unsupported_off_macos_and_windows`. (b) `DesktopBackend::secure_ocr_frames` had no screenshot map, so platform frames (screen points) were compared against OCR blocks (image pixels): the macOS implementation returned the screen-point values unchanged and the Windows default was an empty list, meaning **no platform actually redacted a secure field**. P12b adds the map parameter, both backends convert through the shared `image_util::screen_rect_to_image`, and the P11 row's documented placeholder is closed. **Tests.** Pure math is tested on *every* runner, not only the Windows job: `image_util::tests` covers `window_placement`, `backing_scale`, `size_mismatch_warning`, the 150%-scale `image_to_screen`/`screen_to_image` round trip, and `screen_rect_to_image` (non-zero origin, downscale, letterbox pad, off-canvas, never-empty); `vision::ocr::tests` covers `ocr_box_to_image` (top-left origin preserved, rounding, negative-origin clamp, image clamp, never-empty, and the round trip back through `ScreenshotMap::to_screen`); `backend::windows::activate::tests` covers the whole fallback decision matrix plus the warning and the outcome; two new dispatch tests (`secure_ocr_frames_receives_the_map_of_the_ocr_capture`, `no_secure_frames_keeps_every_ocr_block`) back the redaction fix with `StubBackend::secure_frames` / `secure_frames_map` injection. `windows_media_ocr_runs_and_never_returns_unsupported` runs the real engine on the Windows runner against a blank image and treats `Unsupported` as a failure. **Local on this Mac:** `cargo fmt --all -- --check && cargo build && cargo test --all-targets && cargo clippy --all-targets -- -D warnings && cargo clippy --all-targets --all-features -- -D warnings` all pass (218 unit + 26 CLI tests). The local toolchain was at 1.91 while CI runs the current stable, so `x86_64-pc-windows-gnu` is additionally type-checked and clippy-linted locally (`cargo clippy --target x86_64-pc-windows-gnu --all-targets --all-features -- -D warnings`) after updating stable to 1.99.1 to match CI — that is how the `chunks_exact_to_as_chunks` lint was caught locally instead of on the runner. **Live smoke on this Mac:** not applicable to the Windows paths, and the macOS paths are unchanged — `doctor` reports `permissions_ok: no` (Accessibility, Post Event, Input Monitoring denied for Grok Bot; Screen Recording granted), `tree --app Finder` returns the structured `permission_required`, `ocr --app Finder` and `screenshot --app Finder` return the structured `display_unavailable` (the host screen is locked), `list-apps` returns real JSON. **CI** run `39380771653` is green on rustfmt, ubuntu, macos, and windows. **Limitations, stated plainly.** No Windows host was available, so **none of the Windows capture, OCR, or activate-fallback paths were executed live** — CI compiles them, runs their unit tests, and (for OCR only) actually invokes `OcrEngine` against a blank image, but a real `Windows.Graphics.Capture` frame from a live HWND, a real `SetForegroundWindow` + enigo click, and OCR over real window pixels are all unverified on hardware. The `x86_64-pc-windows-gnu` cross-check is a type/clippy check, not a run. The 5 s capture budget is a guess: `Windows.Graphics.Capture` frames are compositor-driven, so a window that legitimately stops repainting within 5 s of the request falls back to xcap rather than waiting. `windows-capture` 2.0.1 is edition 2024, i.e. Rust 1.85+, which the crate's 1.91 floor satisfies. `CaptureControl::stop` failures are surfaced, but a leaked capture worker would not be. Linux OCR remains an explicit 0.2.0 gap (P13). P13 is untouched. |
| P13 Linux AT-SPI, portal, X11 | DONE | `644fe4fa4b5721f883898c844478767765eaad84` part A, `0b02dfdbfab6c57d388fcfb5abc1a0fdbb52aef4` part B | **Part A** (`644fe4fa4b5721f883898c844478767765eaad84`) lands the Linux tree, actions, doctor, and `event` tier on X11 without `portal`. **Part B** (`0b02dfdbfab6c57d388fcfb5abc1a0fdbb52aef4`) adds `src/backend/linux/portal.rs` behind the `portal` feature: `ashpd` ScreenCast capture (prefer a window source, else the monitor containing the window and crop it), one frame pulled off the portal's PipeWire fd into RGBA, and `ashpd` RemoteDesktop for tier `event` on Wayland with an optional `reis` EIS handshake. `capture_source: portal` is wired into `capture_window`; a cancelled dialog is `permission_required` and never falls back, while a technical failure falls back to the xcap crop with a warning. Buttons use evdev codes and keys use X11 keysyms. `doctor` gained a `portal` field reporting compile-time capability. **Limitations, stated plainly:** (1) Linux `list_windows` / `list_apps` / `launch` / `activate` / `quit` are still `unsupported` — P13b's scope was capture and input — so `screenshot --app` cannot resolve a window target and **no CLI command reaches `capture_window` yet**; `capture_source: portal` therefore never appears in real output. Tier `event` with explicit `--x/--y` *is* reachable, since it needs no window enumeration. (2) `reis` 0.7 has no public API to create an `ei_device`, so the EIS handshake only reports availability and never dispatches relative motion; the portal's absolute motion carries every event. (3) `drag`, button hold, hold-key and free-text typing are `unsupported` on the portal tier rather than partially sent. (4) **CI-only verification:** `~/.grok-prompts/linux-check.sh` exited 2 on this machine (Docker daemon unavailable, and per instructions I did not start it), so ubuntu-latest CI was the only Linux compiler. It took four fix commits (`209362d`, `2323bf0`, `119eeae`, `0b02dfd`) for Linux-only API mismatches that macOS cannot see — `Request::response()` is synchronous in ashpd 0.13, `set_sources` wants `Option<BitFlags<SourceType>>`, `block_on` needs an owned future, and `OwnedFd -> UnixStream` is an infallible `From`. (5) No live Wayland session was available, so nothing was exercised against a real compositor or a real portal dialog; the portal math, source selection, error mapping and keysym table are unit-tested, and the rest is compile-verified only. New code: `src/backend/linux/{runtime,atspi,x11}.rs` and `src/backend/linux/mod.rs` rewires the dispatch. Process-wide current-thread tokio runtime (`runtime::block_on`) is created lazily on the CLI thread; `DesktopBackend` stays sync. AT-SPI: `AccessibilityConnection::new` + `tokio::time::timeout` (3 s), `find` an `ObjectRefOwned` matching `--app` / `--description`, walk the registry via `as_accessible_proxy`. Roles come from the AT-SPI enum (`Role::name()`). Redaction: **only** `Role::PasswordText` redacts the value (the older `State::Sensitive && !State::Enabled` rule was dropped because AT-SPI `Sensitive` means 'user-actionable', not 'secret'); the renderer keeps the `node_is_secure` contract; `tree::is_secure` was extended to recognise both `AXSecureTextField` and `password text`. Frames: `Component::get_extents(Screen)` per node when the `Component` interface is present — kept as `None` in P13a because the round-trip-per-node cost does not pay back without part B's per-window background. Element actions: `press` -> `Action::do_action(0)`; `set_value` -> `Value::set_current_value` (or `EditableText::set_text_contents`); `select_text` -> `Text::set_selection`; `scroll_to` -> `Component::scroll_to`. Missing interface -> `execution_failed`. Same `crate::element::gate` stale-element revalidation; part A wires the full path against the AT-SPI bus (no part-B dependency for press/focus/set_value/select_text/scroll_to/type). X11 XTest: `x11rb` 0.13 with the `xtest` feature; `x11rb::protocol::xtest::fake_input` translates the agent's pointer-and-keyboard requests; `tier: "x11"` in the response; documented as not per-window background. Tier `event` requires `$DISPLAY` set and `$WAYLAND_DISPLAY` unset (the `--x11` CLI flag bypasses the Wayland check). Wayland without `portal` -> `tier: unsupported`. Tier `activate` stays enigo on X11. Linux `doctor` tests AT-SPI bus connect (returns `execution_failed("AT-SPI bus unavailable…")` on a missing daemon, which is what the agent sees and what the user fixes with `apt install at-spi2-core at-spi2-atk`), plus `portal` feature-compiled status, `$DISPLAY`/`$WAYLAND_DISPLAY`, and the per-action `note`. `tier event` is reachable on plain X11, `unsupported` on Wayland until P13b adds XDG Desktop Portal RemoteDesktop. New `--x11` CLI flag flows through `DoctorOptions.x11` (hidden on non-Linux) so a session with both `DISPLAY` and `WAYLAND_DISPLAY` can force X11. CI adds `libatspi2.0-dev` to the Linux apt set; `libdbus-1-dev` was already there. CI fixes `bcac65a` (Linux-only build errors: `RustConnection`, `Connection::setup`, `Cookie::check`, `Access..., `2f07b3d` (tree is real, drop the unsupported assertion), `b857554` (element actions via AT-SPI, password redaction, xtest wiring), `d58afd7` / `5b7f65c` / `9904725` / `9119f8d` / `0f45360` / `c187c38` / `69dbd81` / `0b1a993` / `7530658` / `644fe4f` (Linux clippy / `Result` layers / `RenderedNode` lifetime / `into_accessible_proxy` / `for<'a>` test type / `clone` on Copy / Linux password role / env-independent x11 test / non-password role unit test). Element actions at runtime end of part A: `press` -> `Action::do_action`; `focus` -> `Component::grab_focus`; `set_value` -> `EditableText::set_text_contents` or `Value::set_current_value`; `select_text` -> `Text::set_selection` / `add_selection`; `scroll_to` -> `Component::scroll_to`; `type` -> `EditableText::set_text_contents`; `perform` -> `Action::do_action(name)`. Secure `Role::PasswordText` nodes refuse `set_value`, `select_text`, and `type` with `policy_denied`. The Linux `element_action` dispatcher returns `execution_failed` for a missing interface, never an invented click. Limitations: Linux CI runs without an at-spi2 bus, so the `via` chain is exercised by the unit tests; live Linux behaviour depends on `at-spi2-core at-spi2-atk` being installed (the doctor reports `atspi_bus` and exits with the fix-it text on failure).
| P14 Isolation | DONE | `0cd1a7a91a3c4fcc07aef4486a6384bc178bd7c2`, CI fix `096e3956dc67b6b2ec456f556e475757f7bab4aa` | New `src/isolation.rs` (pure, unit-tested on every OS) plus `docs/ISOLATION.md`. **`--display <id>`** is a global flag and a `display` context key: it resolves a monitor from `list_displays`, and `gate_display` (in `protocol.rs`, run after `remap_coordinates` so `--coord image` is already a screen point) rejects every out-of-monitor point and every `--region`/`--window` that does not fit. The filtered capture reuses the shared `backend::capture` path with the monitor rectangle, so the map origin is the monitor origin and no per-platform code was needed — that is what makes it work on all three OS from one implementation. A window capture is **not** redirected onto another monitor: a window outside the filter is `invalid_arguments`. **Syntax is checked before the backend is touched**, so a garbage value is `invalid_arguments` even on a machine with no enumerable display. **`displays` and `isolation` do not consume the filter** — they are how you find the ids. **`--display virtual`** is Linux-only: it prints the recipe and, with `--exec`, execs the command with `DISPLAY=:99`. Xvfb is never started implicitly; `starts_server: false` is in the JSON and in a unit test. **The macOS `virtual-display` feature probes and stops.** It `dlopen`s CoreGraphics explicitly (RTLD_DEFAULT cannot see a framework the process has not loaded, which made an early version report a false negative) and `dlsym`s the two private symbols. It never calls them: creating a display needs an Objective-C `CGVirtualDisplayDescriptor` configured through private class setup, so this build ships the probe plus an honest `unsupported`/`experimental` and **no create path at all**. `isolation` claims `experimental` only when both symbols really resolved; on this macOS 27.0.1 machine they are absent, so a live run honestly reports `unsupported`. **`isolation`** prints `{platform, display_filter, xvfb, cg_virtual_display, vm, infeasible{5 keys}}`, read-only, identical on every OS, with a snapshot test asserting the five infeasible keys so the command and `docs/ISOLATION.md` cannot drift. **Limitations, stated plainly:** (1) `--display` is a filter, not a sandbox — it changes what this binary captures and addresses, not what the user sees. (2) The Xvfb path was **not exercised live**: it is Linux-only and `~/.grok-prompts/linux-check.sh` exited 2 here (Docker daemon unavailable, not started per instructions), so ubuntu-latest CI compiled and unit-tested it but no Xvfb session was ever driven. The `--exec` path itself is unit-tested for argument splitting and covered end-to-end by two Linux CI tests. (3) `--display` cannot filter `list_windows`, so on Linux, where P13b left those actions `unsupported`, `--display` cannot reach the portal capture path any more than P13b could. (4) Live smoke on this Mac was limited by permissions and a locked screen: `doctor` reports Accessibility off (post-events and input-monitoring also off) and `displays` returned an empty list, so `screenshot` returns `display_unavailable: the screen is locked`. `--display 1 screenshot` returned `invalid_arguments` ("no display is reported by this platform") because the gate fires before capture — correct, and no capture was attempted. (5) `--exec` is deliberately not `global`, so it only applies before a subcommand; that is the whole shape of the recipe (`computer-use --display virtual --exec -- computer-use screenshot`). CI needed one follow-up, `096e395`: two of my own CLI tests assumed the runner could enumerate displays, which ubuntu-latest cannot — the assertion, not the feature, was wrong. |
| P15 Docs and 0.2.0 release | DONE | `c9547474a4808d7fd537f72724df71a14f36475c` | Bumped to `version = "0.2.0"`, `rust-version = "1.91"`. The crate `include` list now ships `README.md`, `LICENSE-*`, `src/`, `docs/{ROADMAP,ISOLATION,PACKAGING,SKYLIGHT}.md` and `docs/policy.example.toml`, `packaging/macos/{Info,entitlements}.plist`, and `scripts/package-macos-app.sh`. README rewritten from the current CLI surface (commands, default max-width, policy path, feature flags, permissions per OS, tiers, JSON actions, error codes, library, coordinates, isolation). New `docs/CLI.md` is the cli-reference generated from `--help` and the protocol enums; `tests/cli_doc_matches_help.rs` parses the Markdown and asserts every action name and CLI subcommand is reachable on a fresh build. The doc notes that `smart-computer-use/references/cli-reference.md` is regenerated in a separate repo. CHANGELOG `0.2.0` follows keep-a-changelog with every P1–P14 highlight, a `Known limitations` section (Accessibility off on the dev Mac, Windows/Linux CI-only, CDP click via `Input.dispatchMouseEvent`, Windows CDP port discovery unsupported, Linux OCR absent, private-API features experimental, unsigned app bundle unless secrets), and an empty `Cut from 0.2.0` (no parity row was dropped; two rows stay PARTIAL on honest reasons — scroll-into-view and select-text can return `execution_failed` when the platform interface is not advertised). The parity table is unchanged; a `vs Codex` summary below it lists where we exceed, match, and fall short with no marketing claims. `cargo publish --dry-run` packed 107 files including `README.md`, `docs/ROADMAP.md`, and `scripts/package-macos-app.sh`. The release workflow gained `--features portal` and the PipeWire / clang / libclang-dev apt packages for the Linux job. `scripts/package-macos-app.sh` on the release binary produced `Computer Use.app` with bundle id `ai.lindoelio.computer-use`, `CFBundleShortVersionString = 0.2.0`, `CFBundleVersion = 0.2.0`, ad-hoc signed (Developer ID secrets are not configured). CI green on all four jobs (`rustfmt`, `test (macos-latest)`, `test (ubuntu-latest)`, `test (windows-latest)`); `cargo fmt --all -- --check`, `cargo build`, `cargo test --all-targets`, `cargo clippy --all-targets -- -D warnings`, and `cargo clippy --all-targets --all-features -- -D warnings` are clean locally; `~/.grok-prompts/linux-check.sh` exited 2 (Docker daemon unavailable), so Linux gating was CI-only. **Tag, release, and `cargo publish` are left to the user per the task contract.** |

## Suggested implementation order inside a phase
Each phase is one commit unless the note below says otherwise. Do not start P3 before P2 is on `main`. P4 can follow P2 in parallel with P3 only if the screenshot command does not need element indices. P5 depends on P2 and P4. P6 should land before P7 so the event marker and the tap share one constant. P12 and P13 depend on P2's `tree.rs` and P6's policy, and they can proceed in parallel with each other. P10 and P11 depend on P4's map. P14 and P15 are last.

P2 is the largest phase. Split it only if review requires it: first the pure `tree.rs` and cache, then `ax.rs`. Both halves stay inside P2 in this table. The second half's commit message still says `P2`.
