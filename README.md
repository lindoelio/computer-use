# computer-use

Cross-platform desktop automation for agents: screenshots, mouse, keyboard, scroll, displays, accessibility trees, OCR, CDP attach, isolation, and a permission doctor. One Rust binary on macOS, Linux, and Windows. The same actions are available as a Rust library.

The [smart-computer-use](https://github.com/lindoelio/smart-computer-use) agent skill shells out to this binary.

## Install

### GitHub Releases

Download a build for your platform from [GitHub Releases](https://github.com/lindoelio/computer-use/releases):

| Asset | Platform | Notes |
| --- | --- | --- |
| `computer-use-linux-x64.tar.gz` | Linux x86_64 | Built with `--features portal` (ScreenCast + RemoteDesktop + PipeWire). |
| `computer-use-macos-arm64.tar.gz` | macOS Apple silicon | Default features. `Computer Use.app` is bundled as a separate zip when the helper is built (see `docs/PACKAGING.md`). |
| `computer-use-windows-x64.zip` | Windows x86_64 | Default features. |

Each archive contains the `computer-use` binary (`computer-use.exe` on Windows). A matching `<asset>.sha256` file and a combined `SHA256SUMS` file are attached to the release.

```bash
# macOS (Apple silicon)
curl -fsSL -o computer-use-macos-arm64.tar.gz \
  https://github.com/lindoelio/computer-use/releases/latest/download/computer-use-macos-arm64.tar.gz
tar -xzf computer-use-macos-arm64.tar.gz
sudo install -m 755 computer-use /usr/local/bin/computer-use

# Linux x86_64
curl -fsSL -o computer-use-linux-x64.tar.gz \
  https://github.com/lindoelio/computer-use/releases/latest/download/computer-use-linux-x64.tar.gz
tar -xzf computer-use-linux-x64.tar.gz
sudo install -m 755 computer-use /usr/local/bin/computer-use
```

Check the archive before you install it:

```bash
shasum -a 256 -c computer-use-macos-arm64.tar.gz.sha256
# or, on Linux:
sha256sum -c computer-use-linux-x64.tar.gz.sha256
```

### cargo

```bash
cargo install computer-use
```

Rust 1.91 is the supported minimum. On Linux, install the native libraries listed under [Permissions](#permissions) before building.

The default build carries no optional feature. On Linux, `cargo install computer-use --features portal` gives the same capability as the release binary: per-window capture and event-tier input on a Wayland session.

## Usage

Every action writes one JSON object to stdout and a trailing newline. A failed action exits with status 1 and a `{"ok":false,"error":{...}}` body. A bad command line (missing flags, unknown arguments) exits with status 2 and still prints JSON. `--help` and `--version` stay human-readable and exit 0.

```bash
computer-use --help
computer-use --version
computer-use wait --ms 10
computer-use --pretty wait --ms 10
```

`serve` reads newline-delimited JSON from stdin and writes one JSON response per line. `--json` does the same. `exec` runs one JSON action from an argument and honors `--pretty`. There is no TCP server in any release.

```bash
printf '%s\n' '{"id":1,"action":"wait","ms":10}' | computer-use serve
printf '%s\n' '{"id":1,"action":"click","x":10,"y":20}' | computer-use --json
computer-use exec '{"action":"wait","ms":10}'
computer-use --pretty exec '{"id":"a","action":"displays"}'
```

`--adapter native|openai|anthropic` rewrites the `exec` and stdin requests on the way in. The native protocol is what `Action` and the action-name list describe below; the adapter layers translate the OpenAI `computer` and Anthropic `computer_20251124` shapes into the native one and set `coord=image`. See `docs/CLI.md` for the full JSON schema and `--help` for the surface area.

### Default max-width

`screenshot` (and `zoom`) scale the image down so its width is at most **1440 pixels** by default. Pass `--max-width 0` to keep the source width. `--max-width N` with any other positive integer caps the width at `N`. `--canvas WxH` letterboxes the result into `WxH` without scaling up. On a Retina display the image pixel size can be `scale` times the logical size.

### Policy path

`~/.config/computer-use/policy.toml` on every OS, including macOS and Windows. The admin file is `/etc/computer-use/policy.toml` on Linux and macOS, and `%ProgramData%\computer-use\policy.toml` on Windows. `--policy <PATH>` overrides the user file. A missing file is **default allow** plus the built-in deny list. `--policy check --app X` reports what the effective policy would decide without performing the action.

### Feature flags

| Flag | What it pulls in | Default on this binary |
| --- | --- | --- |
| (default) | Screenshots, mouse, keyboard, accessibility tree, OCR, isolation, the macOS helper `.app` plumbing. | always |
| `cdp` | `chromiumoxide` 0.9.1 + a small `tokio`. Required for `computer-use --cdp browser ...`. | **on in the release binaries**; off for a default `cargo install` |
| `portal` | `ashpd` 0.13.13 ScreenCast + RemoteDesktop, `pipewire` 0.9, `reis` 0.7. Linux only. | on in the Linux release binary; off unless you ask for it |
| `skylight` | macOS private SkyLight focus path behind `--tier synthetic` / `--synthetic-focus`. | off |
| `virtual-display` | macOS private `CGVirtualDisplayDescriptor` probe; probe-only, never creates a display. | off |

`--features cdp`,`--features portal`,`--features skylight`,`--features virtual-display` build what the flag page describes. `--all-features` compiles everywhere (Linux builds also link PipeWire).

### Top-level commands

```
screenshot          Capture the screen, a region, or one window, and write a PNG or JPEG file
click               Move the pointer to a logical point and click, or press an element
triple-click        Click three times. Alias of `click --count 3`
press               Press an element from the current tree. `--count` may be 1, 2, or 3
set-value           Set an element's value. Secure fields are refused
select-text         Select a character range. Secure fields are refused
perform-action      Perform an allowlisted accessibility action
scroll-to           Scroll an element into view
focus               Focus an element
move                Move the pointer without clicking
drag                Drag from the first point through the rest, then release
mouse-down          Press a mouse button and leave it down until `mouse-up`
mouse-up            Release a mouse button pressed by `mouse-down`
type                Type Unicode text into the focused control
key                 Press a key combination
hold-key            Hold one key combination, then release it
clipboard           Read or write the clipboard (`get`, `set`)
zoom                Screenshot one region. With `--app`/`--window`, the region is window-local
scroll              Scroll the mouse wheel
get-state           Report displays, cursor, frontmost window, and a best-effort accessibility tree
wait                Sleep, then return `waited_ms`
displays            List connected displays without capturing pixels
isolation           Report what this build can isolate and what it cannot
list-apps           List running applications
list-windows        List on-screen windows in z-order. Index 0 is front
launch              Launch an application by name, bundle id, path, or pid
activate            Bring an application forward
quit                Ask an application to quit
policy              Show whether policy allows an app. This does not perform an action
pause               Pause mutating actions until `resume`
resume              Clear the pause flag
doctor              Report permissions, the TCC identity, and how to fix what is missing
tree                Print the key window's accessibility tree
ocr                 Run OCR on the most recent capture for `--app` or `--window`
menu                Walk a running app's menu bar (macOS only)
apple-event         Send an Apple event script via `osascript` (macOS only)
shortcuts           Run a Shortcuts workflow by name (macOS only)
dock                List or click Dock items (macOS only)
spaces              Group on-screen windows by the screen that contains each window's center
session             Session cache and accessibility teardown (`end`, `prune`)
serve               Read newline-delimited JSON actions from stdin
exec                Run one JSON action from an argument
batch               Run a batch of JSON actions from an argument
cursor-position     Print the pointer position
overlay             Local agent-cursor helper. This is not a network service
browser             Chrome DevTools Protocol (CDP) subcommands. Requires `--cdp`
help                Print this message or the help of the given subcommand(s)
```

### Permissions

Input and capture need permission from the operating system. `get-state` still returns `ok: true` when a subsystem is denied: the missing field is `null` and `note` says why. `doctor` enumerates each required grant plus the TCC identity; the JSON result is the source of truth.

#### macOS

| Permission | What uses it | Where to grant |
| --- | --- | --- |
| **Screen Recording** | `screenshot`, `zoom`, the `capture` path in `get-state`, ScreenCaptureKit window capture | System Settings → Privacy & Security → Screen Recording |
| **Accessibility** | The accessibility tree, hit-testing, `AXPress`, `set_value`, `select_text`, element `type`, `menu`, Apple events, `osascript` fallback | System Settings → Privacy & Security → Accessibility |
| **Post Event** | Tier `event` (`CGEventPostToPid`), `CGEventCreate`, hit-test | System Settings → Privacy & Security → Accessibility |
| **Input Monitoring** | The P6 kill-switch tap | System Settings → Privacy & Security → Input Monitoring |

The bundle id is `ai.lindoelio.computer-use` when the binary is run from `Computer Use.app` (`scripts/package-macos-app.sh` builds it). A bare binary does **not** have a stable bundle id — the TCC grant is for the signed bundle. The `--prompt` flag on `doctor` calls the macOS request APIs without changing anything in this process otherwise; `--open-settings` opens the first missing pane.

#### Windows

Run the process in a session that can send input (an interactive desktop, not Session 0).

- Apps and windows come from `EnumWindows`, `GetWindowTextW`, `GetWindowThreadProcessId`, and `QueryFullProcessImageNameW` (`windows` 0.62). There is no PowerShell anywhere in the binary.
- The accessibility tree is the UI Automation control-view walker (`uiautomation` 0.25.1). `tree` reports `source: "uia"`, `--app pid:NNN` is a deterministic selector, and `GetForegroundWindow` is never used to resolve a window the caller already named.
- Element actions run through UIA control patterns (`Invoke`, `Value`, `Text`, `ScrollItem`, `ExpandCollapse`) in the background — tier `ax`, no cursor motion. `IsPassword` controls refuse `set_value` and `type --element`. When a pattern is missing, or its single call fails (some Win32 apps ignore `Invoke` until they are in front), the action is retried as tier `activate`: `SetForegroundWindow` plus enigo input, under the same `foreground.lock` as every other foreground path, with a `foreground required` warning on the result. `--require-background` refuses that fallback, and the P6 policy allowlist still applies.
- Per-window capture is one `Windows.Graphics.Capture` frame from the resolved `HWND` (`windows-capture` 2.0.1). If graphics capture cannot produce a frame, the desktop image is cropped instead and reported as `capture_source: "xcap"`, so covered pixels may be wrong.
- OCR is `Windows.Media.Ocr.OcrEngine` for the user-profile languages. If no recognizer language pack is installed, `ocr` returns `execution_failed` naming Settings → Apps → Optional features.
- `doctor` reports `accessibility` (did `UIAutomation::new` succeed), plus best-effort `uiaccess` and `integrity` read from the process token with `OpenProcessToken` / `GetTokenInformation`. Neither is a required grant.

A corporate policy that blocks UI Automation or simulated input surfaces as `execution_failed` or a null field.

#### Linux

The binary talks to X11 through `libxdo` (enigo) and captures through `xcap` (XCB, RandR, D-Bus). On a Wayland session, the `portal` feature is needed for capture (ScreenCast) and event-tier input (RemoteDesktop). Install the build packages on Debian/Ubuntu:

```bash
sudo apt-get install -y pkg-config libxdo-dev libx11-dev libxcb1-dev \
  libxrandr-dev libdbus-1-dev libxkbcommon-dev libatspi2.0-dev \
  libpipewire-0.3-dev libclang-dev clang
```

`xdotool` on `PATH` improves the frontmost-window lookup. The accessibility tree is `AT-SPI` (default features). Tier `event` is X11 XTest on plain X11 and the RemoteDesktop portal on Wayland when the `portal` feature is compiled in. Without `portal`, Wayland tier `event` is `unsupported`. Grant the session access to the X display (`DISPLAY` set, and the same user that owns the session).

The release binary is built with `--features portal`, which links `libpipewire-0.3` at build time and pulls one frame off the portal's PipeWire stream. Linux OCR is `unsupported`.

## Input tiers

`--tier` selects the input path. Auto (the default) is platform-tuned:

- **macOS**: `ax` for presses and value changes; `event` then `activate` for coordinates, `type` without `--element`, `key`, and `scroll`. `synthetic` is never automatic.
- **Windows**: `ax` (UIA control patterns); falls back to `activate` (`SetForegroundWindow` + enigo) under `foreground.lock` when a pattern is missing or fails. Auto is `activate` for free-text `type`, `key`, and `scroll`.
- **Linux**: `ax` (AT-SPI) when present; `event` (XTest on X11, RemoteDesktop portal on Wayland) for coordinates; `activate` (enigo) as the foreground fallback.

| `--tier` | What it does |
| --- | --- |
| `ax` | Accessibility action or value. Does not activate the app. Also the scroll-bar path for `scroll`. |
| `event` | Post events to the target process. Does not move the system cursor. macOS posts to the target pid via `CGEventPostToPid`. Linux uses XTest on X11 and the RemoteDesktop portal on Wayland. |
| `synthetic` | Private SkyLight front-process call, then post events. Requires `--features skylight` and `--tier synthetic` (or `--synthetic-focus`). |
| `activate` | Activate the target, run foreground input, then restore the previous app. Takes `foreground.lock`. |

`--require-background` refuses `activate` and turns cursor motion into `execution_failed`. `--allow-foreground` skips the lock. `--no-overlay` skips the agent-cursor helper socket. The agent cursor overlay (`overlay serve`) is macOS-only; a missing helper adds `overlay helper not running` and the action still succeeds.

### Scroll is verified on macOS, not assumed

A scroll wheel posted with `CGEventPostToPid` is **not delivered to the views of a non-frontmost app**: the post returns without error and nothing moves. So `scroll` on macOS measures instead of assuming. It reads the `AXScrollArea` scroll bar (under `--x/--y`, else the window's first usable scroll area) before and after the post, then reports what actually happened:

| `via` | When |
| --- | --- |
| `event` | The posted wheel event moved the bar. |
| `ax_scrollbar` | The wheel event did nothing, so `AXValue` was written on the bar. Adds the warning `wheel event not delivered to background app; used AX scroll bar`. Neither path activates the app or moves the cursor. |

`scrolled` is `true` only when a bar value really changed, and `before` / `after` carry the values (0.0–1.0) on the primary axis. A bar already at its limit returns `ok` with `scrolled: false` and a warning naming the edge; a scroll that cannot be verified at all (no `AXScrollArea`) is `execution_failed`, never a silent success. `--tier ax` scrolls by driving the bar directly and posts no event.

Two honest limitations:

- **The wheel event is not delivered even to a frontmost app.** Measured live on macOS 27 with TextEdit frontmost: the posted wheel left `AXValue` untouched, so the fallback ran. The verification is what makes that visible instead of silent. The warning text still says "background app" because that contract is fixed.
- **The step is an approximation.** AX does not say how much of a document is on screen, and an `AXScrollBar` element spans the whole track, so the derived fraction sits near 1.0 and one wheel line moves roughly a third of the content. That is close to the system default of three lines per notch, but it is a coarse scroll, not a precise one. Pass an explicit `--amount` for control.

Windows and Linux do not verify this way; their scroll responses keep their existing shape.

## JSON actions

The `Action` enum in `src/protocol.rs` is the protocol. `id` is echoed on the response. Unknown fields that belong to the action are rejected. Global flags (`app`, `window`, `element`, `diff`, `full`, `return_state`, `include_screenshot`, `coord`, `session`, `tier`, `adapter`, `display`, `require_visible`, `policy`, `confirm_cmd`, `allow_foreground`, `require_background`, `no_overlay`, `cdp`, `cdp_url`) are siblings of `action` and stripped before the action is decoded.

| `action` | Fields | Notes |
| --- | --- | --- |
| `screenshot` | `output`/`path`, `region` (`"x,y,w,h"` or `{x,y,width,height}`), `format` (`png`/`jpeg`/`jpg`), `max_width`, `canvas` (`"WxH"`), `base64`, `marks` | Default `max_width` is 1440. `0` keeps the source width. `marks` stamps tree indices (`n`) on a window target or OCR indices (`oN`) when the tree is empty. |
| `click` | `x`, `y`, `button` (`left`/`right`/`middle`/`back`/`forward`), `double`, `count` | `count` is 1, 2, or 3. `double: true` means `count: 2` when omitted. With `--element`/`element` the screen coordinates are ignored. |
| `double_click` | `x`, `y`, `button` | Alias: `click --double`. |
| `triple_click` | `x`, `y`, `button` | Alias: `click --count 3`. |
| `move` | `x`, `y` | Alias: `move_mouse`. |
| `press` | `count` (1–3), `button` | AXPress for left, AXShowMenu for right when advertised. |
| `type` | `text` | Uses `AXValue` when `--element` is set and settable. |
| `key` | `keys` | Combo such as `cmd+shift+4`. |
| `scroll` | `x`, `y`, `dx`/`scroll_x`, `dy`/`scroll_y`, `direction`, `amount` | Positive `dy` is down; positive `dx` is right. `direction` + `amount` is a separate form; do not mix. macOS adds `via`, `scrolled`, `before`, `after` (see *Scroll is verified on macOS*). |
| `drag` | `path` (array of points or `x,y,x,y` string), `button` | At least two points. Tier `event` posts mouse-dragged events to the pid; tier `activate` uses enigo. |
| `mouse_down` | `x`, `y`, `button` | Held until `mouse-up` or process exit. |
| `mouse_up` | `x`, `y`, `button` | Releases the press from `mouse_down`. |
| `hold_key` | `keys`, `ms` (≤ 30000) | One combination. Space-separated sequences are refused. |
| `zoom` | `region` (required), `output`/`path`, `format`, `max_width`, `canvas`, `base64` | Window-local coordinates when `--app`/`--window` is set. |
| `clipboard_get` | none | Text wins; `kind: "empty"` is reported. An image is written to the session cache and `kind: "image"` is reported with a `path`. |
| `clipboard_set` | `text`, `image` (PNG path) | Requires a confirm hook. Empty `text` is `invalid_arguments`. |
| `focus` | none | Sets `AXFocused` on the `--element`. |
| `set_value` | `value` | Refused for secure fields (`policy_denied`; the value is never read). |
| `select_text` | `start`, `end` | Refused for secure fields. |
| `perform_action` | `name` | Allowlist (`AXPress`, `AXShowMenu`, `AXIncrement`, `AXDecrement`, `AXConfirm`, `AXCancel`, `AXRaise`, `AXScrollToVisible`, the four page-scroll actions). |
| `scroll_to` | none | Aliases: `scroll-to`, `scroll_to_visible`, `scroll-to-visible`. |
| `get_state` | none | Alias: `get-state`. |
| `cursor_position` | none | Reads the pointer. The Anthropic adapter maps `cursor_position` here. |
| `wait` | `ms`, `seconds` | Summed when both are set. Fractions are allowed in `seconds`. |
| `displays` | none | List displays without capturing pixels. |
| `isolation` | none | Pure report: what this build can isolate. |
| `list_apps` | `all` | `all: true` includes accessory and prohibited apps. |
| `list_windows` | none | z-order; index 0 is front. |
| `launch` | `app` (sibling) | Launch via `NSWorkspace` on macOS; otherwise `unsupported`. |
| `activate` | `app`, `window` | `--window` is checked first. |
| `quit` | `force` | `--force` is a confirm-gated terminate. |
| `policy_check` | `app` (sibling) | Reports `allow`/`deny` without performing the action. |
| `pause` / `resume` | none | Cross-process kill switch. macOS only (CGEventTap). |
| `doctor` | `prompt` (alias `request`), `open_settings`, `x11` | Envelope `ok: true` even when a permission is missing. |
| `tree` | none | Numbered accessibility tree for the key window. |
| `ocr` | `force` | Re-run OCR even when `--app` already produced it. |
| `session_end` | none | Restore Electron accessibility flags this process changed. |
| `session_prune` | `dry_run`, `ttl_secs` | Remove empty session directories and stale ones. Default TTL is 24h. |
| `menu_list` | `app` (override), `depth` | macOS only. Walks `AXMenuBar → AXMenuBarItem → AXMenu`. |
| `menu_click` | `app` (override), `path` (`"File > Save"`) | macOS only. `AXPress`es the leaf. |
| `apple_event` | `app` (override), `script`, `language` (`applescript`/`javascript`) | macOS only. Runs `osascript`. The script body is never logged. |
| `shortcuts_run` | `name` | macOS only. Invokes `shortcuts run <name>`. |
| `dock_list` | none | macOS only. Lists Dock items. |
| `dock_click` | `title` | macOS only. Case-insensitive title match. |
| `spaces` | none | Groups on-screen windows by screen. Mission Control moves are not attempted. |
| `batch` | `actions`, `stop_on_error` | One resolved target, one revision sequence. Each step re-validates. Nested batch is `invalid_arguments`. |
| `browser_tabs` | none | `cdp` feature. Returns `{tabs, count}`; each tab has `kind`, `target_id`, `url`, `title`. |
| `browser_activate` | `target_id` | `cdp` feature. |
| `browser_navigate` | `url`, `target_id`, `wait`, `timeout_ms`, `new_tab` | `cdp` feature. URL is checked against `[browser].blocked_urls`. |
| `browser_back` / `browser_forward` | `target_id` | `cdp` feature. |
| `browser_reload` | `target_id`, `wait`, `timeout_ms` | `cdp` feature. |
| `browser_wait` | `target_id`, `wait`, `timeout_ms` | `cdp` feature. |
| `browser_snapshot` | `target_id`, `diff`, `interactive_only`, `max_nodes` | `cdp` feature. Compact accessibility tree with `ref=eN` handles. |
| `browser_click` | `selector`/`ref_id`, `target_id`, `button`, `count`, `wait`, `timeout_ms`, `force` | `cdp` feature. Actionability checks by default. |
| `browser_text` | `selector`/`ref_id`, `target_id` | `cdp` feature. |
| `browser_type` | `text`, `selector`/`ref_id`, `target_id`, `clear`, `submit`, `wait`, `timeout_ms` | `cdp` feature. Secure fields are refused. |
| `browser_press` | `keys`, `selector`/`ref_id`, `target_id` | `cdp` feature. |
| `browser_hover` | `selector`/`ref_id`, `target_id` | `cdp` feature. |
| `browser_scroll` | `selector`/`ref_id`, `target_id`, `dx`, `dy` | `cdp` feature. |
| `browser_select` | `selector`/`ref_id`, `target_id`, `value`, `label` | `cdp` feature. |
| `browser_check` | `selector`/`ref_id`, `target_id`, `checked` | `cdp` feature. |
| `browser_upload` | `selector`/`ref_id`, `target_id`, `files` | `cdp` feature. |
| `browser_dialog` | `target_id`, `accept`, `prompt_text` | `cdp` feature. |
| `browser_screenshot` | `target_id`, `output`, `full_page`, `format`, `quality` | `cdp` feature. |
| `browser_eval` | `script`, `target_id`, `timeout_ms`, `await_promise` | `cdp` feature. The script body is never logged verbatim. |
| `browser_tab_new` | `url` | `cdp` feature. |
| `browser_tab_close` | `target_id` | `cdp` feature. |
| `browser_launch` | `browser`, `headless`, `profile`, `port`, `url` | `cdp` feature. Starts a managed Chromium with an isolated profile. |
| `browser_quit` | `session`, `purge` | `cdp` feature. |
| `browser_sessions` | none | `cdp` feature. |

```json
{"id":1,"action":"click","x":10,"y":20,"button":"left"}
{"id":2,"action":"key","keys":"cmd+shift+4"}
{"id":3,"action":"scroll","direction":"down","amount":3}
{"id":4,"action":"screenshot","region":"0,0,800,600","format":"png"}
{"action":"get_state"}
```

The `--adapter openai` and `--adapter anthropic` translations cover click, double_click, scroll, type, keypress, move, wait, screenshot, cursor_position, back, forward, drag, mouse_down, mouse_up, triple_click, left_click_drag, hold_key, and zoom.

### Error codes

The JSON `error.code` is one of the strings below. `ComputerError::code()` is the stable contract.

| Code | When |
| --- | --- |
| `invalid_arguments` | Bad CLI flags or malformed JSON. |
| `execution_failed` | Framework error that is not in this table. |
| `io_error` | stdin/stdout/filesystem failure. |
| `unsupported` | Off-platform or feature-off. |
| `app_not_found` | Name, bundle id, or pid matched nothing running, and launch was not requested. |
| `window_not_found` | App resolved, window did not. |
| `element_not_found` | Index missing in the cached or fresh tree, or there is no session cache. |
| `stale_element` | The path's role, identifier, or frame (beyond 2 points) changed, or the cached pid, window, or revision header does not match. Action not performed. Body includes the new tree. |
| `permission_required` | Accessibility or Screen Recording is off. The `doctor` `fix` object names the Settings pane. |
| `policy_denied` | Built-in or configured deny. |
| `confirm_required` | Sensitive action and no confirm hook is configured. |
| `confirm_denied` | Hook exited non-zero or timed out. |
| `blocked_url` | Browser URL matched the block list. |
| `paused` | Local user input tripped the kill switch. |
| `cdp_unavailable` | Feature off, or Chrome has no debugging port. |
| `timeout` | A browser wait, action, or dialog rescue exceeded its timeout. |
| `display_unavailable` | The screen is locked, the display is asleep, or no display or shareable window is available. Checked before `window_not_found`; no xcap crop fallback. |

## Library

```rust
use computer_use::{dispatch, Action};

let response = dispatch(Action::Wait {
    ms: Some(0),
    seconds: None,
});
assert!(response.ok);
```

`DesktopBackend` (also exported as `Backend`) is the platform trait. `create_desktop_backend()` returns the backend for the current operating system. `dispatch` and `dispatch_with` share the path used by the CLI. `Action` and the JSON request parser use serde with `tag = "action"`. Errors use `thiserror` (`ComputerError`). The full surface (`CallContext`, `LineOptions`, `handle_request_line_opts`) is documented in `docs/CLI.md` and lives in `src/protocol.rs`.

## Coordinates

Click, move, scroll, and screenshot regions use **logical** points. A display's `width`, `height`, `x`, and `y` are logical. `scale` is physical pixels divided by logical width (often `2.0` on Retina). A screenshot's `width` and `height` are image pixels, which can be larger than the logical region. The map lives at `${XDG_CACHE_HOME:-~/.cache}/computer-use/sessions/<session>/screenshot-map.json` (macOS and Linux) or `%LOCALAPPDATA%\computer-use\cache\sessions\<session>\screenshot-map.json` (Windows), mode 0600.

`--coord image` (or `coord=image`) interprets `x`/`y` as image pixels of the most recent screenshot; the map remaps them to screen points. The OpenAI and Anthropic adapters set `coord=image` automatically and require a stored map.

`--display <id>` resolves a monitor from `displays` and gates every screen point, `--region`, and `--window` against it. Points outside the monitor are `invalid_arguments`. The filtered capture reuses the shared `backend::capture` path, so the map origin is the monitor origin. `--display virtual` is Linux-only; it prints the Xvfb recipe and never captures.

## Isolation

`isolation` is a pure report (`cdp` is not part of the 0.4.0 output). macOS reports `virtual_display: "experimental"` only when both `CGVirtualDisplayDescriptorCreate` and `CGVirtualDisplayCreate` resolved; otherwise `unsupported`. The build never creates a virtual display — see `docs/ISOLATION.md` for the reason and the VM proxy design.

## Development

```bash
cargo fmt --all
cargo build
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

Tests do not move the pointer, capture the screen, or require Screen Recording or Accessibility. macOS-only tests behind `#[cfg(target_os = "macos")]` are skipped on Linux and Windows runners. The CI matrix is Ubuntu, macOS, and Windows, and every one of them checks that the default feature set compiles and that the `--all-features` build passes clippy and the tests. Ubuntu additionally checks the `--features portal` build, which links PipeWire and never contacts a portal daemon.

`~/.grok-prompts/linux-check.sh` runs the same checks inside `rust:1-bookworm` when Docker is available; otherwise CI is the gate.

## Documentation

- [`docs/CLI.md`](docs/CLI.md) — every command, flag, JSON action, and error code, generated from `--help` and the protocol enums.
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — the phase plan, the progress table, and the parity audit.
- [`docs/ISOLATION.md`](docs/ISOLATION.md) — display filter, Xvfb recipe, virtual-display limitations.
- [`docs/PACKAGING.md`](docs/PACKAGING.md) — macOS `.app` packaging, signing, notarization.
- [`docs/SKYLIGHT.md`](docs/SKYLIGHT.md) — synthetic-focus design notes.
- [`docs/policy.example.toml`](docs/policy.example.toml) — policy file reference.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Copyright 2026 Lindoélio Lázaro.
## Notes from live testing

- **Element indices across CLI calls.** Indices from `tree` are cached per session. The default session is the current process, so separate one-shot invocations only share indices when you pass the same `--session <ID>` to `tree` and to the element action.
- **`set_value` replaces the field.** `set-value` (and `type --element N` without an explicit `--tier`) writes the AX value, replacing the whole field contents. Pass `--tier event` to insert keystrokes at the focused caret instead.
- **`type --tier event` needs keyboard focus.** Keystrokes posted to a pid are dropped if no element in that app has focus. `type` then returns ok with a `warning`; click the field (`click --tier event`) or pass `--element N` first.
- **`scroll --tier event` used to lie.** It returned `ok` without scrolling a background app, because a wheel event posted to a non-frontmost pid is never delivered to its views. `scroll` now reads the scroll bar back and reports `via`, `scrolled`, `before`, and `after`; when the wheel event is not delivered it falls back to the `AXValue` scroll bar and says so in a warning. If there is no scroll area to verify against, it is `execution_failed` instead of a silent success.
