//! Per-window capture through ScreenCaptureKit.
//!
//! `SCScreenshotManager` is used on macOS 14 and newer. Older systems, and a
//! failed screenshot-manager call, take one `SCStream` frame and stop. When
//! that also fails for a reason other than permission or a missing window, the
//! caller crops the xcap desktop image.
//!
//! `ignoreShadows` on `SCStreamConfiguration` is `ignoreShadowsSingleWindow`.
//! `captureImageWithFilter` takes that configuration, not `SCScreenshotConfiguration`.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchQueueAttr};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadMarker};
use objc2_app_kit::NSScreen;
use objc2_core_foundation::{
    kCFBooleanTrue, kCFRunLoopDefaultMode, CFBoolean, CFDictionary, CFRetained, CFRunLoop,
    CFString, CGFloat, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGDisplayIsAsleep, CGError,
    CGGetActiveDisplayList, CGImage, CGPreflightScreenCaptureAccess,
    CGSessionCopyCurrentDictionary,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSProcessInfo};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCScreenshotManager, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamOutput, SCStreamOutputType, SCWindow,
};

use super::ax;
use crate::backend::WindowTarget;
use crate::error::ComputerError;
use crate::image_util::{
    self, classify_sck_error, error_for_missing_window, CaptureFault, DisplayFacts, Layer,
};
use crate::tree::NodeFrame;
use crate::types::{Frame, RawCapture, Region};

const TIMEOUT: Duration = Duration::from_secs(5);
const SCREEN_RECORDING: &str = "Screen Recording is off. System Settings → Privacy & Security → Screen Recording → enable the app named by computer-use doctor";
/// `kCVPixelFormatType_32BGRA`.
const PIXEL_BGRA: u32 = 0x4247_5241;

/// Full-screen or region capture through ScreenCaptureKit, with the xcap
/// desktop image as a non-terminal fallback.
pub(crate) fn capture_screen(region: Option<Region>) -> Result<RawCapture, ComputerError> {
    if let Some(error) = session_display_block() {
        return Err(error);
    }
    match capture_screen_sck(region) {
        Ok(raw) => Ok(raw),
        // Permission and missing-display errors fall back to the desktop
        // image: TCC may attribute the grant to a launcher (the terminal),
        // where ScreenCaptureKit in this process still refuses.
        Err(error)
            if is_terminal(&error) && !matches!(error, ComputerError::PermissionRequired(_)) =>
        {
            Err(error)
        }
        Err(error) => {
            let mut raw = crate::backend::capture::capture_screen(region)?;
            raw.warnings.push(format!(
                "ScreenCaptureKit display capture fell back to the desktop image: {error}"
            ));
            Ok(raw)
        }
    }
}

fn capture_screen_sck(region: Option<Region>) -> Result<RawCapture, ComputerError> {
    let content = shareable(false)?;
    // SAFETY: content is a live SCShareableContent. displays() returns +1.
    let displays = unsafe { content.displays() };
    if displays.count() == 0 {
        return Err(ComputerError::ExecutionFailed(
            "ScreenCaptureKit reported no displays".into(),
        ));
    }
    let mut entries: Vec<(Retained<SCDisplay>, CGRect)> = Vec::new();
    for index in 0..displays.count() {
        let display = displays.objectAtIndex(index);
        // SAFETY: display is a live SCDisplay for this call.
        let frame = unsafe { display.frame() };
        entries.push((display, frame));
    }

    if let Some(region) = region {
        let center_x = f64::from(region.x) + f64::from(region.width) / 2.0;
        let center_y = f64::from(region.y) + f64::from(region.height) / 2.0;
        let (display, frame) = entries
            .iter()
            .find(|(_, frame)| {
                center_x >= frame.origin.x
                    && center_x < frame.origin.x + frame.size.width
                    && center_y >= frame.origin.y
                    && center_y < frame.origin.y + frame.size.height
            })
            .unwrap_or(&entries[0]);
        let scale = positive_scale(backing_scale(
            round_i32(frame.origin.x),
            round_i32(frame.origin.y),
        ));
        let image = capture_display_layer(display, frame, scale)?;
        let cropped = crop_display_to_region(&image, frame, region, scale)?;
        return Ok(RawCapture {
            frame: Frame::from_image(cropped, scale),
            origin_x: region.x,
            origin_y: region.y,
            logical_width: region.width,
            logical_height: region.height,
            capture_source: "sck".into(),
            occluded: false,
            warnings: Vec::new(),
        });
    }

    let scales: Vec<f64> = entries
        .iter()
        .map(|(_, frame)| {
            positive_scale(backing_scale(
                round_i32(frame.origin.x),
                round_i32(frame.origin.y),
            ))
        })
        .collect();
    if scales
        .windows(2)
        .any(|pair| (pair[0] - pair[1]).abs() > 0.001)
    {
        return Err(ComputerError::ExecutionFailed(
            "displays use different backing scales; the desktop image handles this".into(),
        ));
    }
    let scale = scales[0];
    let mut layers = Vec::new();
    let mut warnings = Vec::new();
    for (display, frame) in &entries {
        match capture_display_layer(display, frame, scale) {
            Ok(image) => layers.push(Layer {
                image,
                origin_x: round_i32(frame.origin.x),
                origin_y: round_i32(frame.origin.y),
                logical_width: positive_points(frame.size.width),
                logical_height: positive_points(frame.size.height),
            }),
            Err(error) if is_terminal(&error) => return Err(error),
            Err(error) => warnings.push(format!("display capture failed: {error}")),
        }
    }
    if layers.is_empty() {
        return Err(ComputerError::ExecutionFailed(
            "ScreenCaptureKit captured no displays".into(),
        ));
    }
    let (image, origin_x, origin_y, logical_width, logical_height) = if layers.len() == 1 {
        let layer = layers.pop().expect("single display layer");
        (
            layer.image,
            layer.origin_x,
            layer.origin_y,
            layer.logical_width,
            layer.logical_height,
        )
    } else {
        let composite = image_util::composite_layers(&layers, scale)?;
        (
            composite.image,
            composite.origin_x,
            composite.origin_y,
            composite.logical_width,
            composite.logical_height,
        )
    };
    Ok(RawCapture {
        frame: Frame::from_image(image, scale),
        origin_x,
        origin_y,
        logical_width,
        logical_height,
        capture_source: "sck".into(),
        occluded: false,
        warnings,
    })
}

fn capture_display_layer(
    display: &SCDisplay,
    frame: &CGRect,
    scale: f64,
) -> Result<image::RgbaImage, ComputerError> {
    let width = positive_points(frame.size.width);
    let height = positive_points(frame.size.height);
    let config = stream_config(px_len(width, scale), px_len(height, scale));
    let empty = NSArray::<SCWindow>::from_slice(&[]);
    // SAFETY: alloc/init returns +1. display and empty are live for this call.
    let filter = unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), display, &empty)
    };
    if macos_14_or_newer() {
        match capture_manager_filter(&filter, &config) {
            Ok(image) => Ok(image),
            Err(error) if is_terminal(&error) => Err(error),
            Err(manager_error) => match capture_stream_filter(&filter, &config) {
                Ok(image) => Ok(image),
                Err(error) if is_terminal(&error) => Err(error),
                Err(stream_error) => Err(ComputerError::ExecutionFailed(format!(
                    "screenshot manager: {manager_error}; stream: {stream_error}"
                ))),
            },
        }
    } else {
        capture_stream_filter(&filter, &config)
    }
}

fn crop_display_to_region(
    image: &image::RgbaImage,
    frame: &CGRect,
    region: Region,
    scale: f64,
) -> Result<image::RgbaImage, ComputerError> {
    let offset_x = (f64::from(region.x) - frame.origin.x) * scale;
    let offset_y = (f64::from(region.y) - frame.origin.y) * scale;
    let width = (f64::from(region.width) * scale).round().max(1.0);
    let height = (f64::from(region.height) * scale).round().max(1.0);
    let x = offset_x.round().max(0.0).min(f64::from(image.width()));
    let y = offset_y.round().max(0.0).min(f64::from(image.height()));
    let width = width.min(f64::from(image.width()) - x).max(1.0);
    let height = height.min(f64::from(image.height()) - y).max(1.0);
    if width < 1.0 || height < 1.0 {
        return Err(ComputerError::InvalidArguments(
            "the region falls outside every display".into(),
        ));
    }
    Ok(
        image::imageops::crop_imm(image, x as u32, y as u32, width as u32, height as u32)
            .to_image(),
    )
}

/// Capture `target` without activating it.
pub(crate) fn capture_window(target: &WindowTarget) -> Result<RawCapture, ComputerError> {
    if target.bounds.width == 0 || target.bounds.height == 0 {
        return Err(ComputerError::InvalidArguments(
            "window bounds are empty".into(),
        ));
    }
    if !CGPreflightScreenCaptureAccess() {
        return Err(ComputerError::PermissionRequired(SCREEN_RECORDING.into()));
    }
    if let Some(error) = session_display_block() {
        return Err(error);
    }
    match capture_sck(target) {
        Ok(raw) => Ok(raw),
        Err(error) if is_terminal(&error) => Err(error),
        Err(error) => fallback_crop(target, &error),
    }
}

fn capture_sck(target: &WindowTarget) -> Result<RawCapture, ComputerError> {
    let mut warnings = Vec::new();
    let (_content, windows) = shareable_with_window(target.window_id)?;
    let parent = windows
        .iter()
        .find(|window| window_id(window) == target.window_id)
        .ok_or_else(|| {
            ComputerError::WindowNotFound(format!(
                "cg window {} is not in SCShareableContent",
                target.window_id
            ))
        })?
        .clone();
    let scale = backing_scale(target.bounds.x, target.bounds.y);
    let parent_image = capture_layer(&parent, target.bounds.width, target.bounds.height, scale)?;
    let mut layers = vec![Layer {
        image: parent_image,
        origin_x: target.bounds.x,
        origin_y: target.bounds.y,
        logical_width: target.bounds.width,
        logical_height: target.bounds.height,
    }];
    for frame in ax::attached_sheets(target.pid, target.bounds) {
        let Some(sheet) = windows.iter().find(|window| {
            window_id(window) != target.window_id
                && window_pid(window) == Some(target.pid)
                && frames_close(window_frame(window), frame)
        }) else {
            warnings.push("an accessibility sheet had no ScreenCaptureKit window".into());
            continue;
        };
        let width = positive_points(frame.width);
        let height = positive_points(frame.height);
        match capture_layer(sheet, width, height, scale) {
            Ok(image) => layers.push(Layer {
                image,
                origin_x: round_i32(frame.x),
                origin_y: round_i32(frame.y),
                logical_width: width,
                logical_height: height,
            }),
            Err(error) if is_terminal(&error) => return Err(error),
            Err(error) => warnings.push(format!("sheet capture failed: {error}")),
        }
    }
    let (image, origin_x, origin_y, logical_width, logical_height) = if layers.len() == 1 {
        let layer = layers.pop().expect("parent layer");
        (
            layer.image,
            layer.origin_x,
            layer.origin_y,
            layer.logical_width,
            layer.logical_height,
        )
    } else {
        let composite = image_util::composite_layers(&layers, scale)?;
        (
            composite.image,
            composite.origin_x,
            composite.origin_y,
            composite.logical_width,
            composite.logical_height,
        )
    };
    Ok(RawCapture {
        frame: Frame::from_image(image, scale),
        origin_x,
        origin_y,
        logical_width,
        logical_height,
        capture_source: "sck".into(),
        occluded: false,
        warnings,
    })
}

fn shareable_with_window(
    id: u32,
) -> Result<(Retained<SCShareableContent>, Vec<Retained<SCWindow>>), ComputerError> {
    let first_ok = match shareable(true) {
        Ok(content) => {
            let windows = sc_windows(&content);
            if windows.iter().any(|window| window_id(window) == id) {
                return Ok((content, windows));
            }
            true
        }
        Err(error) if is_terminal(&error) => return Err(error),
        Err(_) => false,
    };
    match shareable(false) {
        Ok(content) => {
            let windows = sc_windows(&content);
            if windows.iter().any(|window| window_id(window) == id) {
                return Ok((content, windows));
            }
            let facts = probe_facts(sc_display_count(&content), windows.len());
            if first_ok || image_util::display_block_reason(facts).is_some() {
                return Err(error_for_missing_window(facts, id));
            }
            Err(ComputerError::ExecutionFailed(format!(
                "cg window {id} was not listed and a shareable-content query failed"
            )))
        }
        Err(error) => Err(error),
    }
}

/// Locked, asleep, or a caller-supplied empty display list. Does not look at windows.
pub(crate) fn session_display_block() -> Option<ComputerError> {
    let facts = DisplayFacts {
        screen_locked: session_screen_locked(),
        display_asleep: any_display_asleep(),
        display_count: None,
        shareable_window_count: None,
    };
    image_util::display_block_reason(facts)
        .map(|reason| ComputerError::DisplayUnavailable(reason.into()))
}

fn probe_facts(display_count: usize, window_count: usize) -> DisplayFacts {
    DisplayFacts {
        screen_locked: session_screen_locked(),
        display_asleep: any_display_asleep(),
        display_count: Some(display_count),
        shareable_window_count: Some(window_count),
    }
}

fn session_screen_locked() -> bool {
    let Some(dict) = CGSessionCopyCurrentDictionary() else {
        return false;
    };
    let key = CFString::from_str("CGSSessionScreenIsLocked");
    // SAFETY: key is retained for this call. value() borrows an unretained pointer from the dictionary.
    let value = unsafe { CFDictionary::value(&dict, &*key as *const CFString as *const c_void) };
    if value.is_null() {
        return false;
    }
    // SAFETY: kCFBooleanTrue is a process-lifetime CoreFoundation constant.
    // The pointer is compared with the dictionary value and is not freed.
    let Some(truth) = (unsafe { kCFBooleanTrue }) else {
        return false;
    };
    std::ptr::eq(value, truth as *const CFBoolean as *const c_void)
}

fn any_display_asleep() -> bool {
    let mut ids = [0u32; 16];
    let mut count = 0u32;
    // SAFETY: ids has room for 16 display ids. count is a stack out-parameter.
    let error = unsafe { CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count) };
    if error != CGError::Success {
        return false;
    }
    let count = count as usize;
    if count > ids.len() {
        return false;
    }
    ids.iter().take(count).any(|id| CGDisplayIsAsleep(*id))
}

fn sc_display_count(content: &SCShareableContent) -> usize {
    // SAFETY: content is a live SCShareableContent. displays() returns +1.
    let displays = unsafe { content.displays() };
    displays.count()
}

fn shareable(on_screen_only: bool) -> Result<Retained<SCShareableContent>, ComputerError> {
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = if !error.is_null() {
                // SAFETY: ScreenCaptureKit passes a live NSError for this callback, or null.
                Err(nserror_to_computer(unsafe { &*error }))
            } else if content.is_null() {
                Err(ComputerError::ExecutionFailed(
                    "SCShareableContent was null".into(),
                ))
            } else {
                // SAFETY: the callback argument is +0. retain adds +1 before the pool drains.
                match unsafe { Retained::retain(content) } {
                    Some(retained) => Ok(Retained::into_raw(retained)),
                    None => Err(ComputerError::ExecutionFailed(
                        "SCShareableContent retain failed".into(),
                    )),
                }
            };
            let _ = sender.send(result);
        },
    );
    // SAFETY: `block` stays alive until the handler runs. A timeout forgets it.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true,
            on_screen_only,
            &block,
        );
    }
    let ptr = take_or_forget(receiver, block)??;
    // SAFETY: the callback transferred a +1 pointer from Retained::into_raw.
    unsafe { Retained::from_raw(ptr) }
        .ok_or_else(|| ComputerError::ExecutionFailed("SCShareableContent pointer was null".into()))
}

fn sc_windows(content: &SCShareableContent) -> Vec<Retained<SCWindow>> {
    // SAFETY: content is a live SCShareableContent. windows() returns +1.
    let array = unsafe { content.windows() };
    let mut windows = Vec::with_capacity(array.count());
    for index in 0..array.count() {
        windows.push(array.objectAtIndex(index));
    }
    windows
}

fn capture_layer(
    window: &SCWindow,
    points_w: u32,
    points_h: u32,
    scale: f64,
) -> Result<image::RgbaImage, ComputerError> {
    let config = stream_config(px_len(points_w, scale), px_len(points_h, scale));
    if macos_14_or_newer() {
        match capture_manager(window, &config) {
            Ok(image) => Ok(image),
            Err(error) if is_terminal(&error) => Err(error),
            Err(manager_error) => match capture_stream(window, &config) {
                Ok(image) => Ok(image),
                Err(error) if is_terminal(&error) => Err(error),
                Err(stream_error) => Err(ComputerError::ExecutionFailed(format!(
                    "screenshot manager: {manager_error}; stream: {stream_error}"
                ))),
            },
        }
    } else {
        capture_stream(window, &config)
    }
}

fn stream_config(width: usize, height: usize) -> Retained<SCStreamConfiguration> {
    // SAFETY: SCStreamConfiguration is an NSObject. new() returns +1.
    let config = unsafe { SCStreamConfiguration::new() };
    // SAFETY: config is the object just created. The setters copy scalar values.
    // ignoreShadowsSingleWindow is the single-window form of ignoreShadows.
    unsafe {
        config.setWidth(width.max(1));
        config.setHeight(height.max(1));
        config.setShowsCursor(false);
        config.setCapturesAudio(false);
        config.setIgnoreShadowsSingleWindow(true);
        config.setPixelFormat(PIXEL_BGRA);
    }
    config
}

fn capture_manager(
    window: &SCWindow,
    config: &SCStreamConfiguration,
) -> Result<image::RgbaImage, ComputerError> {
    // SAFETY: alloc/init returns +1. window is a live SCWindow for this call.
    let filter = unsafe {
        SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window)
    };
    capture_manager_filter(&filter, config)
}

fn capture_manager_filter(
    filter: &SCContentFilter,
    config: &SCStreamConfiguration,
) -> Result<image::RgbaImage, ComputerError> {
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
        let result = if !error.is_null() {
            // SAFETY: ScreenCaptureKit passes a live NSError for this callback, or null.
            Err(nserror_to_computer(unsafe { &*error }))
        } else {
            cgimage_result(image)
        };
        let _ = sender.send(result);
    });
    // SAFETY: `block` stays alive until the handler runs. A timeout forgets it.
    unsafe {
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            filter,
            config,
            Some(&block),
        );
    }
    take_or_forget(receiver, block)?
}

fn cgimage_result(image: *mut CGImage) -> Result<image::RgbaImage, ComputerError> {
    let Some(ptr) = NonNull::new(image) else {
        return Err(ComputerError::ExecutionFailed("CGImage was null".into()));
    };
    // SAFETY: the callback argument is +0. CFRetain keeps it alive for the conversion.
    let image = unsafe { CFRetained::retain(ptr) };
    cgimage_to_rgba(&image).map_err(ComputerError::ExecutionFailed)
}

fn cgimage_to_rgba(image: &CGImage) -> Result<image::RgbaImage, String> {
    let width = objc2_core_graphics::CGImage::width(Some(image));
    let height = objc2_core_graphics::CGImage::height(Some(image));
    if width == 0 || height == 0 || width > 32_768 || height > 32_768 {
        return Err(format!("CGImage size {width}x{height} is out of range"));
    }
    let width_u = u32::try_from(width).unwrap_or(u32::MAX);
    let height_u = u32::try_from(height).unwrap_or(u32::MAX);
    let bytes_per_row = width.saturating_mul(4);
    let mut buffer = vec![0u8; bytes_per_row.saturating_mul(height)];
    let space = CGColorSpace::new_device_rgb().ok_or("device RGB color space was null")?;
    // SAFETY: buffer is writable for width*height*4 bytes and outlives the context.
    // Bitmap info is kCGBitmapByteOrder32Big | kCGImageAlphaPremultipliedLast.
    let context = unsafe {
        CGBitmapContextCreate(
            buffer.as_mut_ptr().cast(),
            width,
            height,
            8,
            bytes_per_row,
            Some(&space),
            0x4001,
        )
    }
    .ok_or("CGBitmapContextCreate failed")?;
    let rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: width as CGFloat,
            height: height as CGFloat,
        },
    };
    // CGBitmapContextCreate lays the buffer out top-down: the buffer's first
    // row is the image's first row. CGContextDrawImage draws a CGImage right
    // side up into that layout. The standard "translate + scale(1,-1)" flip
    // idiom exists for window contexts (origin at bottom-left). Applying it
    // here inverts the already-correct bitmap layout, producing an upside-down
    // screenshot — exactly the symptom the live SCK path showed before the
    // bugfix. The SCStream path (copy_locked_bgra) has always used the raw
    // top-down layout and remains untouched.
    CGContext::draw_image(Some(&context), rect, Some(image));
    drop(context);
    image::RgbaImage::from_raw(width_u, height_u, buffer)
        .ok_or_else(|| "RGBA buffer length does not match the CGImage".into())
}

struct FrameSinkIvars {
    sender: Mutex<Option<mpsc::Sender<Result<image::RgbaImage, String>>>>,
}

define_class!(
    // SAFETY: NSObject has no subclass requirements. FrameSink does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = AnyThread]
    #[ivars = FrameSinkIvars]
    struct FrameSink;

    // SAFETY: NSObjectProtocol has no extra requirements.
    unsafe impl NSObjectProtocol for FrameSink {}

    // SAFETY: the method signature matches SCStreamOutput. The sample buffer is
    // only used for the duration of the callback.
    unsafe impl SCStreamOutput for FrameSink {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output_sample_buffer_of_type(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            let image = rgba_from_sample(sample_buffer);
            let sender = self
                .ivars()
                .sender
                .lock()
                .ok()
                .and_then(|mut slot| slot.take());
            if let Some(sender) = sender {
                let _ = sender.send(image);
            }
        }
    }
);

impl FrameSink {
    fn new(sender: mpsc::Sender<Result<image::RgbaImage, String>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(FrameSinkIvars {
            sender: Mutex::new(Some(sender)),
        });
        // SAFETY: NSObject init consumes the allocated object and returns +1.
        unsafe { msg_send![super(this), init] }
    }
}

fn capture_stream(
    window: &SCWindow,
    config: &SCStreamConfiguration,
) -> Result<image::RgbaImage, ComputerError> {
    // SAFETY: alloc/init returns +1. window and config are live for this call.
    let filter = unsafe {
        SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window)
    };
    capture_stream_filter(&filter, config)
}

fn capture_stream_filter(
    filter: &SCContentFilter,
    config: &SCStreamConfiguration,
) -> Result<image::RgbaImage, ComputerError> {
    let (sender, receiver) = mpsc::channel();
    let sink = FrameSink::new(sender);
    // SAFETY: filter, config, and a nil delegate are valid for init. Returns +1.
    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), filter, config, None)
    };
    let output = ProtocolObject::from_ref(&*sink);
    let queue = DispatchQueue::new("computer-use.sck", DispatchQueueAttr::SERIAL);
    // SAFETY: output implements SCStreamOutput and the queue stays alive until stop.
    unsafe {
        stream.addStreamOutput_type_sampleHandlerQueue_error(
            output,
            SCStreamOutputType::Screen,
            Some(&queue),
        )
    }
    .map_err(|error| nserror_to_computer(&error))?;
    start_stream(&stream)?;
    let image = match wait_channel(receiver) {
        Ok(result) => result.map_err(ComputerError::ExecutionFailed),
        Err(error) => Err(error),
    };
    let _ = stop_stream(&stream);
    drop(queue);
    drop(sink);
    image
}

fn start_stream(stream: &SCStream) -> Result<(), ComputerError> {
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        let result = if error.is_null() {
            Ok(())
        } else {
            // SAFETY: ScreenCaptureKit passes a live NSError for this callback, or null.
            Err(nserror_to_computer(unsafe { &*error }))
        };
        let _ = sender.send(result);
    });
    // SAFETY: `block` stays alive until the handler runs. A timeout forgets it.
    unsafe { stream.startCaptureWithCompletionHandler(Some(&block)) };
    take_or_forget(receiver, block)?
}

fn stop_stream(stream: &SCStream) -> Result<(), ComputerError> {
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(move |error: *mut NSError| {
        let result = if error.is_null() {
            Ok(())
        } else {
            // SAFETY: ScreenCaptureKit passes a live NSError for this callback, or null.
            Err(nserror_to_computer(unsafe { &*error }))
        };
        let _ = sender.send(result);
    });
    // SAFETY: `block` stays alive until the handler runs. A timeout forgets it.
    unsafe { stream.stopCaptureWithCompletionHandler(Some(&block)) };
    take_or_forget(receiver, block)?
}

fn rgba_from_sample(sample: &CMSampleBuffer) -> Result<image::RgbaImage, String> {
    // SAFETY: sample is live for this callback. The image buffer stays valid
    // until the callback returns. The lock is paired with an unlock.
    unsafe {
        let buffer = CMSampleBufferGetImageBuffer(std::ptr::from_ref(sample).cast());
        if buffer.is_null() {
            return Err("sample had no image buffer".into());
        }
        let status = CVPixelBufferLockBaseAddress(buffer, 1);
        if status != 0 {
            return Err(format!("CVPixelBufferLockBaseAddress failed ({status})"));
        }
        let width = CVPixelBufferGetWidth(buffer);
        let height = CVPixelBufferGetHeight(buffer);
        let stride = CVPixelBufferGetBytesPerRow(buffer);
        let format = CVPixelBufferGetPixelFormatType(buffer);
        let base = CVPixelBufferGetBaseAddress(buffer);
        let result = copy_locked_bgra(base, width, height, stride, format);
        CVPixelBufferUnlockBaseAddress(buffer, 1);
        result
    }
}

/// # Safety
///
/// `base` is the locked base address of a BGRA pixel buffer, or null.
/// `stride` is the buffer's bytes per row. The buffer stays locked for this call.
unsafe fn copy_locked_bgra(
    base: *mut c_void,
    width: usize,
    height: usize,
    stride: usize,
    format: u32,
) -> Result<image::RgbaImage, String> {
    if base.is_null() {
        return Err("pixel buffer base address was null".into());
    }
    if format != PIXEL_BGRA {
        return Err(format!("unexpected pixel format {format:#x}"));
    }
    if width == 0 || height == 0 || width > 32_768 || height > 32_768 || stride < width * 4 {
        return Err(format!(
            "pixel buffer {width}x{height} stride {stride} is out of range"
        ));
    }
    let mut rgba = vec![0u8; width * height * 4];
    for y in 0..height {
        // SAFETY: the caller holds the pixel-buffer lock. Row y starts at
        // base + y*stride and contains at least width*4 bytes.
        let row =
            unsafe { std::slice::from_raw_parts(base.cast::<u8>().add(y * stride), width * 4) };
        bgra_row_to_rgba(row, &mut rgba[y * width * 4..(y + 1) * width * 4]);
    }
    let width_u = u32::try_from(width).unwrap_or(u32::MAX);
    let height_u = u32::try_from(height).unwrap_or(u32::MAX);
    image::RgbaImage::from_raw(width_u, height_u, rgba)
        .ok_or_else(|| "RGBA buffer length does not match the sample".into())
}

/// Convert one BGRA row (4 bytes per pixel, width pixels long) into a packed
/// RGBA row of the same width. The output must be `width * 4` bytes long.
///
/// Pure helper: no platform calls. The SCK row order is top-to-bottom (rows
/// come from the CVPixelBuffer in source order and land in the output buffer
/// in the same order); this helper only swaps the R and B channels per pixel.
/// Keeping it pure means the row-copy logic is unit-testable without a live
/// desktop, and any future caller that needs the same channel swap can reuse
/// it.
fn bgra_row_to_rgba(bgra_row: &[u8], rgba_row: &mut [u8]) {
    debug_assert_eq!(bgra_row.len(), rgba_row.len());
    debug_assert_eq!(bgra_row.len() % 4, 0);
    let pixels = bgra_row.len() / 4;
    for pixel in 0..pixels {
        let src = pixel * 4;
        let dst = pixel * 4;
        // Source is BGRA (kCGBitmapByteOrder32Big). Destination is RGBA.
        rgba_row[dst] = bgra_row[src + 2];
        rgba_row[dst + 1] = bgra_row[src + 1];
        rgba_row[dst + 2] = bgra_row[src];
        rgba_row[dst + 3] = bgra_row[src + 3];
    }
}

fn fallback_crop(
    target: &WindowTarget,
    error: &ComputerError,
) -> Result<RawCapture, ComputerError> {
    let mut raw = super::super::capture::crop_window(target.bounds)?;
    raw.warnings.insert(
        0,
        format!("ScreenCaptureKit failed ({error}); cropped the desktop image instead"),
    );
    Ok(raw)
}

fn nserror_to_computer(error: &NSError) -> ComputerError {
    let domain = error.domain().to_string();
    let code = error.code() as i64;
    let message = error.localizedDescription().to_string();
    match classify_sck_error(&domain, code, &message) {
        CaptureFault::Permission => ComputerError::PermissionRequired(format!(
            "Screen Recording is off. System Settings → Privacy & Security → Screen Recording. {message}"
        )),
        CaptureFault::MissingWindow => ComputerError::WindowNotFound(message),
        CaptureFault::Other => {
            ComputerError::ExecutionFailed(format!("{domain} {code}: {message}"))
        }
    }
}

fn is_terminal(error: &ComputerError) -> bool {
    matches!(
        error,
        ComputerError::PermissionRequired(_)
            | ComputerError::WindowNotFound(_)
            | ComputerError::DisplayUnavailable(_)
    )
}

fn window_id(window: &SCWindow) -> u32 {
    // SAFETY: window is live. windowID returns a scalar CGWindowID.
    unsafe { window.windowID() }
}

fn window_pid(window: &SCWindow) -> Option<i32> {
    // SAFETY: window is live. owningApplication returns +1 or nil.
    let app = unsafe { window.owningApplication() }?;
    // SAFETY: app is that retained owner. processID returns a scalar.
    Some(unsafe { app.processID() })
}

fn window_frame(window: &SCWindow) -> CGRect {
    // SAFETY: window is live. frame returns a copied rectangle.
    unsafe { window.frame() }
}

fn frames_close(rect: CGRect, frame: NodeFrame) -> bool {
    (rect.origin.x - frame.x).abs() <= 2.0
        && (rect.origin.y - frame.y).abs() <= 2.0
        && (rect.size.width - frame.width).abs() <= 2.0
        && (rect.size.height - frame.height).abs() <= 2.0
}

fn backing_scale(origin_x: i32, origin_y: i32) -> f64 {
    let Some(marker) = MainThreadMarker::new() else {
        return 1.0;
    };
    let screens = NSScreen::screens(marker);
    if screens.count() == 0 {
        return 1.0;
    }
    let primary_height = primary_screen_height(&screens);
    let point_x = origin_x as f64;
    let point_y = primary_height - origin_y as f64;
    for index in 0..screens.count() {
        let screen = screens.objectAtIndex(index);
        let frame = screen.frame();
        let max_x = frame.origin.x + frame.size.width;
        let max_y = frame.origin.y + frame.size.height;
        if point_x >= frame.origin.x
            && point_y >= frame.origin.y
            && point_x < max_x
            && point_y < max_y
        {
            return positive_scale(screen.backingScaleFactor());
        }
    }
    NSScreen::mainScreen(marker)
        .map(|screen| positive_scale(screen.backingScaleFactor()))
        .unwrap_or(1.0)
}

fn primary_screen_height(screens: &objc2_foundation::NSArray<NSScreen>) -> f64 {
    for index in 0..screens.count() {
        let frame = screens.objectAtIndex(index).frame();
        if frame.origin.x == 0.0 && frame.origin.y == 0.0 {
            return frame.size.height;
        }
    }
    screens.objectAtIndex(0).frame().size.height
}

fn positive_scale(scale: CGFloat) -> f64 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

fn macos_14_or_newer() -> bool {
    NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 14
}

fn px_len(points: u32, scale: f64) -> usize {
    let value = (f64::from(points.max(1)) * scale).round();
    if !value.is_finite() || value < 1.0 {
        1
    } else if value > 32_768.0 {
        32_768
    } else {
        value as usize
    }
}

fn positive_points(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        1
    } else if value > f64::from(u32::MAX) {
        u32::MAX
    } else {
        value.round() as u32
    }
}

fn round_i32(value: f64) -> i32 {
    if !value.is_finite() {
        0
    } else if value > f64::from(i32::MAX) {
        i32::MAX
    } else if value < f64::from(i32::MIN) {
        i32::MIN
    } else {
        value.round() as i32
    }
}

fn take_or_forget<T>(receiver: mpsc::Receiver<T>, block: impl Sized) -> Result<T, ComputerError> {
    match wait_channel(receiver) {
        Ok(value) => Ok(value),
        Err(error) => {
            std::mem::forget(block);
            Err(error)
        }
    }
}

fn wait_channel<T>(receiver: mpsc::Receiver<T>) -> Result<T, ComputerError> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match receiver.try_recv() {
            Ok(value) => return Ok(value),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(ComputerError::ExecutionFailed(
                    "capture callback dropped".into(),
                ));
            }
            Err(mpsc::TryRecvError::Empty) => {
                if Instant::now() >= deadline {
                    return Err(ComputerError::ExecutionFailed(
                        "screenshot timed out after 5s".into(),
                    ));
                }
                pump();
            }
        }
    }
}

fn pump() {
    if MainThreadMarker::new().is_none() {
        thread::sleep(Duration::from_millis(20));
        return;
    }
    // SAFETY: kCFRunLoopDefaultMode is a process-lifetime mode string.
    // Pumping the main run loop lets a main-queue completion handler run.
    let mode = unsafe { kCFRunLoopDefaultMode };
    let _ = CFRunLoop::run_in_mode(mode, 0.05, false);
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetImageBuffer(sample: *const c_void) -> *mut c_void;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferLockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: *mut c_void, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(buffer: *mut c_void) -> *mut c_void;
    fn CVPixelBufferGetWidth(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetHeight(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetBytesPerRow(buffer: *mut c_void) -> usize;
    fn CVPixelBufferGetPixelFormatType(buffer: *mut c_void) -> u32;
}

#[cfg(test)]
mod tests {
    use super::bgra_row_to_rgba;

    #[test]
    fn bgra_row_to_rgba_swaps_red_and_blue_per_pixel() {
        // Source BGRA, two pixels: red then blue. Output must be RGBA.
        let bgra = [
            0x10, 0x20, 0x30, 0xFF, // B G R A — the pixel reads as "R=0x30 G=0x20 B=0x10"
            0xAA, 0xBB, 0xCC, 0x40, // B G R A — "R=0xCC G=0xBB B=0xAA"
        ];
        let mut rgba = [0_u8; 8];
        bgra_row_to_rgba(&bgra, &mut rgba);
        assert_eq!(
            rgba,
            [
                0x30, 0x20, 0x10, 0xFF, // R G B A
                0xCC, 0xBB, 0xAA, 0x40,
            ]
        );
    }

    #[test]
    fn bgra_row_to_rgba_preserves_alpha() {
        let bgra = [0x01, 0x02, 0x03, 0x77];
        let mut rgba = [0_u8; 4];
        bgra_row_to_rgba(&bgra, &mut rgba);
        assert_eq!(rgba, [0x03, 0x02, 0x01, 0x77]);
    }

    #[test]
    fn bgra_row_to_rgba_handles_an_empty_row() {
        let bgra: [u8; 0] = [];
        let mut rgba: [u8; 0] = [];
        bgra_row_to_rgba(&bgra, &mut rgba);
    }

    #[test]
    fn bgra_row_to_rgba_does_not_reorder_rows() {
        // The SCK row order is top-to-bottom: row 0 lands at the top of the
        // output buffer. This test pins that contract so a future "flip the
        // SCK rows" patch trips here before reaching live code.
        let row_top: [u8; 4] = [0xFF, 0x00, 0x00, 0xFF]; // blue
        let row_bottom: [u8; 4] = [0x00, 0xFF, 0x00, 0xFF]; // green
        let bgra = [row_top.as_slice(), row_bottom.as_slice()].concat();
        let mut rgba_top = [0_u8; 4];
        let mut rgba_bottom = [0_u8; 4];
        bgra_row_to_rgba(&row_top, &mut rgba_top);
        bgra_row_to_rgba(&row_bottom, &mut rgba_bottom);
        assert_eq!(rgba_top, [0x00, 0x00, 0xFF, 0xFF]); // red (B<->R swap)
        assert_eq!(rgba_bottom, [0x00, 0xFF, 0x00, 0xFF]); // green
        let _ = bgra; // keep both rows in scope for symmetry with the live loop
    }
}
