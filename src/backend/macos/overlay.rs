//! Click-through agent cursor.
//!
//! `computer-use overlay serve` opens one borderless window and reads
//! `{x,y,session}` lines from the overlay socket. The ring is not the system
//! cursor. Unit tests do not compile this file, so they never create a window.

#![allow(unsafe_code)]

use std::io::{BufRead, BufReader};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBezierPath, NSColor,
    NSScreen, NSStatusWindowLevel, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{kCFRunLoopDefaultMode, CFRunLoop, CGFloat};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize};

use crate::error::ComputerError;
use crate::overlay::{self, parse_overlay_line};

struct AgentPoint {
    x: f64,
    y: f64,
    hue: CGFloat,
    visible: bool,
}

static AGENT: Mutex<AgentPoint> = Mutex::new(AgentPoint {
    x: 0.0,
    y: 0.0,
    hue: 0.0,
    visible: false,
});

struct RingGeometry {
    primary_height: f64,
    origin_x: f64,
    origin_y: f64,
}

define_class!(
    // SAFETY: NSView is the documented superclass. RingView adds no Drop glue.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = RingGeometry]
    struct RingView;

    // SAFETY: NSObjectProtocol has no extra requirements.
    unsafe impl NSObjectProtocol for RingView {}

    impl RingView {
        #[unsafe(method(isOpaque))]
        fn is_opaque(&self) -> bool {
            false
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            paint_ring(self.ivars());
        }
    }
);

impl RingView {
    fn make(mtm: objc2::MainThreadMarker, frame: NSRect, geometry: RingGeometry) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(geometry);
        // SAFETY: initWithFrame: is NSView's initializer. `this` is a fresh allocation.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }
}

/// Open the helper window and read the socket until the process exits.
pub(crate) fn serve() -> Result<(), ComputerError> {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return Err(ComputerError::ExecutionFailed(
            "overlay serve must run on the main thread".into(),
        ));
    };
    let app = NSApplication::sharedApplication(mtm);
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let frame = union_frame(mtm);
    let geometry = RingGeometry {
        primary_height: primary_height(mtm),
        origin_x: frame.origin.x,
        origin_y: frame.origin.y,
    };
    let view_frame = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
    let view = RingView::make(mtm, view_frame, geometry);
    // SAFETY: the window is borderless, buffered, and created on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    window.setOpaque(false);
    window.setHasShadow(false);
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setIgnoresMouseEvents(true);
    window.setLevel(NSStatusWindowLevel);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    window.setContentView(Some(&view));
    let path = crate::cache::cache_dir().join("overlay.sock");
    let listener = overlay::bind_socket(&path).map_err(ComputerError::Io)?;
    thread::Builder::new()
        .name("computer-use-overlay".into())
        .spawn(move || accept_points(listener))
        .map_err(ComputerError::Io)?;
    window.orderFrontRegardless();
    loop {
        // SAFETY: `kCFRunLoopDefaultMode` is a process-lifetime Apple constant.
        let mode = unsafe { kCFRunLoopDefaultMode };
        let _ = CFRunLoop::run_in_mode(mode, 0.05, false);
        view.setNeedsDisplay(true);
    }
}

fn accept_points(listener: std::os::unix::net::UnixListener) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        while reader
            .read_line(&mut line)
            .ok()
            .filter(|count| *count > 0)
            .is_some()
        {
            if let Ok(point) = parse_overlay_line(line.as_bytes()) {
                let mut agent = AGENT.lock().unwrap_or_else(|poison| poison.into_inner());
                agent.x = f64::from(point.x);
                agent.y = f64::from(point.y);
                agent.hue = overlay::session_hue(&point.session) as CGFloat / 360.0;
                agent.visible = true;
            }
            line.clear();
        }
    }
}

fn paint_ring(geometry: &RingGeometry) {
    let agent = AGENT.lock().unwrap_or_else(|poison| poison.into_inner());
    if !agent.visible {
        return;
    }
    let appkit_x = agent.x;
    let appkit_y = geometry.primary_height - agent.y;
    let local_x = appkit_x - geometry.origin_x;
    let local_y = appkit_y - geometry.origin_y;
    let hue = agent.hue;
    drop(agent);
    let rect = NSRect::new(
        NSPoint::new(local_x - 8.0, local_y - 8.0),
        NSSize::new(16.0, 16.0),
    );
    let color = NSColor::colorWithHue_saturation_brightness_alpha(hue, 0.75, 0.95, 1.0);
    color.setStroke();
    let path = NSBezierPath::bezierPathWithOvalInRect(rect);
    path.setLineWidth(2.0);
    path.stroke();
}

fn union_frame(mtm: objc2::MainThreadMarker) -> NSRect {
    let screens = NSScreen::screens(mtm);
    if screens.count() == 0 {
        return NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(800.0, 600.0));
    }
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for index in 0..screens.count() {
        let frame = screens.objectAtIndex(index).frame();
        min_x = min_x.min(frame.origin.x);
        min_y = min_y.min(frame.origin.y);
        max_x = max_x.max(frame.origin.x + frame.size.width);
        max_y = max_y.max(frame.origin.y + frame.size.height);
    }
    NSRect::new(
        NSPoint::new(min_x, min_y),
        NSSize::new(max_x - min_x, max_y - min_y),
    )
}

fn primary_height(mtm: objc2::MainThreadMarker) -> f64 {
    let screens = NSScreen::screens(mtm);
    for index in 0..screens.count() {
        let frame = screens.objectAtIndex(index).frame();
        if frame.origin.x == 0.0 && frame.origin.y == 0.0 {
            return frame.size.height;
        }
    }
    if screens.count() == 0 {
        800.0
    } else {
        screens.objectAtIndex(0).frame().size.height
    }
}
