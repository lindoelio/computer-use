# Isolation

What `computer-use` can isolate, what it cannot, and what an operator sets up instead.

The machine-readable half of this page is `computer-use isolation`. The two are kept
from drifting by a snapshot test on the JSON object.

```console
$ computer-use isolation
```

## Three different things

They are not the same thing and this page does not blur them.

| Thing | Shipped | Reality |
| --- | --- | --- |
| `--display <id>` | yes | Restricts capture and coordinate conversion to one monitor. Real, on all three platforms. |
| `--display virtual` (Xvfb) | recipe | A separate X server, prepared by you. The binary never starts one. |
| macOS `CGVirtualDisplay` | probe only | Private API, **experimental**, system-visible. Not isolation. |

## 1. `--display <id>`

Restricts the action to one monitor from `computer-use displays`.

```console
$ computer-use displays
{"displays":[{"id":1,"name":"Built-in Retina Display","x":0,"y":0,"width":1728,"height":1117,"scale":2.0,"is_primary":true}],"ok":true}

$ computer-use --display 1 screenshot --output /tmp/monitor-1.png
{"width":1440,"height":932,"scale":2.0,"origin":{"x":0,"y":0},...,"ok":true}
```

- **Points stay virtual-desktop points.** `--display` narrows what may be addressed; it does
  not rebase coordinates. On a two-monitor desktop the second monitor's points are its own
  virtual-desktop points, not `0,0`-relative.
- **The screenshot map origin is the monitor's origin.** The map is computed from the
  filtered capture, so `origin` is that monitor's top-left, not the desktop's.
- **A point outside the monitor is `invalid_arguments`,** and nothing is performed. The right
  and bottom edges are exclusive.
- An id that no display reports is `invalid_arguments`. The message lists what `displays` does
  report.
- A `--region` or `--window` target that does not fit inside the filtered monitor is
  `invalid_arguments`. A window capture is not redirected onto another monitor.
- `displays` and `isolation` never consume the filter — they are how you find out the ids.

This is a filter, not a sandbox. It changes what this binary captures and addresses. It does
not change what the user sees, and another process on the same session is unaffected.

## 2. Linux virtual display (Xvfb)

The one supported recipe that is actually a separate seat. Xvfb has no window manager and no
real devices, so an agent driving it cannot touch the human's session.

Print the recipe:

```console
$ computer-use --display virtual
```

Start the server yourself, in another terminal:

```console
$ Xvfb :99 -screen 0 1440x900x24 &
```

Then run this binary against it:

```console
$ DISPLAY=:99 computer-use displays
$ DISPLAY=:99 computer-use screenshot --output /tmp/xvfb.png
```

Or let `computer-use` set the environment for you:

```console
$ computer-use --display virtual --exec -- computer-use screenshot --output /tmp/xvfb.png
```

`--exec` replaces the current process with your command and sets `DISPLAY=:99`. It does not
start Xvfb. That is deliberate: an agent must not leave a stray X server behind or race a
second one on `:99`. If the command cannot be executed you get `execution_failed`.

When `DISPLAY` already points at a separate server, that server *is* the isolation. The
existing X11 backend does the capture and the input. No new protocol is involved, and nothing
in `computer-use` changes.

On macOS and Windows, `--display virtual` is `unsupported`: there is no supported recipe.

### Nested Wayland compositor

A nested compositor on a private `WAYLAND_DISPLAY` is a manual setup, not something this crate
automates. Never touch the host compositor.

```console
$ XDG_RUNTIME_DIR=/run/user/$(id -u) cage --wl-display=wayland-9 -- sh -c 'computer-use --app Firefox tree'
```

Replace `cage` with `labwc` if you want panels and a keybinder. Both keep the nested session on
its own socket, so the host's windows are not in it.

The XDG Desktop Portal dialog still appears **inside** that session — the ScreenCast dialog and
the RemoteDesktop grant both render into the nested compositor, not onto the host display.
Grant them once per session.

Input without a portal grant is impossible: that is the design of the portal. See
`--features portal` and the `portal` field in `computer-use doctor` for whether this build has
it.

## 3. macOS `CGVirtualDisplay` — experimental, and not isolation

Behind the off-by-default `virtual-display` feature. This build resolves two private CoreGraphics
symbols and **stops there**:

- `CGVirtualDisplayDescriptorCreate`
- `CGVirtualDisplayCreate`

```console
$ computer-use --features virtual-display isolation
```

`cg_virtual_display` is `experimental` only when both symbols resolved, and `unsupported`
otherwise. The probe never calls them.

Creating the display would need an Objective-C `CGVirtualDisplayDescriptor` configured through
private class setup that this build does not perform, so there is no command that creates one.
A stub that claimed to work would be worse than an honest `unsupported`, so that is what you
get. This is a probe, not a feature.

If you are here looking for a sandbox, stop reading. Even a *created* virtual display would not
be one:

- **It is system-visible.** WindowServer composites it and the user sees it. It is not a
  per-process sandbox.
- **TCC is shared.** Screen Recording permission granted to this binary covers the virtual
  display too. There is no separate grant.
- **The menu bar is shared.** It is the host session's menu bar.
- **Input focus is shared.** Foreground app, keyboard focus, and Spaces are host-session state.

## 4. The VM proxy — documented, not shipped

The real answer on macOS and Windows is a virtual machine, and it is an *operator* setup, not a
feature of this crate. The crate does not grow a hypervisor dependency.

One page, the whole thing: the guest runs the same binary, and the host pipes JSON lines in.
The host does not run `computer-use serve` anywhere; it forwards stdin.

On the guest, inside the VM:

```console
$ computer-use serve
```

On the host, one long-lived process:

```console
$ ssh -T guest@vm.local 'computer-use serve' <<'EOF'
{"id":1,"action":"screenshot","output":"/tmp/inside-the-vm.png"}
{"id":2,"action":"tree","app":"Firefox"}
EOF
```

Each line in is one JSON line out, on the same stdout, in order. `ssh -T` matters: no TTY, so
nothing the binary prints can corrupt the stream. A Unix socket or a virtio channel works the
same way and avoids re-authenticating per request.

The guest's TCC prompts, its own displays, and its own input are all the guest's. That is the
boundary — and it is the whole reason to do this instead of the virtual display above.

## What is infeasible in 0.2.0

These five are the `infeasible` object in `computer-use isolation`.

| Key | Why |
| --- | --- |
| `macos_vm_per_agent` | Feasible as an operator setup (UTM or `Virtualization.framework` guest running this binary, JSON lines piped over SSH or a virtio socket). Not built: no hypervisor dependency. |
| `share_host_app_into_guest` | Infeasible. A host application cannot be driven from inside a guest. |
| `hide_virtual_display_from_user` | Infeasible with public APIs. A created `CGVirtualDisplay` is composited by WindowServer and visible to the user. |
| `windows_virtual_display` | Not implemented here. The documented equivalent is a Windows VM or Windows Sandbox running the same binary. UI Automation against the host from a sandbox is a different trust boundary and is not implemented. |
| `nested_wayland_input_without_portal` | Infeasible by design of the XDG RemoteDesktop portal: a grant is required. |