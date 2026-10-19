# computer-use 0.2.0 roadmap
Status: P4 is done. Resume from the Progress table.
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
| Linux | `src/backend/linux.rs` | Frontmost window from `xdotool` when installed. Accessibility returns `unsupported`. Wayland is not a target. |
| Windows | `src/backend/windows.rs` | Foreground title and process name from a short PowerShell `GetForegroundWindow` script. No UI Automation tree. |
| Images | `src/image_util.rs` | PNG or JPEG (quality 80). `--max-width` is opt-in and uses Lanczos3. No default cap. |
| Errors | `src/error.rs` | `invalid_arguments`, `execution_failed`, `io_error`, `unsupported`. |
| Tests | `src/**` unit tests, `tests/cli.rs` | No test moves the pointer or captures the screen. `tests/cli.rs` asserts version `0.1.0`. |
| CI | `.github/workflows/ci.yml` | rustfmt, clippy `-D warnings`, `cargo test --all-targets` on Ubuntu, macOS, and Windows. Linux apt packages: `pkg-config libxdo-dev libx11-dev libxcb1-dev libxrandr-dev libdbus-1-dev libxkbcommon-dev`. |
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
| `toml` | 0.9 | P6 | `policy.toml`. Fall back to 0.8 if 0.9's `from_str` signature moved. |
| `chromiumoxide` | 0.9.1 | P10 | Feature `cdp` only. `Browser::connect`, page `goto`, `evaluate`, CSS query. MSRV 1.85. |
| `tokio` | 1 | P10 | Feature `cdp` and the Linux async backends. Current-thread runtime, `block_on` from the CLI thread only. |
| `uiautomation` | 0.25.1 | P12 | Features `control`, `pattern`. `UIAutomation`, invoke / value / scroll / text / expand-collapse. |
| `windows` | 0.61 | P12 | Unify with whatever `windows-capture` 2.0.1 and `uiautomation` 0.25.1 already pull. Features: `Win32_UI_Accessibility`, `Graphics_Capture`, `Media_Ocr`, `Win32_UI_WindowsAndMessaging`, `Win32_Graphics_Gdi`. |
| `windows-capture` | 2.0.1 | P12 | One frame from an HWND. Do not capture the monitor when the caller named a window. |
| `atspi` | 0.30.0 | P13 | Features `connection`, `proxies`, `tokio`. Action, Component, Text, Value. MSRV 1.77.2. |
| `ashpd` | 0.13.13 | P13 | Features `screencast`, `remote_desktop`. MSRV 1.87. |
| `pipewire` | 0.9 | P13 | Consume the ScreenCast stream. Native library `libpipewire-0.3`. |
| `reis` | 0.7.1 | P13 | libei client when the compositor offers an EIS socket. The crate is incomplete. Feature `tokio`. |
| `x11rb` | 0.14.0 | P13 | Feature `xtest`. `fake_input` for the X11 fallback. |
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
Added as clap global args, and as sibling fields of `action` in JSON. `parse_request` already removes `id` before `Action` is decoded. Extend that strip list so these keys never reach the action enum: `app`, `window`, `element`, `diff`, `full`, `return_state`, `include_screenshot`, `coord`, `session`, `tier`, `adapter`, `display`, `require_visible`.

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
Auto order is `ax`, then `event`, then `activate`. `synthetic` runs only when requested.

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
| `click` | `press` or coordinate click. `button` left/right/middle. |
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
| `zoom` | `screenshot` of `region: [x1, y1, x2, y2]` in image pixels |
| `cursor_position` | read-only `cursor_position` |
| `left_mouse_down`, `left_mouse_up` | `unsupported` until P8 |

If a published patch renames a field, accept the alias in the adapter and add a fixture. Do not guess a second schema in the dispatcher.

`drag`, `triple_click`, `left_click_drag`, `hold_key`, and the OpenAI buttons `back` and `forward` return `unsupported` and name P8. OpenAI `wheel` is `middle`. OpenAI `wait` with no duration sleeps 0 ms, because that schema has no duration. Anthropic `zoom` stays in image pixels on the translated action. Dispatch remaps it with the stored map. The far edge of a region may sit on the image width or height. A point may not.

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

Resolution order: built-in deny, then admin policy, then user policy. Deny wins over allow. Admin policy can only add denies and raise confirmation. It cannot allow an app the built-in list refuses. `default = "deny"` means an app not listed is `policy_denied`. Ship the example file as `docs/policy.example.toml` in P6 with `default = "allow"` commented, so a missing file does not lock the user out. A missing file uses `default = "allow"` plus the built-in deny list.

Built-in deny, matched on bundle id and on the canonical executable path:

- Terminals: `com.apple.Terminal`, `com.googlecode.iterm2`, `dev.warp.Warp-Stable`, `org.alacritty`, `net.kovidgoyal.kitty`, `com.mitchellh.ghostty`, `com.github.wez.wezterm`, Windows Terminal, `powershell.exe`, `cmd.exe`, `wt.exe`, `gnome-terminal`, `konsole`, `xterm`, `wezterm`, `kitty`, `alacritty`.
- Password managers: 1Password, Bitwarden, KeePassXC, Dashlane, Enpass.
- `com.apple.keychainaccess`.
- System Settings (`com.apple.systempreferences`) when the window title or AX identifier contains `Privacy` or `Security`. Other System Settings windows stay reachable unless the user denies the bundle.

Identity is the canonical executable path from `NSRunningApplication.executableURL` (macOS), `QueryFullProcessImageName` (Windows), or `/proc/<pid>/exe` (Linux), after symlink resolution. A bundle id match with a path that does not match an `allow.path` is `policy_denied`. Comparison is on the canonical path, not the argv the process was started with.

### Confirm hook
Stdin is one JSON object: `action`, `app.bundle_id`, `app.path`, `pid`, `window_id`, `element`, `reason`. Stdout is ignored. Exit 0 allows. Any other exit or a timeout yields `confirm_denied`. Sensitive set:

- Always `policy_denied`, hook or not: `set_value` or `type` into a secure text field. Key events aimed at a secure field.
- Confirm: `quit`, `clipboard_set`, click or `AXPress` whose title matches `(?i)\b(delete|erase|pay|purchase|send|submit)\b`, key combos `cmd+q` and `alt+f4`.
- No hook configured: those confirmed actions return `confirm_required` and do not run.

The hook is `--confirm-cmd`, or `COMPUTER_USE_CONFIRM_CMD`, or `confirm.command` in the policy. The flag wins.

### Audit line
One JSON object per line, append-only:

```json
{"ts":"2026-10-18T12:00:00Z","session":"9","action":"press","bundle_id":"com.apple.Notes","pid":412,"window_id":88,"element":12,"code":"ok","tier":"ax"}
```

Never write `text`, `value`, clipboard bytes, or URLs that were blocked. A refusal is audited with the error code.

### Kill switch
On the first action of a process, if `pause_on_user_input` is true, install a listen-only `CGEventTap` (`kCGSessionEventTap`, `kCGEventTapOptionListenOnly`) for key and mouse events. Create synthetic events with `CGEventSourceSetUserData(source, 0x00C0_5EC0_0DE1)` and set `kCGEventSourceUserData` (field 43) to the same marker. The tap ignores events carrying that marker. Any other event sets `paused`. Later actions return `paused` until `computer-use resume`. If the tap cannot be created, `doctor` says so and actions still run, with `warnings: ["user-input pause unavailable"]` on each response.

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
- Linux and Windows `doctor` return `ok: true` with `accessibility: null`, `screen_capture: null`, and `note` explaining that the probe lands in P12 or P13.

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
- `doctor` on Linux returns the P13 note. Windows returns the P12 note. `--request` does not prompt on those platforms.

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
- CLI `tree` on Linux and Windows returns `unsupported`.
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

`type` prefers `set_value` when `--element` is set and `AXValue` is settable (`AXUIElementIsAttributeSettable`). That success uses `tier: "ax"` and `via: "set_value"`. Otherwise it focuses the element and types with enigo, and the success uses `tier: "enigo"` and `via: "enigo"`. Without `--element`, `type` still uses enigo on the focused control and does not report a tier. That split is temporary. P7 routes the enigo path through tiers.

Frame revalidation allows 2 points on each edge. Past that, or when the role or identifier changed, the result is `stale_element` and the action is not performed. The fresh tree is a sibling `tree` field. Element actions update `observed` in the session cache and do not replace the `tree --diff` baseline. `scroll-to-visible` is an alias of `scroll_to`. Secure fields (`AXSecureTextField`, or a subrole containing `Secure`) refuse `set_value`, `select_text`, and `type --element` with `policy_denied`. The value is never read. The built-in deny list runs before any accessibility call on the target. Linux and Windows return `unsupported`.

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

Sheets are read from Accessibility and composited above the parent. Accessibility off, or a sheet with no matching `SCWindow`, skips the sheet and can add a warning. The parent capture still succeeds. Linux and Windows have no window list yet, so `screenshot --app` returns `unsupported` there until P13 and P12. A crop helper exists for when bounds are known.

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
- Each fixture in `tests/fixtures/openai/*.json` and `tests/fixtures/anthropic/*.json` translates to the expected `Action`. Unsupported actions expect `unsupported` and the name `P8`.
- Image coordinate (100, 50) on a stored map becomes the expected screen point: origin `(120, 40)`, logical `800×500`, content `1440×900`, pad `0` rounds to `(176, 68)`.
- Batch stops on the second action when the stub fails the first and `stop_on_error` is true (`stopped_at` is 0 and the second click is not attempted). With `stop_on_error` false the stub records both attempts.
- `return_state` on the stub includes the numbered tree from `backend.tree` (the same accessibility query list `inspect_accessibility` records). That diff becomes the new `tree --diff` baseline. Without `return_state` the baseline stays put.
- `include_screenshot` failure keeps the action result, sets `state.screenshot` to null, and adds a warning.
- `display_unavailable` is a pure function of lock, sleep, and display counts. The unit test uses fake inputs and does not call Core Graphics.
- `session prune` on a temp directory removes empty and stale sessions and keeps a live owner's content.

### Implementation notes
Post-action `tree` is `TreeReport.full_text`. The `tree` command's `tree` field is still the chosen full-or-diff text. `cursor_position` is a small read-only action because the backend already exposes the pointer. Drag, triple-click, mouse-button hold, and `hold_key` stay `unsupported` until P8 rather than pretending to work. OpenAI `wait` without a duration is 0 ms so a fixture does not sleep. Batch applies `return_state` once at the end. A step's own `return_state` is ignored so the batch does not snapshot between steps.

## P6 — Safety
### Design
Extend `src/policy.rs` to load TOML. Add `src/audit.rs`. Wire `policy::decide(target, action) -> Allow | Deny | Confirm` at the start of `dispatch_inner`, before the backend call. `wait`, `doctor`, `displays`, `list_apps`, and `list_windows` are always allowed. `tree` and `screenshot` of a denied app are denied too. Reading a password manager is not a feature.

Admin file is read first and merged. A user `allow` cannot override an admin `deny` or a built-in deny. Parse errors in the user file are `invalid_arguments` and name the line. Parse errors in the admin file are `policy_denied` with the path, so a broken admin file fails closed.

Blocked URLs: when the target bundle is a browser (Safari, Chrome, Chromium, Edge, Brave, Arc, Firefox) read `AXURL` or the window title's URL. Match the user and admin `blocked_urls` as globs (`*` only). A match returns `blocked_url` and does not press. CDP navigation in P10 uses the same function.

Confirm hook: spawn the command with the JSON document on stdin, `run_timeout` at `timeout_secs` (default 30, cap 120). See [Confirm hook](#confirm-hook).

Audit every allowed, denied, and failed action. Open the file with append and `O_NOFOLLOW` where the OS supports it.

Kill switch: implement the tap in `src/backend/macos/input.rs` even though the rest of that file lands in P7. The tap is listen-only. `computer-use resume` clears the atomic flag. `computer-use pause` sets it.

Windows and Linux honor the same TOML. Path matching uses their identity APIs. The tap is macOS-only in this phase. Other platforms set `warnings: ["user-input pause unavailable"]` until P12 and P13.

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
`unsupported` for `synthetic` without the feature. `execution_failed` when `CGEventPostToPid` returns an error or when `cursor_moved` is true and `--require-background` is set (the flag treats cursor motion as failure). Lock conflict on `activate`.

### Tests
- Tier selection table with a stub that fails `ax` and records which tier ran.
- User-data marker constant is the one the tap compares.
- Window-local point math: screen (200, 150) and origin (100, 100) produce (100, 50).
- Foreground lock: a second stub call with `activate` fails while the lock file is held.
- Overlay protocol parser accepts a JSON line and rejects a binary blob. Do not open an `NSWindow` in unit tests.

## P8 — More pointer and keyboard actions, clipboard, bundle
### Design
Extend `src/keys.rs` and enigo / `CGEvent` posting.

- `drag`: `points: [{x,y}, ...]` at least 2. Down at the first, move through the middle, up at the last. Tier `event` uses mouse-dragged events posted to the pid. Tier `activate` uses enigo button down, move, up.
- `mouse_down` and `mouse_up` share a button. A down without a later up in the same process is released on `Drop` of the input session.
- Click `--count 3` is a triple click. `--button right|middle` already exists for coordinates. Extend it to `press`.
- `hold_key`: press modifiers and key, sleep `--ms`, release. Cap the hold at 30 seconds.
- xdotool names: accept `super`, `ctrl`, `alt`, `shift`, `Return`, `BackSpace`, `Delete`, `Escape`, `Tab`, `space`, `Up`, `Down`, `Left`, `Right`, `F1`–`F24`, `plus`. Map them through `canonical_key`. `super` is `Modifier::Meta`, which is already the platform meta key.
- Clipboard: `arboard` 3.7.0. `clipboard_get` returns text, or `kind: "image"` plus a PNG path. `clipboard_set` is a confirmed action. Empty text is `invalid_arguments`. Cap stored text in the audit by omitting it.
- `zoom` is `screenshot` of a region. The Anthropic adapter already points at it.
- `doctor` grows `input_monitor` when the event tap from P6 was created during this process. The JSON gains `bundle_id` of the running binary so the user can see whether TCC is attached to a stable id.

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
`confirm_required` on clipboard set. `invalid_arguments` on a one-point drag, a hold over 30s, or an unknown keysym. `permission_required` when arboard reports the macOS pasteboard permission error.

### Tests
- Keysym table: `super+c`, `Return`, `ctrl+alt+Delete`, `plus`.
- Drag rejects fewer than 2 points and records N moves on the stub.
- Clipboard stub does not touch the real pasteboard. The trait method is stubbed.
- `package-macos-app.sh` is invoked in CI only as `bash -n` until P15 builds the zip. P15 runs the script on the release job.

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

Attach only. Do not spawn a new Chrome. Discover an existing port by reading the target pid's command line for `--remote-debugging-port=N` (`/proc` on Linux, `sysctl` KERN_PROCARGS2 on macOS, `Win32` command-line query on Windows). Otherwise require `--cdp-url ws://127.0.0.1:9222`. The command also requires `--cdp` so a binary built with the feature still does nothing until the flag is present.

Operations: list targets, activate a target, `Page.navigate`, CSS `querySelector`, click by selector (element box center, then the normal click path in image or screen space), `innerText` of a selector, and `Runtime.evaluate` only when `--eval` is also set. `evaluate` is off by default because it is an arbitrary script channel.

Navigation and the current URL go through `policy::blocked_url` before `Page.navigate`. A blocked URL never reaches the socket.

### CLI
```
computer-use browser tabs --cdp --app "Google Chrome"
computer-use browser navigate --cdp --url https://example.com
computer-use browser click --cdp --selector "button.save"
computer-use browser text --cdp --selector "h1"
```

### JSON
```json
{"action":"browser_tabs","cdp":true,"app":"Google Chrome"}
{"action":"browser_navigate","cdp":true,"url":"https://example.com"}
{"action":"browser_click","cdp":true,"selector":"button.save"}
{"action":"browser_text","cdp":true,"selector":"h1"}
```

### Failure modes
`unsupported` when the feature is off. `cdp_unavailable` when no port and no `--cdp-url`. `blocked_url` before navigate. `element_not_found` when the selector matches nothing. `policy_denied` when the browser bundle is denied.

### Tests
- Adapter from a fixture WebSocket is not required. Test URL blocking, command-line port parsing, and the "feature off" error with `#[cfg(not(feature = "cdp"))]`.
- With the feature, a unit test builds the request types. Do not launch Chrome in CI.
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
`permission_required` when the screenshot cannot be taken. `unsupported` on Linux and on Windows before P12. Empty `blocks` is success.

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
- Set a 2s timeout on the automation call where the crate allows it. A hung target returns `execution_failed` for that call.
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

Per-window capture: `windows-capture` 2.0.1 from the HWND (`Windows.Graphics.Capture`). One frame. Map pixels to screen points with the window rect and the DPI scale from `GetDpiForWindow`. Fallback: crop the xcap image, with `capture_source: "xcap"`.

Background: UIA patterns do not need foreground. That is the Windows equivalent of tier `ax`. If a pattern is missing, tier `activate` uses `SetForegroundWindow` and enigo, under the same `foreground.lock`. Codex stays foreground-only with an allowlist. This phase is past that, and the policy allowlist from P6 still applies. Document that some Win32 apps ignore Invoke unless they are foreground. The warning `foreground required` is returned when Invoke fails once and the retry with `activate` is what worked.

`doctor` on Windows reports whether `UIAutomation::new` succeeded.

### CLI
The same commands as macOS. `list-apps`, `list-windows`, `tree`, `press`, `set-value`, `screenshot --window` work on Windows after this phase. `menu`, `dock`, `spaces`, and `apple-event` stay `unsupported`.

### JSON
Same action names. `source` is `"uia"`. `doctor.accessibility` is true when UIA initializes.

### Failure modes
`app_not_found`, `window_not_found`, `permission_required` is rare on Windows. A UIAccess or integrity mismatch is `execution_failed` with the HRESULT. `policy_denied` uses the exe path.

### Tests
- A fake tree of control types renders with indices and redacts `IsPassword`.
- Diff reuse: the macOS fixtures and a Windows fixture share `tree.rs` tests.
- Dispatcher test: a requested pid is the one passed to the stub. There is no foreground fallback in the stub when `app` is set.
- CI compile on `windows-latest` with `--all-features`. Live UIA is not run on the runner.

## P13 — Linux AT-SPI, portals, and X11
### Design
`DesktopBackend` stays sync. Linux methods `block_on` a process-wide current-thread runtime created on first use from the CLI thread.

AT-SPI (`atspi` 0.30.0), always compiled on Linux:

- `AccessibilityConnection::new`.
- Find the application on the desktop by pid.
- Walk children. Render with `tree.rs`. Roles are AT-SPI roles.
- `Action::do_action` for press. `Value::set_current_value` when the interface exists. `Text` for `set_value` and caret selection. `Component::get_extents(CoordType::Screen)` for frames. `Component::scroll_to` or the scroll action for scroll-into-view.
- Secure text: the `Text` interface state `SENSITIVE` combined with role `PASSWORD_TEXT` is redacted.

Screen capture, feature `portal`:

- `ashpd` 0.13.13 `ScreenCast`. Start a session, prefer a window source when the portal offers one, else the monitor that contains the window, and crop.
- Open the PipeWire remote with `pipewire` 0.9 and pull one video frame into RGBA.
- The portal will show a system dialog the first time. Document that the agent cannot click that dialog for itself. `permission_required` is the error when the user cancels.

Input:

- Tier `ax` is the AT-SPI action.
- Tier `event` on Wayland is `ashpd` `RemoteDesktop` (pointer and keyboard) when the session is granted. If the portal returns an EIS socket, a `reis` 0.7 client may send relative motion. `reis` is incomplete: a failed handshake falls through and does not panic.
- Tier `event` on X11 (`DISPLAY` set and `WAYLAND_DISPLAY` unset, or `--x11`) uses `x11rb` 0.14 feature `xtest`: `xtest::fake_input` for motion, button, and key. This does not include a background-per-window path. Say so in the response with `tier: "x11"`.
- Tier `activate` remains enigo, which already uses libxdo on X11. On Wayland, enigo is not a supported tier. Return `unsupported` rather than sending the event nowhere.

`doctor` reports whether the AT-SPI bus connected, whether `WAYLAND_DISPLAY` or `DISPLAY` is set, and whether the `portal` feature is compiled.

X11 fallback capture stays xcap, which already uses XCB.

### CLI
Same commands. `computer-use doctor` on Linux fills `accessibility` from the bus probe. `--x11` forces the XTest path.

### JSON
`source` is `"atspi"`. Capture `capture_source` is `"portal"` or `"xcap"`.

### Failure modes
`permission_required` when the portal dialog is denied. `unsupported` for Wayland coordinate input without `portal` or without a granted RemoteDesktop session. `execution_failed` when the AT-SPI bus is absent (install `at-spi2-core`). `cdp` is independent of this phase.

### Tests
- Role and password redaction on fixture AT-SPI nodes through `tree.rs`.
- XTest request encoding is tested by inspecting the `x11rb` request bytes built for a motion, without a display. Skip the test if the feature flag layout makes that unreachable, and instead test the coordinate conversion into the request struct.
- Portal is not contacted in CI. `cargo check --features portal` on Ubuntu with `libpipewire-0.3-dev` is the compile check.
- Default `cargo test` on Ubuntu still passes without PipeWire.

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
- Release workflow: the macOS job runs `scripts/package-macos-app.sh`, uploads the zip next to the existing tarball, and runs the optional sign/notarize step when secrets are present. Linux release builds with `--features portal`. Windows release builds with the default target dependencies (UIA and `windows-capture` are target deps, not features).
- Confirm `cargo publish --dry-run` includes the README and this roadmap.
- Update the parity table statuses in this file to match what merged. A row that did not land stays `TODO` and is listed under "Cut from 0.2.0" in the changelog.

### Tests
The full CI matrix, plus `bash -n scripts/package-macos-app.sh`, plus a dry-run package on `macos-latest` that unzips and checks `Contents/Info.plist` contains `ai.lindoelio.computer-use`.

---

## Parity checklist
`Codex` describes OpenAI Codex Computer Use as publicly documented and as described by the reverse-engineering notes this plan is based on. `Ours (target)` is the 0.2.0 goal. `Status` is this repository today.

| Codex CU feature | Codex | Ours (target) | Status | Notes |
| --- | --- | --- | --- | --- |
| Key-window AX tree as compact numbered text on each observation | Yes | Yes, `tree` and `get-state` | DONE | P2. Numbered text from the native AX tree. osascript remains the fallback and sets `source=osascript`. Linux and Windows `tree` returns `unsupported`. |
| Window screenshot with the tree | Yes | Yes, target window only | DONE | P4 on macOS captures the target window with ScreenCaptureKit (`capture_source: sck`). Occluded windows use a desktop-independent filter. Other ScreenCaptureKit failures crop the xcap desktop image and set `capture_source: xcap` and `occluded: true`. Attached sheets are composited when Accessibility can see them. Linux and Windows `screenshot --app` returns `unsupported` until P13 and P12. P5 `include_screenshot` attaches that capture after a successful mutating action. A capture failure keeps the action result, sets `state.screenshot` to null, and adds a warning. |
| `element_index` targeting | Yes | `--element` / `element` | DONE | P3. `press`, `focus`, `set-value`, `select-text`, `perform-action`, `scroll-to`, and `click --element` use the index. Linux and Windows return `unsupported` until P13 and P12. |
| `x`/`y` fallback when no index | Yes | Hit-test, then event tier | PARTIAL | Coordinate click and move work through enigo and move the cursor. P3 hit-tests on macOS (`AXUIElementCopyElementAtPosition`) and reports `hit.role` and `hit.title`. A hit-test error is a warning and the click still runs. P7 adds the event tier. |
| Ids bound to pid + window + revision | Yes | `BoundElement` in the session cache | DONE | P2 stores `BoundElement` and the structural revision. P3 acts on the index after revalidation. |
| Re-fetch and validate before acting | Yes | Yes, mismatch is `stale_element` | DONE | P3 walks the cached child-index path and returns `stale_element` when the role, identifier, or frame (beyond 2 points) changed, or when the cached pid, window, or revision header does not match. The fresh tree is in the body. Nothing is performed. Element actions update `observed`. The `tree --diff` baseline stays put unless `return_state` is set. |
| Scroll into view before the action | Yes | `AXScrollToVisible` or platform equivalent | PARTIAL | P3 on macOS calls `AXScrollToVisible` when the element frame is outside the window and the action is supported. Failure is a warning unless `--require-visible`. Windows and Linux remain TODO for P12 and P13. |
| `AXManualAccessibility` and `AXEnhancedUserInterface` for Electron | Yes | Set on attach, restore on teardown | DONE | P2 sets both on first attach and restores them from `session end` and process exit. A crash, or a caller that never calls `shutdown_sessions`, can leave the flag set until the target app quits. A later `session end` restores from the cache and does not write an attribute this process did not change. |
| Content hosted in another process | Yes | Walk child pids, cap 3 helpers | DONE | P2. Foreign child pids are walked once, then a parent-pid scan. Cap is 3. |
| Set text through the value, not keystrokes | Yes | `set_value` via `AXValue` | DONE | P3 sets `AXValue` for `set_value` and for `type --element` when the attribute is settable. A non-settable element is focused and typed with enigo (`tier: enigo`). `type` without `--element` stays enigo until P7. |
| `set_value` | Yes | `set_value` | DONE | P3 on macOS via `AXValue`. Secure fields are `policy_denied` and the value is never read. Linux and Windows return `unsupported` until P13 and P12. |
| `select_text` via selected range | Yes | `AXSelectedTextRange` / UIA Text / AT-SPI Text | PARTIAL | P3 on macOS writes `AXSelectedTextRange` as an `AXValue` of type `kAXValueCFRangeType`. `start` must be less than or equal to `end`, and `end` must sit inside `AXNumberOfCharacters` when that attribute exists. Secure fields are refused. Windows UIA and Linux AT-SPI remain TODO for P12 and P13. |
| Secondary actions (`AXShowMenu`, increment, confirm) | Yes | `perform_action` allowlist | DONE | P3 allowlist: `AXPress`, `AXShowMenu`, `AXIncrement`, `AXDecrement`, `AXConfirm`, `AXCancel`, `AXRaise`, `AXScrollToVisible`, and the four page-scroll actions. Other names are `invalid_arguments`. A missing action is `execution_failed` and lists `CopyActionNames`. Linux and Windows return `unsupported` until P13 and P12. |
| Tree diff between turns | Yes | `--diff` and cache baseline | DONE | P2. `--diff` is the default once a baseline exists. The signature is role, identifier-or-title, and child count, with an LCS inside each parent. |
| Force-full flag | Yes | `--full` | DONE | P2. `--full` prints the whole tree and replaces the baseline. |
| Truncation of long child lists | Yes | 40 children, depth 12, 400 lines | DONE | P2 caps live in `tree.rs`: 40 children (`… +N`), depth 12, 400 lines, then `truncated=true`. Secure values and URL queries are not stored. |
| URL shortening | Yes | Scheme, host, path. Query stripped. | DONE | P2. `http` and `https` values keep scheme, host, and at most 60 characters of the path. The query is dropped. |
| Capture only the target window | Yes | SCK desktop-independent window, HWND, portal | DONE | P4 on macOS uses `SCContentFilter` `initWithDesktopIndependentWindow`. A CG window id missing from both shareable-content queries is `window_not_found` and is not cropped. A locked or asleep session, or ScreenCaptureKit returning no displays or windows, is `display_unavailable` before `window_not_found`. A window-targeted screenshot asks that before it resolves the window. Screen Recording off is `permission_required`. Linux and Windows return `unsupported` until P13 and P12. HWND and portal capture stay TODO there. |
| Every action can return the new state | Yes | `--state` plus optional screenshot | DONE | P5. Successful mutating actions return `state.tree` as numbered full text, `state.diff`, and the new revision. That diff becomes the `tree --diff` baseline. Read-only actions ignore `--state` and `--screenshot`. A tree or screenshot failure keeps the action result and adds `warnings`. |
| Events posted to the target pid | Yes | `CGEventPostToPid` | TODO | P7. |
| Window number and window-local location on those events | Yes | Field 91 and local point | TODO | P7. |
| Synthetic focus enforcer (SkyLight / CPS) | Yes, private | Feature `skylight`, fail closed | TODO | P7. Off unless the feature and `--tier synthetic` are both set. |
| Agent cursor overlay | Yes | Click-through `NSWindow`, helper process | TODO | P7. |
| Parallel agents | Yes | Per-session cache, no shared cursor on ax/event tiers | TODO | P7 lock. 0.1.0 has no session and always moves the one cursor. |
| Does not steal the cursor or the focus | Yes on the background path | Tiers `ax` and `event` | TODO | 0.1.0 steals both, via enigo. |
| `list_apps` | Yes | `list-apps` with bundle id, pid, path | DONE | P1 on macOS via NSWorkspace. Linux and Windows return `unsupported` until P13 and P12. |
| Window inventory | Yes | `list-windows` joined to CG z-order | DONE | P1 lists on-screen CG windows with z-order, bounds, layer, and window number. P2 sets `ax_matched` when an AX window shares those bounds within 2 points. The flag stays false, with a note, when Accessibility is off. |
| Launch apps | Yes | `launch` via `NSWorkspace` | DONE | P1 on macOS, with `activate` and `quit`. `quit` calls `terminate()` only. Off macOS those three commands return `unsupported`. |
| z-order | Yes | CGWindow list order, index 0 in front | DONE | P1. An `--app` filter renumbers so 0 is that app's front window. A window id uses the `id:` prefix. |
| Per-app approval / allowlist | Yes | `policy.toml` allow and deny | TODO | P6. The built-in deny list landed in P1. |
| Admin policy | Yes | `/etc/computer-use/policy.toml`, deny wins | TODO | P6. |
| Sensitive-action confirmation | Yes | Confirm hook, secure fields always refused | TODO | P6. |
| Refuses terminals | Yes | Built-in deny list | PARTIAL | P1 matches bundle id, executable file name, and an `.app` path component. P2 adds Terax (`app.crynta.terax`). `policy.toml` is P6. |
| Blocked-URL error | Yes | `blocked_url` for browsers and for CDP | TODO | P6, used again by P10. |
| Path-resolved app identity | Yes | Canonical executable path | PARTIAL | P1 deny checks the canonical executable path. `allow.path` enforcement is P6. |
| Relock / pause on local input | Yes | Listen-only `CGEventTap`, ignore our marker | TODO | P6. |
| Windows foreground-only plus allowlist | Yes, foreground-only | UIA patterns without foreground, policy still applies | TODO | P12. Target is past Codex. Status stays TODO until it ships. |
| Linux | Unverified | AT-SPI, ScreenCast, RemoteDesktop or libei, XTest fallback | TODO | P13. Codex does not ship a verified Linux backend. |
| Batched `actions[]` | Yes | `action: "batch"` | DONE | P5. One resolved target and one cache revision sequence. Each step re-validates. `stop_on_error` defaults true and sets `stopped_at`. Nested batch is `invalid_arguments`. `return_state` runs once at the end. A step's own `return_state` is ignored. |
| OpenAI `computer` tool adapter | No | `--adapter openai` | DONE | P5. Translates click, double_click, scroll, type, keypress, move, wait, and screenshot. `wheel` is middle. `back`, `forward`, and `drag` are `unsupported` until P8. Image coordinates need a stored screenshot map. No auto-detection. |
| Anthropic `computer_20251124` adapter | No | `--adapter anthropic` | DONE | P5. Translates screenshot, left/right/middle click, double_click, mouse_move, scroll, type, key, wait, zoom, and cursor_position. triple_click, left_click_drag, mouse down/up, and hold_key are `unsupported` until P8. |
| Screenshot coordinate remap, about 1440×900 | Yes | Default max-width 1440, map in every image, optional canvas | DONE | P4. Absent `max_width` scales a screenshot down to 1440. `0` does not scale. `--canvas WxH` letterboxes (`fit: canvas`, `pad` is the top-left inset, `content_width` and `content_height` are the drawn size). `scale` stays the backing scale. `image_to_screen` and `screen_to_image` remap image pixels and screen points. The map is stored at `sessions/<session>/screenshot-map.json` mode 0600 and does not change the tree baseline. Mixed-scale monitors share one scale on a stitched desktop image. Linux and Windows desktop screenshots use the same map; their window capture is P13 and P12. |
| MCP server | No | No | DONE | Out of scope on purpose. `serve` stays stdin/stdout. |
| Permission doctor with fix text | Limited | `doctor` JSON, `--request` optional | DONE | P1 on macOS reports AX trust, Screen Recording preflight, and the TCC responsible app. `input_monitor` stays null until P8. Linux notes P13 and Windows notes P12. |
| Clipboard get/set | Unknown | `arboard` 3.7.0, set requires confirm | TODO | P8. |
| Drag, mouse down/up, triple-click, hold key, xdotool names | Partial | Those actions on the same tiers | PARTIAL | Right, middle, and double click exist as foreground coordinates. P5 adapters return `unsupported` and name P8 for drag, triple-click, mouse down/up, and hold key. The rest is P8. |
| Menu bar traversal | Yes for AX menus | `menu list` / `menu click` | TODO | P9. |
| Apple Events, Shortcuts, Dock | Partial | Explicit commands | TODO | P9. JXA today is only the hidden AX fallback. |
| Spaces | Limited | Screen grouping shipped. Moves are private and optional. | TODO | P9. |
| CDP for Chrome | No, AX only | `chromiumoxide` behind feature `cdp` and `--cdp` | TODO | P10. Target is past Codex. |
| OCR and set-of-marks | Screenshot pixels | Vision on macOS, Media OCR on Windows, numbered boxes | TODO | P11. Linux OCR is an explicit gap. |
| Signed helper `.app` so TCC sticks | Yes, a helper binary | `Computer Use.app`, signing optional in CI | TODO | P8 script, P15 workflow. |
| Virtual display / isolation | No | `--display`, Xvfb recipe, private CGVirtualDisplay marked experimental | TODO | P14. VM proxy is documented and not built. |

## Progress
Update this table at the end of a phase. `commit` is the full hash on `main` of the commit that finished that phase. The planning row is this commit, so it cannot name its own hash. Do not mark a phase `DONE` until CI on that commit is green for the three OS jobs.

| Phase | Status | Commit | Notes |
| --- | --- | --- | --- |
| P0 Planning | DONE | this commit | No code changes. Hash is `git rev-parse HEAD` after this commit lands. |
| P1 Targeting, inventory, doctor | DONE | `921e942947a1ce841b3026c0d33fdd2caecc701e` | `rust-version` 1.91. Built-in deny. macOS list-apps, list-windows, launch, activate, quit, and doctor. Feature commit `4266448035c78352c79b9025db01c445d8d0ff39`; this hash is the Windows clippy fix CI accepted. `ax_matched` stays false until P2. `activate` does not raise one AX window. `quit` is `terminate()` only. Linux and Windows inventory returns `unsupported`. Their doctor probes stay null (P13 / P12) and `--request` does not prompt there. `dirs` is deferred to P2. Live doctor on this machine saw Accessibility denied for Grok Bot and Screen Recording granted. |
| P2 macOS AX tree, diff, cache | DONE | `605069185333dd9a018d2e58a66c9bd7564ff6c9` | Native AX tree, numbered text, diff, and session cache. Feature commit `514f94783b8ef92ee8c2e8ab01a1af04dd7ff284`; this hash is the Windows clippy fix CI accepted. Linux and Windows `tree` returns `unsupported`. osascript is the fallback only when creating the application element fails for a reason other than permission or a messaging timeout (`source=osascript`). The observer only marks a pid dirty; every observation still re-fetches. A crash, or a caller that never calls `shutdown_sessions`, can leave Electron flags set until the target app quits. A later `session end` restores from the cache and does not write an attribute this process did not change. Revalidation data is stored; `stale_element` is P3. Helper walks are capped at 3. `ax_matched` stays false when Accessibility is off. `--app` name ties prefer Regular activation policy. Terax (`app.crynta.terax`) is on the built-in deny list. Live `tree --app Finder` returned `permission_required` for Grok Bot. |
| P3 Element actions | DONE | `b849daae9081dcf8e6748828182849ef3018bce5` | `press`, `focus`, `set-value`, `select-text`, `perform-action`, `scroll-to`, and `click --element`, with revalidation. Feature commit `93892935bf1c03da044cc5189eafc7ceeddd845b`; this hash is the off-macOS clippy fix CI accepted. A role, identifier, or frame change beyond 2 points, or a cached pid, window, or revision header that does not match, is `stale_element` and performs nothing. Element actions update `observed` and leave the `tree --diff` baseline. `type --element` uses `AXValue` when settable (`tier: ax`); otherwise it focuses and types with enigo (`tier: enigo`). `type` without `--element` stays enigo. Secure fields refuse `set_value`, `select_text`, and `type --element` with `policy_denied` and the value is never read. The built-in deny list runs before any AX call on the target. `click --x --y` hit-tests first; a hit-test error is a warning and the enigo click still runs. Event posting is P7. A failed post-action re-read is a warning and keeps the previous revision. Linux and Windows return `unsupported` (P13 / P12). Live `focus --app Finder --element 0` returned `permission_required` for Grok Bot. No live action was performed. |
| P4 Window capture and remap | DONE | `959495a2d3e8088019d8f98e3b3f8ed0e69e1b99` | Per-window ScreenCaptureKit capture and the screenshot map. Feature commit `7455d5f38a054d29ed08a378d143b2676db5f87a`; this hash is the off-macOS clippy fix CI accepted. Default max-width is 1440 when `max_width` is absent. `--max-width 0` keeps the source width; that used to be `invalid_arguments` (changelog in P15). macOS 14+ uses `SCScreenshotManager`; older systems and a manager failure take one `SCStream` frame. `ignoreShadows` is `ignoreShadowsSingleWindow`. Other ScreenCaptureKit errors crop xcap (`capture_source: xcap`, `occluded: true`). Permission and a window missing from both shareable queries do not fall back. The map file is separate from the tree snapshot. Mixed-scale monitors keep one scale on a stitched desktop image. This crate does not call `CGWindowListCreateImage`. Linux and Windows `screenshot --app` returns `unsupported` until P13 and P12. Live `screenshot --app Finder` returned `window_not_found`: Finder had no on-screen non-desktop window. Screen Recording was granted. Accessibility was denied, so sheet compositing was not exercised. No app was activated. The `sck` success path was not confirmed on a live window. |
| P5 State, batch, adapters | DONE | `5b39a5ea8482673e1d77351b935e1b02e576f428` | Post-action `return_state` (`--state`) snapshots `TreeReport.full_text` and refreshes the `tree --diff` baseline. Without it, `observed` updates and the baseline stays put. `include_screenshot` (`--screenshot`) uses the P4 capture path. A capture or tree failure leaves the action result, nulls that field, and adds `warnings`. Read-only actions ignore both flags. `screenshot` can return state and copies its body into `state.screenshot`. JSON `batch` shares one target and one revision sequence, re-validates each step, and applies `return_state` once at the end. `stop_on_error` defaults true and sets `stopped_at`. Nested batch is `invalid_arguments`. OpenAI `computer` and Anthropic `computer_20251124` adapters set `coord=image` and remap through `screenshot-map.json`. A missing map is `invalid_arguments` ("take a screenshot first"). Drag, triple-click, mouse down/up, `hold_key`, and OpenAI back/forward buttons are `unsupported` until P8. OpenAI `wheel` is middle. OpenAI `wait` with no duration is 0 ms. `cursor_position` is read-only. `display_unavailable` covers a locked or asleep session and an empty display or shareable-window list, including a window-targeted screenshot before window resolution. `session prune` removes empty dirs and stale dirs whose owner pid is dead and older than the TTL (default 24h). A live pid with content is kept. A pid above `i32::MAX` counts as not alive, because Linux `kill -0` would treat it as `-1`. Tests set `XDG_CACHE_HOME`. Feature commit `fe7c79bf599a8da6f2e56b7bdb428f4d8a3d8af9`. `6c4f784269b1d336627278f829cb1e61414b535d` limits the missing-window helper to macOS and tests. This hash is the Linux pid check CI accepted. Live smoke used a temp cache with the screen locked. Desktop screenshot, `screenshot --app Finder`, and both adapter screenshot actions returned `display_unavailable` ("the screen is locked"). `session prune --dry-run` returned ok. No click, type, activate, launch, or quit. Accessibility was not exercised. Pixel capture was not confirmed. |
| P6 Safety | TODO | | policy.toml, audit, tap, URL block. |
| P7 Background input and overlay | TODO | | SkyLight behind `skylight`. |
| P8 More actions, clipboard, `.app` | TODO | | Packaging script. |
| P9 Menu bar, Apple events, Dock, Spaces | TODO | | Space moves stay private. |
| P10 CDP | TODO | | Feature `cdp`. Attach only. |
| P11 Vision and set-of-marks | TODO | | Linux OCR not in 0.2.0. |
| P12 Windows UIA and capture | TODO | | No untargeted foreground walk. |
| P13 Linux AT-SPI, portal, X11 | TODO | | Feature `portal` for PipeWire. |
| P14 Isolation | TODO | | Ship `--display` and the Xvfb doc. Mark the rest. |
| P15 Docs and 0.2.0 release | TODO | | Version bump, README, signed zip when secrets exist. |

## Suggested implementation order inside a phase
Each phase is one commit unless the note below says otherwise. Do not start P3 before P2 is on `main`. P4 can follow P2 in parallel with P3 only if the screenshot command does not need element indices. P5 depends on P2 and P4. P6 should land before P7 so the event marker and the tap share one constant. P12 and P13 depend on P2's `tree.rs` and P6's policy, and they can proceed in parallel with each other. P10 and P11 depend on P4's map. P14 and P15 are last.

P2 is the largest phase. Split it only if review requires it: first the pure `tree.rs` and cache, then `ax.rs`. Both halves stay inside P2 in this table. The second half's commit message still says `P2`.
