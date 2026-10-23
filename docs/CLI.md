# CLI reference

Every command, flag, JSON action, and error code that the 0.3.0 binary accepts. The source of truth is the help output (`--help` on the binary) and the `Action`, `ComputerError`, and `CallContext` types in `src/protocol.rs` / `src/error.rs`. This document regenerates from `cargo build && ./target/debug/computer-use --help` and the protocol enums.

If you find a divergence between this file and the binary, the binary is right. The test `tests/cli_doc_matches_help.rs` (added in P15) parses this Markdown and asserts that every JSON `action` name and every CLI subcommand listed here is reachable on a fresh build.

## Conventions

- Logical coordinates everywhere on the CLI and in JSON. Image-pixel coordinates are accepted when `coord: "image"` or `--coord image`.
- One JSON object on stdout for every action. `--pretty` pretty-prints a one-shot; `serve` and `--json` stay one object per line.
- `--help` and `--version` exit 0 and are human-readable. Other commands exit 0 on success and 1 on `{"ok":false,...}`. Bad CLI flags exit 2 and still print JSON.

## Post-action state

A mutating action with `--state` returns a `state` block beside the action result:

```json
{
  "ok": true,
  "action": "press",
  "state": {
    "app": "com.apple.Notes",
    "pid": 812,
    "window_id": 44,
    "revision": 18114448678970534631,
    "tree": "[0] AXWindow ...",
    "diff": "~ [9] AXTextField value: \"old\" -> \"new\"",
    "truncated": false,
    "baseline": "existing"
  }
}
```

`state.tree` is the whole numbered tree and `state.diff` is the delta against the previous observation. On a busy window the tree runs to tens of kilobytes while the delta is a few lines, so an agent that drives a loop should ask for the delta only:

```
computer-use press --app Notes --element 12 --state-diff
```

`--state-diff` implies `--state` and omits `state.tree`. The block keeps `app`, `pid`, `window_id`, `revision`, `diff`, `truncated`, and `baseline`.

A read-only action never attaches a state block. A failed action never attaches one either. When the action succeeded but the tree snapshot failed, the block is present with null fields and a `warnings` entry names the failure.

## Global flags

These flags apply to every subcommand. The JSON equivalents are sibling keys of `action` and are stripped before `Action` is decoded.

| Flag | JSON key | Default | Meaning |
| --- | --- | --- | --- |
| `--pretty` | n/a | off | Pretty-print one-shot JSON. Serve mode stays one object per line. |
| `--json` | n/a | off | Read newline-delimited JSON actions from stdin. |
| `--app <NAME\|BUNDLE\|PID>` | `app` | frontmost on macOS | Target application. |
| `--window <TITLE\|INDEX\|ID>` | `window` | key window of `--app` | `index` is z-order; `0` is front. `id:<number>` selects by window id. |
| `--element <N>` | `element` | none | Index from the latest `tree` for this app and window. |
| `--require-visible` | `require_visible` | false | Fail when the element is outside the window and scroll-into-view cannot run. |
| `--diff` | `diff` | true once a baseline exists | Print a diff against the cached tree. |
| `--full` | `full` | off | Print the full tree. The baseline is always the most recent observation. |
| `--session <ID>` | `session` | process id | Cache namespace. |
| `--state` | `return_state` | false | Attach the post-action accessibility tree. |
| `--state-diff` | `state_diff` | false | Attach only the post-action delta. Implies `--state`. Omits `state.tree` and keeps `state.diff`. |
| `--screenshot` | `include_screenshot` | false | With `--state`, also attach a window image. |
| `--coord <screen\|image>` | `coord` | `screen` on the CLI, `image` inside adapters | Coordinate space for `x`/`y`. |
| `--adapter <native\|openai\|anthropic>` | `adapter` | `native` | Schema translator for `exec`, `serve`, and `--json`. |
| `--policy <PATH>` | n/a | `~/.config/computer-use/policy.toml` | User policy file. |
| `--confirm-cmd <COMMAND>` | `confirm_cmd` | env `COMPUTER_USE_CONFIRM_CMD` | Sensitive-action confirm hook. |
| `--tier <TIER>` | `tier` | auto | Input tier. |
| `--synthetic-focus` | sets `tier: "synthetic"` | off | Alias of `--tier synthetic`. Conflicts with `--tier`. |
| `--allow-foreground` | `allow_foreground` | false | Skip `foreground.lock` for tier `activate`. |
| `--require-background` | `require_background` | false | Do not fall back to `activate`. Cursor motion becomes `execution_failed`. |
| `--no-overlay` | `no_overlay` | false | Do not send the agent point to the overlay helper. |
| `--cdp` | `cdp` | off | Route `browser` commands through Chrome DevTools Protocol. The release binaries include the feature; a default `cargo install` needs `--features cdp`. |
| `--cdp-url <URL>` | `cdp_url` | argv discovery / managed session | Override the discovered CDP endpoint. |
| `--on-dialog <MODE>` | `on_dialog` | dismiss | Browser dialog policy: accept, dismiss, or fail. |
| `--agent-overlay <MODE>` | `agent_overlay` | auto | Browser agent marker: auto, on, or off. |
| `--display <ID\|VIRTUAL>` | `display` | all | Restrict capture and coordinate conversion to one monitor id from `displays`. `--display virtual` is the Linux Xvfb recipe. |
| `--exec <CMD>...` | n/a | none | With `--display virtual`, replace this process with a command inside the prepared environment (`DISPLAY=:99`). |

`--tier` values:

- `ax` — accessibility action or value. Does not activate the app.
- `event` — post events to the target process. Does not move the system cursor.
- `synthetic` — private SkyLight front-process call, then post events. Requires `--features skylight`.
- `activate` — activate the target, run foreground input, then restore the previous app.

## Top-level help

```
usage: computer-use [OPTIONS] [COMMAND]

Cross-platform desktop automation for agents.

Every action prints one JSON object on stdout: `{"ok":true,...}` or `{"ok":false,"error":{"code":"...","message":"..."}}`. Failed actions exit non-zero.

`computer-use serve` and `computer-use --json` read newline-delimited JSON requests from stdin and write one JSON response per line. `exec` runs one JSON action. Serve mode always prints compact JSON so each response stays on a single line.
```

`computer-use` lists these subcommands:

```
screenshot          Capture the screen, a region, or one window, and write a PNG or JPEG file
click               Move the pointer to a logical point and click, or press an element
triple-click        Click three times. Alias of `click --count 3` [alias: triple_click]
press               Press an element from the current tree. `--count` may be 1, 2, or 3
set-value           Set an element's value. Secure fields are refused [alias: set_value]
select-text         Select a character range. Secure fields are refused [alias: select_text]
perform-action      Perform an allowlisted accessibility action [alias: perform_action]
scroll-to           Scroll an element into view [alias: scroll-to-visible]
focus               Focus an element
move                Move the pointer without clicking
drag                Drag from the first point through the rest, then release
mouse-down          Press a mouse button and leave it down until `mouse-up` [alias: mouse_down]
mouse-up            Release a mouse button pressed by `mouse-down` [alias: mouse_up]
type                Type Unicode text into the focused control
key                 Press a key combination
hold-key            Hold one key combination, then release it [alias: hold_key]
clipboard           Read or write the clipboard
zoom                Screenshot one region. With `--app` or `--window`, the region is window-local and its far edge may equal the window size
scroll              Scroll the mouse wheel
get-state           Report displays, cursor, frontmost window, and a best-effort accessibility tree [alias: get_state]
wait                Sleep, or wait for one condition (element, text, value, window, tree change, app, title)
verify              Assert one condition with a short timeout. Failure is `verification_failed`
displays            List connected displays without capturing pixels
isolation           Report what this build can isolate and what it cannot
list-apps           List running applications [alias: list_apps]
list-windows        List on-screen windows in z-order. Index 0 is front; system windows are hidden unless --all [alias: list_windows]
launch              Launch an application by name, bundle id, path, or pid
activate            Bring an application forward
quit                Ask an application to quit
policy              Show whether policy allows an app. This does not perform an action
pause               Pause mutating actions until `resume`
resume              Clear the pause flag
doctor              Report permissions, the TCC identity, and how to fix what is missing
tree                Print the key window's accessibility tree
ocr                 Run OCR on the most recent capture for `--app` or `--window`, or on an image file
window              Move, resize, and inspect the front (or `--window`-selected) window
menu                Walk a running app's menu bar. macOS only
apple-event         Send an Apple event script via `osascript`. macOS only
shortcuts           Run a Shortcuts workflow by name. macOS only
dock                List or click Dock items. macOS only
spaces              Group on-screen windows by the screen that contains each window's center
audit               Audit log path and tail. Read-only
session             Session cache and accessibility teardown
serve               Read newline-delimited JSON actions from stdin and write one JSON response per line
exec                Run one JSON action from an argument
batch               Run a batch of JSON actions from an argument
cursor-position     Print the pointer position
overlay             Local agent-cursor helper. This is not a network service
browser             Chrome DevTools Protocol (CDP) subcommands. Requires `--cdp`; the release binaries include the feature
help                Print this message or the help of the given subcommand(s)
```

## Commands

### `screenshot`

```
usage: computer-use screenshot [OPTIONS]

Options:
      --output <PATH>        File to write. Defaults to a path in the system temp directory. Alias: --path.
      --region <X,Y,W,H>     Logical region `x,y,width,height` in the same space as click coordinates.
      --format <FORMAT>      png (default) or jpeg (`jpg` is accepted).
      --max-width <N>        Scale the image down so its width is at most N pixels. Default: 1440. 0 keeps the source width.
      --canvas <WxH>         Letterbox into WxH pixels, for example 1440x900. The image is not scaled up.
      --base64               Add a standard base64 `data` field to the JSON result.
      --marks                Stamp numbered element rectangles on the image (tree indices or OCR `oN`).
```

Example:

```bash
computer-use screenshot --output /tmp/screen.png --region 0,0,800,600
computer-use screenshot --format jpeg --max-width 1280 --base64
```

### `click`

```
usage: computer-use click [OPTIONS]

Options:
      --x <X>             Logical x coordinate. Required with --y when --element is omitted.
      --y <Y>             Logical y coordinate. Required with --x when --element is omitted.
      --button <BUTTON>   left (default), right, middle, back (button 4), forward (button 5).
      --double            Click twice. With --element, this presses twice.
      --count <COUNT>     Click count, 1, 2, or 3. --count 3 is a triple click. Default: 1.
```

### `triple-click`

```
usage: computer-use triple-click [OPTIONS] --x <X> --y <Y>

Options:
      --button <BUTTON>   left (default), right, middle.
```

### `press`

```
usage: computer-use press [OPTIONS]

Options:
      --count <COUNT>     How many times to press. Default: 1.
      --button <BUTTON>   left (default), right, middle, back (button 4), forward (button 5).
```

### `set-value`

```
usage: computer-use set-value [OPTIONS] --value <VALUE>

Options:
      --value <VALUE>     New value.
```

### `select-text`

```
usage: computer-use select-text [OPTIONS] --start <START> --end <END>

Options:
      --start <START>     First selected character.
      --end <END>         Character after the selection.
```

### `perform-action`

```
usage: computer-use perform-action [OPTIONS] --name <NAME>

Options:
      --name <NAME>       Allowlisted accessibility action name.
```

Allowed names (per platform allowlist):

- macOS: `AXPress`, `AXShowMenu`, `AXIncrement`, `AXDecrement`, `AXConfirm`, `AXCancel`, `AXRaise`, `AXScrollToVisible`, `AXScrollToVisibleTopLeft`, `AXScrollToVisibleBottomRight`, `AXScrollToVisibleLeft`, `AXScrollToVisibleRight`, `AXScrollPageUp`, `AXScrollPageDown`, `AXScrollPageLeft`, `AXScrollPageRight`.
- Windows: `press` (UIA `Invoke`), `expand`, `collapse` (UIA `ExpandCollapse`).
- Linux: any action from `Action::get_actions()` on the matched element; the first action is the default.

### `scroll-to`

```
usage: computer-use scroll-to [OPTIONS]
```

### `focus`

```
usage: computer-use focus [OPTIONS]
```

### `move`

```
usage: computer-use move [OPTIONS] --x <X> --y <Y>
```

### `drag`

```
usage: computer-use drag [OPTIONS] --path <X,Y,X,Y>

Options:
      --path <PATH>       At least two points, as `x,y,x,y` or a JSON array of `{x, y}`.
      --button <BUTTON>   left (default), right, middle.
```

### `mouse-down`

```
usage: computer-use mouse-down [OPTIONS] --x <X> --y <Y>

Options:
      --button <BUTTON>   left (default), right, middle.
```

### `mouse-up`

```
usage: computer-use mouse-up [OPTIONS] --x <X> --y <Y>

Options:
      --button <BUTTON>   left (default), right, middle.
```

### `type`

```
usage: computer-use type [OPTIONS] --text <TEXT>

Options:
      --text <TEXT>       Text to type.
```

### `key`

```
usage: computer-use key [OPTIONS] --keys <COMBO>

Options:
      --keys <COMBO>      Combination such as `super+c` or `ctrl+alt+Delete`.
```

Modifier tokens: `cmd`/`command`, `meta`/`super`, `ctrl`/`control`, `alt`/`option`, `shift`.
Named keys: `enter`/`return`, `tab`, `esc`/`escape`, `space`, `left`/`right`/`up`/`down` (and `arrow` aliases), `f1`–`f24`, `backspace`, `delete`, `home`, `end`, `pageup`/`pagedown` (and `page_up`/`page_down`), `insert`, `caps_lock`, `num_lock`, `scroll_lock`, `print_screen`, `menu`, plus xdotool keysyms (`super`, `hyper`, `meta`, `ISO_Level2_Latch`, etc.) on Linux.

### `hold-key`

```
usage: computer-use hold-key [OPTIONS] --keys <COMBO> --ms <N>

Options:
      --keys <COMBO>      One combo. Space-separated sequences are refused.
      --ms <N>            Milliseconds to hold (≤ 30000).
```

### `clipboard`

```
usage: computer-use clipboard [OPTIONS] <COMMAND>

Commands:
  get   Read text, or write a clipboard image to the session cache.
  set   Replace the clipboard. Requires a confirm hook.
```

`clipboard get`: returns `{ok: true, kind: "text", text: "..."}`, `{ok: true, kind: "image", path: "...", width, height}`, or `{ok: true, kind: "empty"}`. Text wins over an image.

`clipboard set` flags:

```
      --text <TEXT>        Text to write. Empty text is refused.
      --image <PNG>        PNG file to write. Used when --text is omitted.
```

### `zoom`

```
usage: computer-use zoom [OPTIONS] --region <X,Y,W,H>

Options:
      --region <X,Y,W,H>   Required. Logical region `x,y,width,height`.
      --output <PATH>      File to write. Defaults to a path in the system temp directory. Alias: --path.
      --format <FORMAT>    png (default) or jpeg.
      --max-width <N>      Scale the image down so its width is at most N pixels. Default: 1440.
      --canvas <WxH>       Letterbox into WxH pixels.
      --base64             Add a standard base64 `data` field to the JSON result.
```

Window-local coordinates when `--app`/`--window` is set; the far edge may equal the window size.

### `scroll`

```
usage: computer-use scroll [OPTIONS]

Options:
      --x <X>             Move the pointer to this logical x before scrolling.
      --y <Y>             Move the pointer to this logical y before scrolling.
      --dx <DX>           Horizontal wheel steps. Positive is right.
      --dy <DY>           Vertical wheel steps. Positive is down.
      --direction <DIR>   up | down | left | right (paired with --amount).
      --amount <N>        Wheel steps. Default: 1.
```

On macOS a posted wheel event is verified, not trusted: `CGEventPostToPid`
returns without error whether or not the app acted on it, and a non-frontmost
app does not act on it. The response therefore carries `via`, `scrolled`,
`before`, and `after`:

| Field | Meaning |
| --- | --- |
| `via` | `event` when the posted wheel event moved the scroll bar, `ax_scrollbar` when `AXValue` had to be written instead. |
| `scrolled` | `true` only when a scroll-bar value actually changed. |
| `before` / `after` | The scroll-bar value (0.0–1.0) on the primary axis — vertical when `--dy` is set, otherwise horizontal. `null` when no bar could be read. |

A bar that was already at its limit in the requested direction returns `ok`
with `scrolled: false` and a warning naming the edge. A scroll that cannot be
verified at all — no `AXScrollArea` under the point or in the window — is
`execution_failed`, never a silent success. `--tier ax` runs the scroll-bar
path directly and posts no event.

The step is an approximation: AX does not report how much of a document is on
screen, so one wheel line moves roughly a third of the scroll bar. Pass an
explicit `--amount` when you need a specific distance.

### `get-state`

```
usage: computer-use get-state [OPTIONS]
```

Reports `{platform, displays, screen, cursor, frontmost_app, frontmost_window, accessibility, note}`. Missing fields are `null` and `note` explains why. On macOS the cursor is read with `CGEventGetLocation`, which needs no input permission.

When `get-state` has no explicit `--app` and the frontmost app is on the deny list, the response stays `ok: true` with the metadata and a `policy` object (`decision: "deny"`), and the `tree` fields are `null`. `tree` itself stays `policy_denied` for a denied app.

### `wait`

```
usage: computer-use wait [OPTIONS]
```

Without a condition, `--ms` and `--seconds` are added when both are set and at least one is required. With a condition, `wait` polls until the condition holds or `--timeout-ms` expires (default 10000, maximum 300000); `--poll-ms` defaults to 100 (minimum 25).

Exactly one condition is accepted:

| Condition | Flags |
| --- | --- |
| Element state | `--element N` with `--element-state exists` (default), `gone`, `visible`, `enabled` |
| Text | `--text TEXT`, optionally `--within-element N` |
| Element value | `--value TEXT --element N` |
| Window | `--window-state exists\|gone` |
| Tree change | `--tree-change` (against the session baseline) |
| App running | `--app-running NAME` |
| Window title | `--title TEXT` |

`--ignore-case` relaxes text, value, title, and app comparisons. Timeout is `timeout`.

Element (`--element`) and value (`--value`) conditions need an accessibility backend: macOS and Windows implement them; Linux returns `unsupported` rather than guessing that an element is gone. `--text`, `--window-state`, `--tree-change`, `--app-running`, and `--title` work wherever `tree`, `list-windows`, and `list-apps` do.

### `verify`

```
usage: computer-use verify [OPTIONS]
```

`verify` takes the same condition flags as `wait` with a 2000 ms default timeout, and is meant as an assertion, not a sleep. Failure is the dedicated `verification_failed` code; the body carries `condition` and the last `observed` value. Example: `computer-use verify --app Notes --element 12 --element-state enabled`.

### `displays`

```
usage: computer-use displays [OPTIONS]
```

### `isolation`

```
usage: computer-use isolation [OPTIONS]
```

Returns `{platform, display_filter, virtual_display, set_of_marks, notes}`. In 0.4.0 it does not report `cdp`.

### `list-apps`

```
usage: computer-use list-apps [OPTIONS]

Options:
      --all    Include accessory and prohibited apps.
```

### `list-windows`

```
usage: computer-use list-windows [OPTIONS]
```

### `launch`

```
usage: computer-use launch [OPTIONS] [APP]
```

`APP` is a positional override for `--app`. macOS uses NSWorkspace. Other platforms return `unsupported` (the built-in deny list runs first).

### `activate`

```
usage: computer-use activate [OPTIONS]
```

Requires `--app`. `--window` is checked first.

### `quit`

```
usage: computer-use quit [OPTIONS]

Options:
      --force    Force-quit after the confirm hook allows it.
```

### `policy`

```
usage: computer-use policy [OPTIONS] <COMMAND>

Commands:
  check   Report allow or deny for `--app`.
```

`policy check` returns `{ok: true, decision: "allow" | "deny", source: "builtin" | "admin" | "user" | "default", code: "ok" | "policy_denied", ...}` without performing an action. Branch on `decision`; `code` repeats it as a machine-readable token.

A user policy file may exempt a specific built-in deny entry with a `[builtin_deny]` section. The admin file cannot set it.

```toml
version = 1

[builtin_deny]
exempt = ["app.crynta.terax"]
```

An exemption matches a bundle id, an executable name, or a `.app` bundle name, and covers the whole app.

Three optional sections tune the security and audit behavior:

```toml
[security]
secure_focus = "deny"        # or "warn": proceed when the focused element is a secure field
unreadable_focus = "warn"    # or "deny": refuse when the focused element cannot be read

[audit]
enabled = true
max_bytes = 10485760         # rotate at 10 MiB (minimum 1024); COMPUTER_USE_AUDIT_MAX_BYTES can raise it
keep = 1                     # rotated backups kept, clamped to 1..=5

[window]
clamp = true                 # keep window moves and resizes inside a display (default true)
```

`secure_focus` decides text-inserting actions without `--element`: the CLI reads the focused element first and refuses a secure field (`deny`, the default) or proceeds with a warning (`warn`). `unreadable_focus` covers platforms or apps that cannot report focus.

### `pause`

```
usage: computer-use pause [OPTIONS]
```

Cross-process kill switch on macOS (CGEventTap). Mutating actions return `paused` after this.

### `resume`

```
usage: computer-use resume [OPTIONS]
```

### `doctor`

```
usage: computer-use doctor [OPTIONS]

Options:
      --prompt             Prompt for Accessibility, Screen Recording, Post Event, and Input Monitoring. Alias: --request.
      --open-settings      Open System Settings to the first missing permission pane (macOS only).
```

`doctor` returns `{ok: true, permissions_ok: bool, missing: [...], fix: [...], fixes: [...], identity: {...}, ...}`. Envelope `ok: true` means the report was produced, so the process exits 0 even when a permission is missing. `fix` is an array parallel to `missing` — index `i` of `fix` describes `missing[i]` — and `fixes` carries the same entries as one-line strings.

### `tree`

```
usage: computer-use tree [OPTIONS]
```

Prints the numbered accessibility tree for the key window (one line per node, `[n] ROLE "title" @x,y wxh value=... actions=...`). Truncation caps: 40 children (`… +N`), depth 12, 400 lines, then `truncated=true`.

The response carries `baseline: "created" | "existing"`. `created` means this observation had no previous tree to compare against and the printed text is the whole tree; `existing` means the printed text is the delta against the previous observation.

A diff line starts with `+` for an added node, `-` for a removed one, and `~` for a changed value (`~ [9] AXTextField value: "old" -> "new"`).

### `ocr`

```
usage: computer-use ocr [OPTIONS]

Options:
      --force          Re-run OCR even when the latest screenshot map is recent.
      --image <PATH>   Run OCR on this image file instead of a live capture.
```

Source: `vision` on macOS (`VNRecognizeTextRequest`), `media_ocr` on Windows (`Windows.Media.Ocr.OcrEngine`). Linux returns `unsupported`.

`ocr --image` reads the file, reports `source: "image"`, `width`, and `height`, and sets `screen` to the block center in image pixels (a 1:1 map). Secure-field redaction does not apply to an arbitrary file, because there is no accessibility tree to redact against.

### `window`

```
usage: computer-use window [OPTIONS] <COMMAND>

Commands:
  move     Move the window to --x/--y, or center it on --to-display.
  resize   Resize the window.
  set      Set any combination of position and size.
  state    Read or set minimized/full-screen state.
  info     Report bounds, display, and status.
```

macOS reads and writes `AXPosition`/`AXSize` on the app's `AXWindows`, matched by title and frame. Windows and Linux return `unsupported` until their native layers land. The target comes from `--app` and `--window` like every other action.

```bash
computer-use window info --app Finder
computer-use window move --app Finder --x 100 --y 100
computer-use window move --app Finder --to-display 1
computer-use window resize --app Finder --width 900 --height 600
computer-use window set --app Finder --width 900          # position unchanged
computer-use window state --app Finder --minimized
computer-use window state --app Finder --restore
computer-use window state --app Finder --fullscreen
computer-use window state --app Finder                    # report only
```

Unless `[window] clamp = false`, a move or resize is clamped to the display that contains the requested center, so a window cannot be parked off-screen. `window move --to-display ID` centers the window on that display (ids come from `displays`).

### `menu`

```
usage: computer-use menu [OPTIONS] <COMMAND>

Commands:
  list    List the menu bar for one running app.
  click   Click one menu item by `">"`-separated titles.
```

`menu list` flags: `--app <APP>` (override of `--app`), `--depth <N>` (default 4, max 8).
`menu click` flags: `--app <APP>` (override), `--path <PATH>` (e.g. `File > Save As > Save`).

### `apple-event`

```
usage: computer-use apple-event [OPTIONS] --script <SCRIPT>

Options:
      --app <APP>             Optional override. Falls back to --app.
      --script <SCRIPT>       Script body passed to osascript -e.
      --language <LANGUAGE>   applescript (default) or javascript.
```

Script body is never logged verbatim; the audit line records `script_bytes` and a stable FNV-1a 64-bit hash. `apple-event` runs arbitrary code, so it is confirm-gated: without a confirm hook it returns `confirm_required`. The hook document carries `extra.script_bytes` and `extra.script_hash` so a hook can allowlist a specific script without seeing its body.

### `shortcuts`

```
usage: computer-use shortcuts [OPTIONS] <COMMAND>

Commands:
  run <NAME>    Run a Shortcuts workflow by name.
```

`shortcuts run` invokes `["shortcuts", "run", name]` with a 60 s timeout. A workflow can do anything, so it is confirm-gated like `apple-event`; the hook document carries `extra.name`.

### `dock`

```
usage: computer-use dock [OPTIONS] <COMMAND>

Commands:
  list    List the Dock items.
  click   Click a Dock item by title.
```

`dock click --title X` is case-insensitive.

### `spaces`

```
usage: computer-use spaces [OPTIONS]
```

Groups on-screen windows by the screen that contains each window's center. Mission Control moves are not attempted.

`spaces move --to-space N` always returns `unsupported`: moving a window between managed spaces needs the private SkyLight window-to-space API, which this build does not call. `window move --to-display` is the supported alternative.

### `audit`

```
usage: computer-use audit [OPTIONS] <COMMAND>

Commands:
  path   Print the audit log path.
  tail   Print the newest audit entries, including the rotated backup.
```

`audit path` returns `{path}` (null when no state directory is available). `audit tail --lines N` (default 50, maximum 500) returns `{lines, count}`, reading `audit.jsonl.1` before `audit.jsonl` so a rotation does not hide the newest entries. Both are read-only and never logged.

### `session`

```
usage: computer-use session [OPTIONS] <COMMAND>

Commands:
  end       Restore Electron accessibility flags this process changed and stop observers.
  prune     Remove empty session directories and stale ones.
```

`session prune` flags: `--dry-run`, `--ttl <TTL>` (default 86400 seconds, i.e. 24 hours).

### `serve`

```
usage: computer-use serve [OPTIONS]
```

Reads newline-delimited JSON requests from stdin and writes one JSON response per line. Stdin protocol, not a TCP server. Exits 0 on a clean end of input.

### `exec`

```
usage: computer-use exec [OPTIONS] <JSON>
```

Runs one JSON action from an argument. Honors `--pretty`. `--adapter` translates OpenAI/Anthropic shapes before dispatch.

### `batch`

```
usage: computer-use batch [OPTIONS] <JSON>
```

Runs a batch of JSON actions from an argument. The argument is a JSON object whose `action` field is `batch` and whose `actions` array holds the steps. Every step shares one target and one cache revision sequence, so element indices are re-validated between steps. A nested `batch` is rejected with `invalid_arguments`.

A step's own `return_state` and `state_diff` are ignored: the batch attaches one post-state at the end, shaped by the top-level flags.

### `cursor-position`

```
usage: computer-use cursor-position [OPTIONS]
```

Prints the pointer position. Alias: `cursor_position`. The same action is available in JSON as `cursor_position`. On macOS this uses `CGEventGetLocation` and needs no permission; off macOS it initializes the input backend.

### `overlay`

```
usage: computer-use overlay [OPTIONS] <COMMAND>

Commands:
  serve    Draw a click-through ring at points read from the overlay socket.
```

macOS only. The main CLI sends `{x, y, session}` on `<cache>/overlay.sock` after a successful positional action. A missing helper adds `overlay helper not running` and the action still succeeds. `--no-overlay` skips the socket.

### `browser` (CDP feature)

```
usage: computer-use browser [OPTIONS] <COMMAND>

Commands:
  tabs         List the targets the browser exposes. Filter by `kind == "page"`.
  activate     Bring a tab to the front. `--target` is required.
  navigate     Navigate a tab to `--url` with an optional wait.
  back         Go back in the tab history.
  forward      Go forward in the tab history.
  reload       Reload the tab.
  wait         Wait for `load`, `domcontentloaded`, `networkidle`, `selector:<css>`, or `url:<glob>`.
  snapshot     Capture a compact accessibility snapshot with `ref=eN` handles.
  click        Click by `--selector` or `--ref`.
  text         Read innerText by `--selector` or `--ref`.
  type         Type `--text` into `--selector`/`--ref`; secure fields are refused.
  press        Press a key combination on the page or an element.
  hover        Move the mouse over an element.
  scroll       Scroll the page or an element.
  select       Select an option by `--value` or `--label`.
  check        Check or uncheck a checkbox/radio (`--unchecked`).
  upload       Set files on an `<input type=file>` with `--file`.
  dialog       Accept or dismiss a JavaScript dialog.
  screenshot   Capture the page with `Page.captureScreenshot`.
  eval         Evaluate JavaScript in the page context.
  tab-new      Open a new tab.
  tab-close    Close a tab by `--target`.
  launch       Start a managed Chromium with an isolated profile.
  quit         Stop a managed Chromium (`--purge` removes the profile).
  sessions     List managed Chromium sessions.
  batch        Run several browser actions in one call.
```

Attach mode stays the default. `--cdp-url` wins; otherwise the port is
discovered from the `--app` pid's argv (macOS/Linux), and with no app the
newest `browser launch` session is used. `browser launch` is the reliable
path on Chrome 136+, which ignores `--remote-debugging-port` on the default
data directory.

Per-subcommand flags:

```
browser tabs
browser activate   --target <TARGET>
browser navigate   --url <URL> [--target <TARGET>] [--wait <WAIT>] [--timeout-ms <MS>] [--new-tab]
browser back|forward [--target <TARGET>]
browser reload     [--target <TARGET>] [--wait <WAIT>] [--timeout-ms <MS>]
browser wait       --wait <WAIT> [--target <TARGET>] [--timeout-ms <MS>]
browser snapshot   [--target <TARGET>] [--diff] [--interactive-only] [--max-nodes <N>]
browser click      (--selector <CSS>|--ref <REF>) [--target <TARGET>] [--button left|right|middle|back|forward] [--count N] [--wait <WAIT>] [--timeout-ms <MS>] [--force]
browser text       (--selector <CSS>|--ref <REF>) [--target <TARGET>]
browser type       --text <TEXT> (--selector <CSS>|--ref <REF>) [--clear] [--submit] [--wait <WAIT>] [--timeout-ms <MS>]
browser press      --keys <COMBO> [--selector <CSS>|--ref <REF>] [--target <TARGET>]
browser hover      (--selector <CSS>|--ref <REF>) [--target <TARGET>]
browser scroll     [--selector <CSS>|--ref <REF>] [--dx <N>] [--dy <N>]
browser select     (--selector <CSS>|--ref <REF>) (--value <V>|--label <L>)
browser check      (--selector <CSS>|--ref <REF>) [--unchecked]
browser upload     (--selector <CSS>|--ref <REF>) --file <PATH> [--file <PATH>...]
browser dialog     [--target <TARGET>] [--accept|--dismiss] [--prompt-text <TEXT>]
browser screenshot [--target <TARGET>] [--output <PATH>] [--full-page] [--format png|jpeg] [--quality <N>]
browser eval       --eval <SCRIPT> [--target <TARGET>] [--timeout-ms <MS>] [--await-promise]
browser tab-new    [--url <URL>]
browser tab-close  --target <TARGET>
browser launch     [--browser <NAME|PATH>] [--headless] [--profile <DIR>] [--port <PORT>] [--url <URL>]
browser quit       [--session <ID>] [--purge]
browser sessions
browser batch      '<JSON>' [--continue-on-error]
```

`--wait` accepts `none`, `load` (default), `domcontentloaded`, `networkidle`,
`selector:<css>`, or `url:<glob>`. `--timeout-ms` defaults to 30000.
`browser batch` takes a JSON array of native browser actions and runs them in
one process, so a whole flow is one call: `computer-use --cdp browser batch
'[{"action":"browser_navigate","url":"https://example.com"},{"action":"browser_snapshot"}]'`.

The global `--agent-overlay auto|on|off` controls the visible agent marker:
a badge, a cursor ring that follows click/type/hover/scroll, a tab title
prefixed with 🤖, and a robot favicon. `auto` is on for `browser launch` and off for an attach.

The CLI flag is `--cdp` and the JSON field is `cdp: true`. A binary built
without the feature returns `unsupported`; without `--cdp` the CLI returns
`invalid_arguments`.

## JSON actions

The protocol is `src/protocol.rs::Action`. The tag is `action`; the rename rule is `snake_case`. Aliases are accepted where the field/command accepts them (e.g. `move_mouse`, `set-value`, `scroll-to`, `browser-tabs`).

| Action name | Fields | Notes |
| --- | --- | --- |
| `screenshot` | `output`/`path`, `region` (`"x,y,w,h"` or `{x,y,width,height}`), `format`, `max_width`, `canvas`, `base64`, `marks` | Default `max_width` is 1440. |
| `click` | `x`, `y`, `button`, `double`, `count` | `count` 1, 2, or 3. |
| `double_click` | `x`, `y`, `button` | |
| `triple_click` | `x`, `y`, `button` | |
| `move` | `x`, `y` | Alias: `move_mouse`. |
| `press` | `count`, `button` | |
| `type` | `text` | |
| `key` | `keys` | |
| `scroll` | `x`, `y`, `dx`/`scroll_x`, `dy`/`scroll_y`, `direction`, `amount` | |
| `drag` | `path` (array or `x,y,x,y` string), `button` | |
| `mouse_down` | `x`, `y`, `button` | |
| `mouse_up` | `x`, `y`, `button` | |
| `hold_key` | `keys`, `ms` (≤ 30000) | |
| `zoom` | `region` (required), `output`/`path`, `format`, `max_width`, `canvas`, `base64` | |
| `clipboard_get` | none | |
| `clipboard_set` | `text`, `image` (PNG path) | |
| `focus` | none | |
| `set_value` | `value` | |
| `select_text` | `start`, `end` | |
| `perform_action` | `name` | |
| `scroll_to` | none | Aliases: `scroll-to`, `scroll_to_visible`, `scroll-to-visible`. |
| `get_state` | none | Alias: `get-state`. |
| `cursor_position` | none | Also reachable as the CLI command `cursor-position`. |
| `wait` | `ms`, `seconds`, or one condition: `element`, `element_state`, `text`, `value`, `within_element`, `window_state`, `tree_change`, `app_running`, `title`; plus `ignore_case`, `timeout_ms`, `poll_ms` | Without a condition it is the sleep wait. `timeout_ms` default 10000 (max 300000), `poll_ms` default 100 (min 25). |
| `verify` | same condition fields as `wait`; `timeout_ms` default 2000 | Failure is `verification_failed` and the body carries `condition` and the last `observed`. |
| `displays` | none | |
| `isolation` | none | |
| `list_apps` | `all`, `match` | Alias: `list-apps`. `match` accepts a substring or a `*` glob over name, bundle id, and path; rows carry `kind` and `system`. |
| `list_windows` | `all` | Alias: `list-windows`. System windows are hidden by default; the response adds `system_hidden` and a `note`, and rows carry `kind` and `system`. Index `0` and `--window N` always resolve app windows; use `id:` for a system row. |
| `launch` | none (uses `--app`) | |
| `activate` | none (uses `--app`/`--window`) | |
| `quit` | `force` | |
| `policy_check` | none (uses `--app`) | |
| `pause` | none | |
| `resume` | none | |
| `doctor` | `prompt` (alias `request`), `open_settings`, `x11`, `full` | `full` validates notarization with `xcrun stapler validate` and reports `notarized`. |
| `tree` | none | |
| `ocr` | `force`, `image` | `image` reads a file; `screen` equals the image pixel and `source` is `"image"`. |
| `session_end` | none | |
| `session_prune` | `dry_run`, `ttl_secs` | |
| `audit_path` | none | Alias: `audit-path`. `{path}` or null. |
| `audit_tail` | `lines` | Alias: `audit-tail`. Default 50, maximum 500. Reads the rotated backup first. |
| `window_move` | `x`, `y`, `to_display` | Alias: `window-move`. macOS only. `to_display` centers on that display. |
| `window_resize` | `width`, `height` | Alias: `window-resize`. macOS only. |
| `window_set` | `x`, `y`, `width`, `height` | Alias: `window-set`. At least one field. macOS only. |
| `window_state` | `minimized`, `fullscreen` | Alias: `window-state`. No fields reports. macOS only. |
| `window_info` | none | Alias: `window-info`. Bounds, display, and status. macOS only. |
| `menu_list` | `app` (override), `depth` | Aliases: `menu-list`, `menu_list`. |
| `menu_click` | `app` (override), `path` | Aliases: `menu-click`, `menu_click`. |
| `apple_event` | `app` (override), `script`, `language` | Aliases: `apple_event`, `apple-event`. |
| `shortcuts_run` | `name` | Aliases: `shortcuts-run`, `shortcuts_run`. |
| `dock_list` | none | Aliases: `dock-list`, `dock_list`. |
| `dock_click` | `title` | Aliases: `dock-click`, `dock_click`. |
| `spaces` | none | |
| `spaces_move` | `to_space` | Alias: `spaces-move`. Always `unsupported`; the private SkyLight window-to-space API is not implemented. Use `window move --to-display`. |
| `batch` | `actions` (array of native actions), `stop_on_error` | Nested batch is `invalid_arguments`. Also reachable as the CLI command `batch`. |
| `browser_tabs` | none | `cdp` feature. Returns `{tabs, count}`. |
| `browser_activate` | `target_id` | `cdp` feature. |
| `browser_navigate` | `url`, `target_id`, `wait`, `timeout_ms`, `new_tab` | `cdp` feature. |
| `browser_back` | `target_id` | `cdp` feature. |
| `browser_forward` | `target_id` | `cdp` feature. |
| `browser_reload` | `target_id`, `wait`, `timeout_ms` | `cdp` feature. |
| `browser_wait` | `target_id`, `wait`, `timeout_ms` | `cdp` feature. |
| `browser_snapshot` | `target_id`, `diff`, `interactive_only`, `max_nodes` | `cdp` feature. |
| `browser_click` | `selector`/`ref_id`, `target_id`, `button`, `count`, `wait`, `timeout_ms`, `force` | `cdp` feature. |
| `browser_text` | `selector`/`ref_id`, `target_id` | `cdp` feature. |
| `browser_type` | `text`, `selector`/`ref_id`, `target_id`, `clear`, `submit`, `wait`, `timeout_ms` | `cdp` feature. |
| `browser_press` | `keys`, `selector`/`ref_id`, `target_id` | `cdp` feature. |
| `browser_hover` | `selector`/`ref_id`, `target_id` | `cdp` feature. |
| `browser_scroll` | `selector`/`ref_id`, `target_id`, `dx`, `dy` | `cdp` feature. |
| `browser_select` | `selector`/`ref_id`, `target_id`, `value`, `label` | `cdp` feature. |
| `browser_check` | `selector`/`ref_id`, `target_id`, `checked` | `cdp` feature. |
| `browser_upload` | `selector`/`ref_id`, `target_id`, `files` | `cdp` feature. |
| `browser_dialog` | `target_id`, `accept`, `prompt_text` | `cdp` feature. |
| `browser_screenshot` | `target_id`, `output`, `full_page`, `format`, `quality`, `base64` | `cdp` feature. |
| `browser_eval` | `script`, `target_id`, `timeout_ms`, `await_promise` | `cdp` feature. |
| `browser_tab_new` | `url` | `cdp` feature. |
| `browser_tab_close` | `target_id` | `cdp` feature. |
| `browser_launch` | `browser`, `headless`, `profile`, `port`, `url` | `cdp` feature. |
| `browser_quit` | `session`, `purge` | `cdp` feature. |
| `browser_sessions` | none | `cdp` feature. |
| `browser_batch` | `actions`, `stop_on_error` | `cdp` feature. |

`adapter` values:

- `native` — pass through.
- `openai` — translate OpenAI `computer` tool actions (click, double_click, scroll, type, keypress, move, wait, screenshot, back, forward, drag, hold_key, zoom, cursor_position). Sets `coord=image`.
- `anthropic` — translate Anthropic `computer_20251124` actions (left/right/middle click, double_click, mouse_move, scroll, type, key, wait, zoom, cursor_position, triple_click, left_click_drag, mouse_down, mouse_up, hold_key). Sets `coord=image`.

## Error codes

`ComputerError::code()` is the stable contract. The CLI also writes the same string into the `error.code` field of every failed response.

| Code | When |
| --- | --- |
| `invalid_arguments` | Bad CLI flags, malformed JSON, or a missing required field. |
| `execution_failed` | Framework error that is not in this table. |
| `io_error` | stdin/stdout/filesystem failure. |
| `unsupported` | Off-platform or feature-off. |
| `app_not_found` | Name, bundle id, or pid matched nothing running, and launch was not requested. |
| `window_not_found` | App resolved, window did not. |
| `element_not_found` | Index missing in the cached or fresh tree, or no session cache. |
| `stale_element` | Path's role, identifier, or frame (beyond 2 points) changed, or the cached pid, window, or revision header does not match. Action not performed. Body includes the new tree. |
| `permission_required` | Accessibility or Screen Recording is off. |
| `policy_denied` | Built-in or configured deny. |
| `confirm_required` | Sensitive action and no confirm hook is configured. |
| `confirm_denied` | Hook exited non-zero or timed out. |
| `blocked_url` | Browser URL matched the block list. |
| `paused` | Local user input tripped the kill switch. |
| `cdp_unavailable` | Feature off, or Chrome has no debugging port. |
| `display_unavailable` | The screen is locked, the display is asleep, or no display or shareable window is available. Checked before `window_not_found`; no xcap crop fallback. |
| `timeout` | A `wait`, browser wait, action, or dialog rescue exceeded its timeout. |
| `verification_failed` | `verify` did not observe its condition before its timeout. The body carries `condition` and the last `observed`. |

## File locations

| Item | macOS / Linux | Windows |
| --- | --- | --- |
| Session cache | `${XDG_CACHE_HOME:-~/.cache}/computer-use/sessions/<session>/` | `%LOCALAPPDATA%\computer-use\cache\sessions\<session>\` |
| User policy | `~/.config/computer-use/policy.toml` | `~/.config/computer-use/policy.toml` |
| Admin policy | `/etc/computer-use/policy.toml` | `%ProgramData%\computer-use\policy.toml` |
| Audit log | `${XDG_STATE_HOME:-~/.local/state}/computer-use/audit.jsonl` (rotated backups `.1`..`.5`) | `%LOCALAPPDATA%\computer-use\audit.jsonl` |
| Foreground lock | `<cache>/foreground.lock` | `<cache>\foreground.lock` |
| Overlay socket | `<cache>/overlay.sock` | `<cache>\overlay.sock` |

Cache files are JSON, mode `0600` on Unix. They hold the last revision, the rendered tree, the structural nodes, and the last screenshot map. They do not hold secure-field values.

## CLI/library parity

Every JSON action has a CLI command. The reverse is also true: every CLI command listed in this document has a matching `Action` variant or runs through `dispatch` (for `serve`, `exec`, `overlay`, `browser`'s feature gate, `policy_check`, `pause`, `resume`). Library callers use `dispatch(Action::...)` and read the `Response`; CLI callers use the subcommands or the `--adapter` translations.

## Test that the doc matches the binary

`tests/cli_doc_matches_help.rs` parses this Markdown for action names and CLI subcommands, then asserts each one is reachable on a fresh build via the protocol enum or a `clap` invocation. Run it with `cargo test --test cli_doc_matches_help`.

## Note for the smart-computer-use repo

The agent skill at <https://github.com/lindoelio/smart-computer-use> ships its own `references/cli-reference.md`. That file mirrors the protocol names but is in a different repo and is regenerated there. This document is the only source of truth for `computer-use` itself.