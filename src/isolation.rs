//! P14 — display filtering and the honest isolation matrix.
//!
//! Three different things live here, and they are deliberately not the same
//! thing:
//!
//! 1. `--display <id>` restricts capture and coordinate conversion to one
//!    monitor from [`DisplayInfo`]. That is real, works on all three platforms
//!    through [`crate::backend::capture`], and is the only part that changes
//!    what the agent can see.
//! 2. `--display virtual` on Linux prints the Xvfb recipe and, with `--exec`,
//!    runs a prepared command with `DISPLAY` set. It never starts a server on
//!    its own.
//! 3. macOS `CGVirtualDisplay` is probed behind the `virtual-display` feature
//!    and reported as **experimental**. It is system-visible, it is not a
//!    sandbox, and this build never creates one.
//!
//! Nothing here starts a hypervisor, a compositor, or an X server. See
//! `docs/ISOLATION.md` for what an operator can set up instead.

use serde_json::{json, Value};

use crate::error::ComputerError;
use crate::types::{DisplayInfo, Region};

/// `--display virtual`: a separate X server, prepared by the operator.
pub const VIRTUAL_SELECTOR: &str = "virtual";

/// Display the Xvfb recipe prepares, as `DISPLAY=:99`.
pub const XVFB_DISPLAY: &str = ":99";

/// Screen geometry the recipe asks Xvfb for.
pub const XVFB_GEOMETRY: &str = "1440x900x24";

/// What `--display` selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplaySelector {
    /// One monitor id from `list_displays`.
    Id(u32),
    /// The Linux Xvfb recipe. Never a capture target on its own.
    Virtual,
}

/// One monitor the rest of the action is confined to.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayTarget {
    /// The monitor, exactly as `list_displays` reported it.
    pub display: DisplayInfo,
    /// The monitor's rectangle in virtual-desktop points. This is also the
    /// capture region, so the screenshot map origin is the monitor origin.
    pub region: Region,
}

impl DisplayTarget {
    /// One-pixel helper for error messages.
    pub fn bounds(&self) -> String {
        format!(
            "{}x{}+{}+{}",
            self.region.width, self.region.height, self.region.x, self.region.y
        )
    }

    /// Every id the caller could have meant, for an unknown-id error.
    pub fn known_ids(displays: &[DisplayInfo]) -> String {
        displays
            .iter()
            .map(|display| display.id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Parse `--display`. `virtual` is matched exactly, case-insensitively.
pub fn parse_selector(spec: &str) -> Result<DisplaySelector, ComputerError> {
    let trimmed = spec.trim();
    if trimmed.eq_ignore_ascii_case(VIRTUAL_SELECTOR) {
        return Ok(DisplaySelector::Virtual);
    }
    trimmed
        .parse::<u32>()
        .map(DisplaySelector::Id)
        .map_err(|_| {
            ComputerError::InvalidArguments(format!(
            "display must be a monitor id from list_displays or `{VIRTUAL_SELECTOR}` (got {spec})"
        ))
        })
}

/// Resolve a numeric `--display` against the live display list.
///
/// `Virtual` never reaches here: it is handled by the CLI, because exec'ing a
/// prepared X server is not something a JSON request can do. A JSON caller that
/// asks for it gets `unsupported`, not a silent whole-desktop capture.
pub fn resolve(spec: &str, displays: &[DisplayInfo]) -> Result<DisplayTarget, ComputerError> {
    match parse_selector(spec)? {
        DisplaySelector::Id(id) => find(displays, id).ok_or_else(|| {
            let known = DisplayTarget::known_ids(displays);
            if known.is_empty() {
                ComputerError::InvalidArguments(format!(
                    "display {id} was not found: no display is reported by this platform"
                ))
            } else {
                ComputerError::InvalidArguments(format!(
                    "display {id} was not found; list_displays reports {known}"
                ))
            }
        }),
        DisplaySelector::Virtual => Err(ComputerError::Unsupported(
            "display `virtual` prepares an environment and is not a capture target; \
             run `computer-use --display virtual` on Linux to print the Xvfb recipe"
                .into(),
        )),
    }
}

/// Find one monitor by id.
pub fn find(displays: &[DisplayInfo], id: u32) -> Option<DisplayTarget> {
    displays
        .iter()
        .find(|display| display.id == id)
        .and_then(display_target)
}

/// Build the target for one monitor. A zero-sized monitor is not a target.
pub fn display_target(display: &DisplayInfo) -> Option<DisplayTarget> {
    if display.width == 0 || display.height == 0 {
        return None;
    }
    let region = Region::new(display.x, display.y, display.width, display.height).ok()?;
    Some(DisplayTarget {
        display: display.clone(),
        region,
    })
}

/// True when the virtual-desktop point is inside the monitor.
///
/// The right and bottom edges are exclusive, so `(x + width, y)` belongs to the
/// next monitor and not to this one.
pub fn point_inside(target: &DisplayTarget, x: i32, y: i32) -> bool {
    let (rx, ry, rw, rh) = (
        i64::from(target.region.x),
        i64::from(target.region.y),
        i64::from(target.region.width),
        i64::from(target.region.height),
    );
    let (px, py) = (i64::from(x), i64::from(y));
    px >= rx && py >= ry && px < rx + rw && py < ry + rh
}

/// Reject a point that the chosen monitor does not cover.
pub fn ensure_point_inside(target: &DisplayTarget, x: i32, y: i32) -> Result<(), ComputerError> {
    if point_inside(target, x, y) {
        return Ok(());
    }
    Err(ComputerError::InvalidArguments(format!(
        "point {x},{y} is outside display {} ({} covers {})",
        target.display.id,
        target.display.name,
        target.bounds()
    )))
}

/// Reject a region that is not fully covered by the chosen monitor.
///
/// `--display` is a hard filter, not a crop hint: a region that hangs off the
/// monitor is an argument error rather than a silently smaller capture.
pub fn ensure_region_inside(target: &DisplayTarget, region: Region) -> Result<(), ComputerError> {
    if region_inside(target, region) {
        return Ok(());
    }
    Err(ComputerError::InvalidArguments(format!(
        "region {},{},{}x{} is not inside display {} ({} covers {})",
        region.x,
        region.y,
        region.width,
        region.height,
        target.display.id,
        target.display.name,
        target.bounds()
    )))
}

/// True when `region` fits entirely inside the monitor.
pub fn region_inside(target: &DisplayTarget, region: Region) -> bool {
    let (rx, ry, rw, rh) = (
        i64::from(target.region.x),
        i64::from(target.region.y),
        i64::from(target.region.width),
        i64::from(target.region.height),
    );
    let x = i64::from(region.x);
    let y = i64::from(region.y);
    let x2 = x + i64::from(region.width);
    let y2 = y + i64::from(region.height);
    x >= rx && y >= ry && x2 <= rx + rw && y2 <= ry + rh
}

/// True when the window rectangle is fully covered by the chosen monitor.
pub fn window_inside(target: &DisplayTarget, bounds: crate::target::WindowBounds) -> bool {
    region_inside(
        target,
        Region {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        },
    )
}

// ---------------------------------------------------------------------------
// The Linux Xvfb recipe
// ---------------------------------------------------------------------------

/// The Xvfb command line the recipe documents and `docs/ISOLATION.md` repeats.
pub fn xvfb_command() -> String {
    format!("Xvfb {XVFB_DISPLAY} -screen 0 {XVFB_GEOMETRY}")
}

/// Environment overrides applied by `--display virtual --exec`.
pub fn prepared_env() -> Vec<(&'static str, String)> {
    vec![("DISPLAY", XVFB_DISPLAY.to_string())]
}

/// The recipe as JSON: the exact commands and what this binary does with them.
///
/// `starts_server` is always false. `--display virtual` prepares an environment
/// and never spawns Xvfb, so an agent cannot leave a stray X server behind or
/// race a second one on `:99`.
pub fn xvfb_recipe() -> Value {
    json!({
        "method": "xvfb",
        "platform": "linux",
        "display": XVFB_DISPLAY,
        "geometry": XVFB_GEOMETRY,
        "server_command": xvfb_command(),
        "client_prefix": format!("DISPLAY={XVFB_DISPLAY}"),
        "example": "computer-use --display virtual --exec -- computer-use screenshot",
        "starts_server": false,
        "env": prepared_env()
            .into_iter()
            .map(|(key, value)| json!({"key": key, "value": value}))
            .collect::<Vec<_>>(),
        "notes": [
            "Start Xvfb yourself, in another terminal. --display virtual never starts one.",
            "When DISPLAY already points at a separate server, that server is the isolation; no new protocol is involved.",
            "The existing X11 backend does the capture and the input. Nothing new is needed.",
        ],
    })
}

/// Split `--exec` arguments, dropping a leading `--` separator.
///
/// `computer-use --display virtual --exec -- env A=1 computer-use tree` should
/// exec `env`, not `--`. Clap usually strips the separator, but not every
/// arrangement does, so this makes both spellings behave the same.
pub fn exec_arguments(raw: &[String]) -> Vec<String> {
    match raw.first() {
        Some(first) if first == "--" => raw[1..].to_vec(),
        _ => raw.to_vec(),
    }
}

/// Replace this process with `command`, running inside the prepared
/// environment. Never returns on success.
#[cfg(target_os = "linux")]
pub fn exec_prepared(command: &[String]) -> Result<std::convert::Infallible, ComputerError> {
    use std::os::unix::process::CommandExt;

    if command.is_empty() {
        return Err(ComputerError::InvalidArguments(
            "--exec needs a command; pass it after `--`, for example \
             `--display virtual --exec -- computer-use screenshot`"
                .into(),
        ));
    }
    let mut child = std::process::Command::new(&command[0]);
    child.args(&command[1..]);
    for (key, value) in prepared_env() {
        child.env(key, value);
    }
    // `exec` replaces this process and only returns when it failed, so the
    // `Infallible` error type is correct: there is no success branch.
    let error = child.exec();
    Err(ComputerError::ExecutionFailed(format!(
        "could not execute {} in the prepared environment: {error}",
        command[0]
    )))
}

/// `--display virtual` is Linux-only.
#[cfg(not(target_os = "linux"))]
pub fn exec_prepared(_command: &[String]) -> Result<std::convert::Infallible, ComputerError> {
    Err(ComputerError::Unsupported(format!(
        "--display virtual needs an X server and only Linux has a supported recipe \
         (current platform: {}); see docs/ISOLATION.md",
        platform()
    )))
}

// ---------------------------------------------------------------------------
// The isolation matrix
// ---------------------------------------------------------------------------

/// `target_os` as the short name this crate uses everywhere else.
pub const fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// True when this platform has a supported `--display virtual` recipe.
///
/// Linux only: Xvfb needs an X server, and macOS and Windows have no supported
/// equivalent here. `docs/ISOLATION.md` explains what exists instead.
pub fn virtual_supported() -> bool {
    cfg!(target_os = "linux")
}

/// True when this binary was built with `--features virtual-display`.
pub fn virtual_display_compiled() -> bool {
    cfg!(feature = "virtual-display")
}

/// What the private macOS symbol probe found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VirtualDisplayProbe {
    /// `CGVirtualDisplayDescriptorCreate` resolved.
    pub descriptor_create: bool,
    /// `CGVirtualDisplayCreate` resolved.
    pub display_create: bool,
    /// What this build would do with those symbols.
    pub action: &'static str,
    /// Why the action is what it is.
    pub note: String,
}

impl VirtualDisplayProbe {
    /// Both symbols resolved.
    pub fn available(&self) -> bool {
        self.descriptor_create && self.display_create
    }

    /// `experimental` when the symbols are there, `unsupported` otherwise.
    pub fn status(&self) -> &'static str {
        if self.available() {
            "experimental"
        } else {
            "unsupported"
        }
    }
}

#[cfg(all(target_os = "macos", feature = "virtual-display"))]
fn probe_virtual_display() -> VirtualDisplayProbe {
    use self::virtual_display::resolve_symbols;
    let symbols = resolve_symbols();
    let available = symbols.descriptor_create && symbols.display_create;
    VirtualDisplayProbe {
        descriptor_create: symbols.descriptor_create,
        display_create: symbols.display_create,
        action: "probe only",
        note: if available {
            "CGVirtualDisplayDescriptorCreate and CGVirtualDisplayCreate resolved in \
             CoreGraphics. The descriptor is an Objective-C object whose remaining \
             properties need private class setup this build does not do, so no display \
             is ever created here. The symbols are system-visible: TCC, the menu bar, \
             and input focus stay shared with the host session. This is not isolation."
                .into()
        } else {
            "CGVirtualDisplay is not exported by this CoreGraphics, so macOS reports \
             `unsupported` for the experimental virtual display."
                .into()
        },
    }
}

#[cfg(not(all(target_os = "macos", feature = "virtual-display")))]
fn probe_virtual_display() -> VirtualDisplayProbe {
    let (descriptor_create, display_create, note) = if cfg!(target_os = "macos") {
        (
            false,
            false,
            "this binary was built without `--features virtual-display`, so the private \
             CoreGraphics symbols are never probed",
        )
    } else if cfg!(feature = "virtual-display") {
        (
            false,
            false,
            "CGVirtualDisplay is a macOS private API; the feature compiles on every target \
             and only probes on macOS",
        )
    } else {
        (
            false,
            false,
            "macOS CGVirtualDisplay is unavailable on this platform",
        )
    };
    VirtualDisplayProbe {
        descriptor_create,
        display_create,
        action: "none",
        note: note.into(),
    }
}

/// Run the macOS symbol probe. Cheap enough for `isolation` on every call.
pub fn probe() -> VirtualDisplayProbe {
    probe_virtual_display()
}

/// Everything that is not built, with the reason. Keys and wording mirror
/// `docs/ISOLATION.md`; the snapshot test in this module is what keeps them
/// from drifting apart.
pub fn infeasible() -> Value {
    json!({
        "macos_vm_per_agent": "operator setup, not shipped: run this binary in a UTM or Virtualization.framework guest and pipe JSON lines in over SSH or a virtio socket",
        "share_host_app_into_guest": "infeasible: a host application cannot be driven from inside a guest",
        "hide_virtual_display_from_user": "infeasible with public APIs: a created CGVirtualDisplay is composited by WindowServer and visible to the user",
        "windows_virtual_display": "not implemented here: use a Windows VM or Windows Sandbox running the same binary",
        "nested_wayland_input_without_portal": "infeasible by design of the XDG RemoteDesktop portal: a grant is required",
    })
}

/// The JSON body of `computer-use isolation`.
pub fn matrix() -> Value {
    let probe = probe();
    let cg_status = if cfg!(target_os = "macos") {
        probe.status()
    } else {
        "unsupported"
    };
    json!({
        "platform": platform(),
        "display_filter": true,
        "display_filter_note": "--display <id> restricts capture and coordinate conversion to one monitor from list_displays; points stay virtual-desktop points",
        "xvfb": "linux only",
        "xvfb_recipe": xvfb_recipe(),
        "cg_virtual_display": cg_status,
        "cg_virtual_display_detail": probe,
        "vm": "documented, not shipped",
        "vm_note": "docs/ISOLATION.md describes the JSON-over-SSH proxy; the crate does not grow a hypervisor dependency",
        "infeasible": infeasible(),
    })
}

#[cfg(all(target_os = "macos", feature = "virtual-display"))]
mod virtual_display {
    //! Private CoreGraphics symbol probe for `CGVirtualDisplay`.
    //!
    //! This resolves two symbols and stops there. It never calls them. Creating
    //! a display needs an Objective-C `CGVirtualDisplayDescriptor` configured
    //! through private class setup that this build does not perform, so the
    //! honest outcome is a probe result plus `experimental`, not a stub that
    //! claims to have made a display.

    #![allow(unsafe_code)]

    use std::ffi::{c_void, CStr};

    /// `CGVirtualDisplayDescriptorCreate` — returns a `CGVirtualDisplayDescriptor *`.
    pub(super) type DescriptorCreate = unsafe extern "C" fn() -> *mut c_void;

    /// `CGVirtualDisplayCreate(descriptor, out_display_id)`.
    pub(super) type DisplayCreate = unsafe extern "C" fn(*const c_void, *mut u32) -> i32;

    const CORE_GRAPHICS: &CStr = c"/System/Library/Frameworks/CoreGraphics.framework/CoreGraphics";

    /// Which symbols CoreGraphics exports.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Symbols {
        pub(super) descriptor_create: bool,
        pub(super) display_create: bool,
    }

    /// Look both symbols up. Never calls them.
    ///
    /// `dlopen` is required rather than `RTLD_DEFAULT`: the global namespace
    /// only searches images already loaded, and a plain CLI need not have
    /// CoreGraphics mapped. Reporting "not exported" because the framework was
    /// never opened would be a wrong answer, not an honest one.
    pub(super) fn resolve_symbols() -> Symbols {
        // The function pointer aliases above document the signatures. The probe
        // only needs the addresses, and never calls through them.
        let _descriptor: Option<DescriptorCreate> = None;
        let _create: Option<DisplayCreate> = None;

        // SAFETY: the path is a NUL-terminated literal for a system framework
        // that is always present on macOS. RTLD_LAZY resolves nothing until a
        // symbol is used, and the handle is closed before returning.
        let handle = unsafe { libc::dlopen(CORE_GRAPHICS.as_ptr(), libc::RTLD_LAZY) };
        if handle.is_null() {
            return Symbols {
                descriptor_create: false,
                display_create: false,
            };
        }
        let found = Symbols {
            descriptor_create: symbol(handle, c"CGVirtualDisplayDescriptorCreate"),
            display_create: symbol(handle, c"CGVirtualDisplayCreate"),
        };
        // SAFETY: `handle` came from dlopen above and is closed exactly once.
        unsafe { libc::dlclose(handle) };
        found
    }

    fn symbol(handle: *mut c_void, name: &CStr) -> bool {
        // SAFETY: `handle` is a live dlopen handle owned by the caller. dlsym
        // returns null for a name the framework does not export, which is the
        // only thing this reports; nothing is called through the result.
        !unsafe { libc::dlsym(handle, name.as_ptr()) }.is_null()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_displays() -> Vec<DisplayInfo> {
        vec![
            DisplayInfo {
                id: 1,
                name: "Left".into(),
                x: 0,
                y: 0,
                width: 1000,
                height: 600,
                scale: 2.0,
                is_primary: true,
            },
            DisplayInfo {
                id: 2,
                name: "Right".into(),
                x: 1000,
                y: 200,
                width: 800,
                height: 400,
                scale: 1.0,
                is_primary: false,
            },
        ]
    }

    #[test]
    fn parses_ids_and_the_virtual_keyword() {
        assert_eq!(parse_selector("2").unwrap(), DisplaySelector::Id(2));
        assert_eq!(parse_selector(" 1 ").unwrap(), DisplaySelector::Id(1));
        assert_eq!(parse_selector("virtual").unwrap(), DisplaySelector::Virtual);
        assert_eq!(parse_selector("VIRTUAL").unwrap(), DisplaySelector::Virtual);
        for bad in ["", "left", "-1", "1.5", "id:1", "99999999999999999999"] {
            let error = parse_selector(bad).unwrap_err();
            assert_eq!(error.code(), "invalid_arguments", "{bad}");
        }
    }

    #[test]
    fn unknown_id_is_invalid_arguments_and_lists_what_exists() {
        let error = resolve("7", &fake_displays()).unwrap_err();
        assert_eq!(error.code(), "invalid_arguments");
        assert!(error.to_string().contains("list_displays reports 1, 2"));

        let error = resolve("3", &[]).unwrap_err();
        assert_eq!(error.code(), "invalid_arguments");
        assert!(error.to_string().contains("no display is reported"));
    }

    #[test]
    fn virtual_is_unsupported_in_the_json_path() {
        let error = resolve("virtual", &fake_displays()).unwrap_err();
        assert_eq!(error.code(), "unsupported");
    }

    #[test]
    fn rejects_a_point_outside_the_chosen_monitor() {
        let target = resolve("2", &fake_displays()).unwrap();
        // Inside the second monitor, which does not start at the origin.
        assert!(ensure_point_inside(&target, 1000, 200).is_ok());
        assert!(ensure_point_inside(&target, 1799, 599).is_ok());

        // Every one of these is on display 1, not on the filtered monitor.
        for (x, y) in [(0, 0), (999, 199), (1800, 200), (1000, 199), (1000, 600)] {
            let error = ensure_point_inside(&target, x, y).unwrap_err();
            assert_eq!(error.code(), "invalid_arguments", "{x},{y}");
        }
    }

    #[test]
    fn the_right_and_bottom_edges_are_exclusive() {
        let target = resolve("1", &fake_displays()).unwrap();
        assert!(point_inside(&target, 999, 599));
        assert!(!point_inside(&target, 1000, 599));
        assert!(!point_inside(&target, 999, 600));
        // The neighbouring monitor owns those points, and it passes its own filter.
        let other = resolve("2", &fake_displays()).unwrap();
        assert!(point_inside(&other, 1000, 200));
    }

    #[test]
    fn the_map_origin_is_the_monitor_origin() {
        let target = resolve("2", &fake_displays()).unwrap();
        assert_eq!((target.region.x, target.region.y), (1000, 200));
        assert_eq!((target.region.width, target.region.height), (800, 400));
        assert_eq!(target.bounds(), "800x400+1000+200");
    }

    #[test]
    fn a_region_must_fit_inside_the_monitor() {
        let target = resolve("2", &fake_displays()).unwrap();
        assert!(ensure_region_inside(&target, Region::new(1000, 200, 800, 400).unwrap()).is_ok());
        assert!(ensure_region_inside(&target, Region::new(1000, 200, 801, 400).unwrap()).is_err());
        assert!(ensure_region_inside(&target, Region::new(999, 200, 800, 400).unwrap()).is_err());
        let error = ensure_region_inside(&target, Region::new(0, 0, 10, 10).unwrap()).unwrap_err();
        assert_eq!(error.code(), "invalid_arguments");
        assert!(error.to_string().contains("800x400+1000+200"));
    }

    #[test]
    fn a_window_must_fit_inside_the_monitor() {
        let target = resolve("1", &fake_displays()).unwrap();
        let inside = crate::target::WindowBounds {
            x: 10,
            y: 10,
            width: 100,
            height: 100,
        };
        let straddling = crate::target::WindowBounds {
            x: 990,
            y: 10,
            width: 100,
            height: 100,
        };
        assert!(window_inside(&target, inside));
        assert!(!window_inside(&target, straddling));
    }

    #[test]
    fn a_zero_sized_monitor_is_not_a_target() {
        let mut broken = fake_displays();
        broken[0].width = 0;
        assert!(find(&broken, 1).is_none());
    }

    #[test]
    fn exec_arguments_drop_the_separator() {
        assert_eq!(
            exec_arguments(&["--".into(), "env".into(), "A=1".into()]),
            vec!["env".to_string(), "A=1".to_string()]
        );
        assert_eq!(
            exec_arguments(&["env".into(), "A=1".into()]),
            vec!["env".to_string(), "A=1".to_string()]
        );
        assert!(exec_arguments(&["--".into()]).is_empty());
    }

    #[test]
    fn the_recipe_documented_is_the_recipe_returned() {
        let recipe = xvfb_recipe();
        assert_eq!(recipe["server_command"], "Xvfb :99 -screen 0 1440x900x24");
        assert_eq!(recipe["display"], ":99");
        assert_eq!(recipe["starts_server"], false);
        assert_eq!(recipe["env"][0]["key"], "DISPLAY");
        assert_eq!(recipe["env"][0]["value"], ":99");
    }

    /// Snapshot of the matrix. `docs/ISOLATION.md` mirrors these keys.
    #[test]
    fn isolation_matrix_is_a_stable_snapshot() {
        let matrix = matrix();
        let infeasible = infeasible();

        assert_eq!(matrix["platform"], platform());
        assert_eq!(matrix["display_filter"], true);
        assert_eq!(matrix["xvfb"], "linux only");
        assert_eq!(matrix["vm"], "documented, not shipped");
        assert_eq!(matrix["cg_virtual_display"], probe().status());
        assert_eq!(
            matrix["cg_virtual_display_detail"]["action"],
            probe().action
        );

        // The five infeasible keys, exactly. `serde_json` orders object keys
        // alphabetically, so the documented order is compared as a set.
        let mut expected = vec![
            "macos_vm_per_agent",
            "share_host_app_into_guest",
            "hide_virtual_display_from_user",
            "windows_virtual_display",
            "nested_wayland_input_without_portal",
        ];
        expected.sort_unstable();
        let mut actual: Vec<String> = infeasible
            .as_object()
            .expect("object")
            .keys()
            .cloned()
            .collect();
        actual.sort();
        assert_eq!(actual, expected);
        for key in &expected {
            assert!(
                infeasible[*key].as_str().expect("reason").len() > 20,
                "{key} needs a reason"
            );
        }
        assert_eq!(matrix["infeasible"], infeasible);
    }

    #[test]
    fn the_virtual_display_claim_never_outruns_the_probe() {
        let matrix = matrix();
        let detail = &matrix["cg_virtual_display_detail"];
        let resolved = detail["descriptor_create"].as_bool().unwrap_or(false)
            && detail["display_create"].as_bool().unwrap_or(false);
        let probed = cfg!(all(target_os = "macos", feature = "virtual-display"));

        // What the build did: it probed, or it did not.
        assert_eq!(detail["action"], if probed { "probe only" } else { "none" });
        if !probed {
            assert!(!resolved);
        }

        // `experimental` is claimed only when both symbols really resolved.
        // A macOS that stopped exporting them reports `unsupported`, which is
        // the documented fallback and not a failure.
        if resolved {
            assert_eq!(matrix["cg_virtual_display"], "experimental");
            assert!(
                detail["note"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("not isolation"),
                "the note must not describe this as isolation"
            );
        } else {
            assert_eq!(matrix["cg_virtual_display"], "unsupported");
            assert!(
                detail["note"].as_str().unwrap_or_default().len() > 20,
                "an unsupported claim needs a reason"
            );
        }
        if !cfg!(target_os = "macos") {
            assert!(!resolved);
            assert_eq!(matrix["cg_virtual_display"], "unsupported");
        }
        // Whatever the probe found, nothing was created.
        assert_eq!(detail["action"], if probed { "probe only" } else { "none" });
    }
}
