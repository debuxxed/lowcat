use std::path::PathBuf;

use gpui::Window;

#[cfg(target_os = "macos")]
pub(super) use macos::{DragEnd, StartDragError, cancel_gpui_drag};

#[cfg(target_os = "macos")]
pub(super) fn start_file_drag(
    paths: Vec<PathBuf>,
    label: String,
    window: &mut Window,
    on_finish: impl Fn(DragEnd) + Send + 'static,
) -> Result<(), StartDragError> {
    macos::start_file_drag(paths, label, window, on_finish)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::{
        fmt,
        panic::{AssertUnwindSafe, catch_unwind},
        path::PathBuf,
        sync::{Mutex, OnceLock},
    };

    use cocoa::{
        appkit::{NSCompositingOperation, NSEventType},
        base::{BOOL, NO, id, nil},
        foundation::{NSInteger, NSPoint, NSRect, NSSize, NSString, NSUInteger},
    };
    use gpui::Window;
    use objc::{
        class,
        declare::ClassDecl,
        msg_send,
        runtime::{Class, Object, Protocol, Sel},
        sel, sel_impl,
    };
    use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

    const NS_DRAG_OPERATION_COPY: NSUInteger = 1;
    const LEFT_MOUSE_BUTTON_MASK: NSUInteger = 1;
    const DRAG_PREVIEW_WIDTH: f64 = 190.0;
    const DRAG_PREVIEW_HEIGHT: f64 = 28.0;
    const DRAG_PREVIEW_ICON_EDGE: f64 = 16.0;
    const GPUI_VIEW_IVAR: &str = "gpuiView";

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {
        static NSFontAttributeName: id;
        static NSForegroundColorAttributeName: id;
        static NSParagraphStyleAttributeName: id;
    }

    type FinishCallback = Box<dyn Fn(DragEnd) + Send>;

    static FINISH_CALLBACK: OnceLock<Mutex<Option<FinishCallback>>> = OnceLock::new();

    struct RetainedDragEvent(id);

    impl Drop for RetainedDragEvent {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![self.0, release];
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    pub(crate) struct DragEnd {
        pub(crate) screen_x: f64,
        pub(crate) screen_y: f64,
        pub(crate) released: bool,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) enum StartDragError {
        FailedToCreateDragEvent,
        MouseReleased,
        UnsupportedWindow,
        MissingNativeView,
        EmptyFileList,
        FailedToCreatePreview,
        FailedToCreateItem,
        SessionAlreadyRegistered,
        AppKitRejectedSession,
    }

    impl fmt::Display for StartDragError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            let message = match self {
                Self::FailedToCreateDragEvent => "failed to create the AppKit drag event",
                Self::MouseReleased => "left mouse button was released before native drag start",
                Self::UnsupportedWindow => "window does not expose an AppKit handle",
                Self::MissingNativeView => "AppKit view or window is unavailable",
                Self::EmptyFileList => "no existing files were available to drag",
                Self::FailedToCreatePreview => "failed to create the drag preview",
                Self::FailedToCreateItem => "failed to create an AppKit dragging item",
                Self::SessionAlreadyRegistered => "another native drag callback is registered",
                Self::AppKitRejectedSession => "AppKit rejected the dragging session",
            };
            f.write_str(message)
        }
    }

    pub(super) fn start_file_drag(
        paths: Vec<PathBuf>,
        label: String,
        window: &mut Window,
        on_finish: impl Fn(DragEnd) + Send + 'static,
    ) -> Result<(), StartDragError> {
        let start = crate::perf::start();
        let files = absolute_files(paths);
        if files.is_empty() {
            crate::perf::finish("native_drag.prepare", start, || "files=0".to_string());
            return Err(StartDragError::EmptyFileList);
        }
        let file_count = files.len();
        crate::perf::finish("native_drag.prepare", start, || {
            format!("files={file_count}")
        });

        if !left_mouse_button_pressed() {
            return Err(StartDragError::MouseReleased);
        }

        let handle = window
            .window_handle()
            .map_err(|_| StartDragError::UnsupportedWindow)?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return Err(StartDragError::UnsupportedWindow);
        };

        let view = handle.ns_view.as_ptr() as id;
        if view == nil {
            return Err(StartDragError::MissingNativeView);
        }

        let start = crate::perf::start();
        let result = unsafe { begin_drag(view, files, &label, on_finish) };
        crate::perf::finish("native_drag.start", start, || {
            format!("ok={}", result.is_ok())
        });
        result
    }

    unsafe fn begin_drag(
        view: id,
        files: Vec<PathBuf>,
        label: &str,
        on_finish: impl Fn(DragEnd) + Send + 'static,
    ) -> Result<(), StartDragError> {
        let native_window: id = unsafe { msg_send![view, window] };
        if native_window == nil {
            return Err(StartDragError::MissingNativeView);
        }
        let content_view: id = unsafe { msg_send![native_window, contentView] };
        if content_view == nil {
            return Err(StartDragError::MissingNativeView);
        }
        let start_event = unsafe { create_drag_event(native_window) }?;

        let items: id = unsafe { msg_send![class!(NSMutableArray), new] };
        if items == nil {
            return Err(StartDragError::FailedToCreateItem);
        }

        let Some(first_path) = files.first() else {
            unsafe {
                let _: () = msg_send![items, release];
            }
            return Err(StartDragError::EmptyFileList);
        };
        let image = unsafe { create_drag_preview(label, first_path) };
        if image == nil {
            unsafe {
                let _: () = msg_send![items, release];
            }
            return Err(StartDragError::FailedToCreatePreview);
        }

        let window_point: NSPoint = unsafe { msg_send![start_event.0, locationInWindow] };
        let view_point: NSPoint =
            unsafe { msg_send![content_view, convertPoint:window_point fromView:nil] };
        let reported_image_size: NSSize = unsafe { msg_send![image, size] };
        let image_size = valid_drag_preview_size(reported_image_size);
        unsafe {
            let _: () = msg_send![image, setSize:image_size];
        }
        let image_frame = NSRect::new(
            NSPoint::new(view_point.x + 8.0, view_point.y - image_size.height - 8.0),
            image_size,
        );

        for (index, path) in files.into_iter().enumerate() {
            let path = path.to_string_lossy();
            let ns_path = unsafe { NSString::alloc(nil).init_str(path.as_ref()) };
            if ns_path == nil {
                unsafe {
                    let _: () = msg_send![image, release];
                    let _: () = msg_send![items, release];
                }
                return Err(StartDragError::FailedToCreateItem);
            }

            let url: id =
                unsafe { msg_send![class!(NSURL), fileURLWithPath:ns_path isDirectory:NO] };
            let item: id = unsafe { msg_send![class!(NSDraggingItem), alloc] };
            let item: id = unsafe { msg_send![item, initWithPasteboardWriter:url] };
            unsafe {
                let _: () = msg_send![ns_path, release];
            }
            if item == nil {
                unsafe {
                    let _: () = msg_send![image, release];
                    let _: () = msg_send![items, release];
                }
                return Err(StartDragError::FailedToCreateItem);
            }

            unsafe {
                let contents = if index == 0 { image } else { nil };
                let _: () = msg_send![item, setDraggingFrame:image_frame contents:contents];
                let _: () = msg_send![items, addObject:item];
                let _: () = msg_send![item, release];
            }
        }

        if !register_finish_callback(on_finish) {
            unsafe {
                let _: () = msg_send![image, release];
                let _: () = msg_send![items, release];
            }
            return Err(StartDragError::SessionAlreadyRegistered);
        }

        unsafe {
            finish_gpui_mouse_drag(view);
        }

        let source: id = unsafe { msg_send![drag_source_class(), new] };
        unsafe {
            (*source).set_ivar(GPUI_VIEW_IVAR, view as usize);
        }
        let session: id = unsafe {
            msg_send![content_view, beginDraggingSessionWithItems:items event:start_event.0 source:source]
        };
        unsafe {
            let _: () = msg_send![source, release];
            let _: () = msg_send![image, release];
            let _: () = msg_send![items, release];
        }

        if session == nil {
            clear_finish_callback();
            return Err(StartDragError::AppKitRejectedSession);
        }

        Ok(())
    }

    unsafe fn create_drag_preview(label: &str, path: &std::path::Path) -> id {
        let size = NSSize::new(DRAG_PREVIEW_WIDTH, DRAG_PREVIEW_HEIGHT);
        let image: id = unsafe { msg_send![class!(NSImage), alloc] };
        let image: id = unsafe { msg_send![image, initWithSize:size] };
        if image == nil {
            return nil;
        }

        unsafe {
            let _: () = msg_send![image, lockFocus];
        }
        let bounds = NSRect::new(NSPoint::new(0.0, 0.0), size);
        let background: id = unsafe { msg_send![class!(NSColor), controlBackgroundColor] };
        let border: id = unsafe { msg_send![class!(NSColor), separatorColor] };
        let shape: id = unsafe {
            msg_send![class!(NSBezierPath), bezierPathWithRoundedRect:bounds xRadius:6.0_f64 yRadius:6.0_f64]
        };
        unsafe {
            let _: () = msg_send![background, setFill];
            let _: () = msg_send![shape, fill];
            let _: () = msg_send![border, setStroke];
            let _: () = msg_send![shape, setLineWidth:1.0_f64];
            let _: () = msg_send![shape, stroke];
        }

        let ns_path = unsafe { NSString::alloc(nil).init_str(&path.to_string_lossy()) };
        let workspace: id = unsafe { msg_send![class!(NSWorkspace), sharedWorkspace] };
        let icon: id = unsafe { msg_send![workspace, iconForFile:ns_path] };
        unsafe {
            let _: () = msg_send![ns_path, release];
        }
        if icon != nil {
            let icon_rect = NSRect::new(
                NSPoint::new(8.0, 6.0),
                NSSize::new(DRAG_PREVIEW_ICON_EDGE, DRAG_PREVIEW_ICON_EDGE),
            );
            let source_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0));
            unsafe {
                let _: () = msg_send![icon,
                    drawInRect:icon_rect
                    fromRect:source_rect
                    operation:NSCompositingOperation::NSCompositeSourceOver
                    fraction:1.0_f64
                ];
            }
        }

        let attributes: id = unsafe { msg_send![class!(NSMutableDictionary), new] };
        let font: id = unsafe { msg_send![class!(NSFont), systemFontOfSize:12.0_f64] };
        let foreground: id = unsafe { msg_send![class!(NSColor), labelColor] };
        let paragraph: id = unsafe { msg_send![class!(NSMutableParagraphStyle), new] };
        unsafe {
            let _: () = msg_send![paragraph, setLineBreakMode:4 as NSInteger];
            let _: () = msg_send![attributes, setObject:font forKey:NSFontAttributeName];
            let _: () =
                msg_send![attributes, setObject:foreground forKey:NSForegroundColorAttributeName];
            let _: () =
                msg_send![attributes, setObject:paragraph forKey:NSParagraphStyleAttributeName];
        }
        let text = unsafe { NSString::alloc(nil).init_str(label) };
        let text_rect = NSRect::new(
            NSPoint::new(30.0, 6.0),
            NSSize::new(DRAG_PREVIEW_WIDTH - 38.0, 16.0),
        );
        unsafe {
            let _: () = msg_send![text, drawInRect:text_rect withAttributes:attributes];
            let _: () = msg_send![text, release];
            let _: () = msg_send![paragraph, release];
            let _: () = msg_send![attributes, release];
            let _: () = msg_send![image, unlockFocus];
        }
        image
    }

    unsafe fn create_drag_event(native_window: id) -> Result<RetainedDragEvent, StartDragError> {
        let app: id = unsafe { msg_send![class!(NSApplication), sharedApplication] };
        if app == nil {
            return Err(StartDragError::FailedToCreateDragEvent);
        }
        let current_event: id = unsafe { msg_send![app, currentEvent] };
        let timestamp: f64 = if current_event == nil {
            0.0
        } else {
            unsafe { msg_send![current_event, timestamp] }
        };
        let location: NSPoint =
            unsafe { msg_send![native_window, mouseLocationOutsideOfEventStream] };
        let window_number: NSInteger = unsafe { msg_send![native_window, windowNumber] };
        let event: id = unsafe {
            msg_send![class!(NSEvent),
                mouseEventWithType:NSEventType::NSLeftMouseDragged as NSUInteger
                location:location
                modifierFlags:0 as NSUInteger
                timestamp:timestamp
                windowNumber:window_number
                context:nil
                eventNumber:0 as NSInteger
                clickCount:1 as NSInteger
                pressure:1.0_f32
            ]
        };
        if event == nil {
            return Err(StartDragError::FailedToCreateDragEvent);
        }
        let retained: id = unsafe { msg_send![event, retain] };
        if retained == nil {
            Err(StartDragError::FailedToCreateDragEvent)
        } else {
            Ok(RetainedDragEvent(retained))
        }
    }

    fn valid_drag_preview_size(reported: NSSize) -> NSSize {
        if reported.width.is_finite()
            && reported.height.is_finite()
            && reported.width > 0.0
            && reported.height > 0.0
        {
            reported
        } else {
            NSSize::new(DRAG_PREVIEW_WIDTH, DRAG_PREVIEW_HEIGHT)
        }
    }

    unsafe fn finish_gpui_mouse_drag(view: id) {
        if view == nil {
            return;
        }
        let native_window: id = unsafe { msg_send![view, window] };
        if native_window == nil {
            return;
        }

        let app: id = unsafe { msg_send![class!(NSApplication), sharedApplication] };
        let current_event: id = if app == nil {
            nil
        } else {
            unsafe { msg_send![app, currentEvent] }
        };
        if current_event != nil {
            let event_type: NSUInteger = unsafe { msg_send![current_event, type] };
            if event_type == NSEventType::NSLeftMouseUp as NSUInteger {
                unsafe {
                    let _: () = msg_send![view, mouseUp:current_event];
                }
                return;
            }
        }

        let position: NSPoint =
            unsafe { msg_send![native_window, mouseLocationOutsideOfEventStream] };
        let modifier_flags: NSUInteger = if current_event == nil {
            0
        } else {
            unsafe { msg_send![current_event, modifierFlags] }
        };
        let timestamp: f64 = if current_event == nil {
            0.0
        } else {
            unsafe { msg_send![current_event, timestamp] }
        };
        let window_number: NSInteger = unsafe { msg_send![native_window, windowNumber] };
        let mouse_up: id = unsafe {
            msg_send![class!(NSEvent),
                mouseEventWithType:NSEventType::NSLeftMouseUp as NSUInteger
                location:position
                modifierFlags:modifier_flags
                timestamp:timestamp
                windowNumber:window_number
                context:nil
                eventNumber:0 as NSInteger
                clickCount:1 as NSInteger
                pressure:0.0_f32
            ]
        };
        if mouse_up == nil {
            return;
        }
        unsafe {
            let _: () = msg_send![view, mouseUp:mouse_up];
        }
    }

    fn left_mouse_button_pressed() -> bool {
        let buttons: NSUInteger = unsafe { msg_send![class!(NSEvent), pressedMouseButtons] };
        buttons & LEFT_MOUSE_BUTTON_MASK != 0
    }

    pub(crate) fn cancel_gpui_drag(window: &mut Window) {
        let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            return;
        };
        let view = handle.ns_view.as_ptr() as id;
        unsafe {
            finish_gpui_mouse_drag(view);
        }
    }

    fn absolute_files(paths: Vec<PathBuf>) -> Vec<PathBuf> {
        paths
            .into_iter()
            .filter_map(|path| {
                if !path.is_file() {
                    return None;
                }
                if path.is_absolute() {
                    Some(path)
                } else {
                    path.canonicalize().ok()
                }
            })
            .collect()
    }

    fn register_finish_callback(callback: impl Fn(DragEnd) + Send + 'static) -> bool {
        let callbacks = FINISH_CALLBACK.get_or_init(|| Mutex::new(None));
        let mut callbacks = callbacks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if callbacks.is_some() {
            return false;
        }
        *callbacks = Some(Box::new(callback));
        true
    }

    fn clear_finish_callback() {
        let Some(callbacks) = FINISH_CALLBACK.get() else {
            return;
        };
        callbacks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    fn run_finish_callback(end: DragEnd) {
        let callback = FINISH_CALLBACK.get().and_then(|callbacks| {
            callbacks
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
        });
        if let Some(callback) = callback {
            let _ = catch_unwind(AssertUnwindSafe(|| callback(end)));
        }
    }

    fn drag_source_class() -> &'static Class {
        static CLASS: OnceLock<&'static Class> = OnceLock::new();
        CLASS.get_or_init(|| unsafe {
            let mut declaration = ClassDecl::new("LowcatNativeDragSource", class!(NSObject))
                .expect("failed to declare LowcatNativeDragSource");
            declaration.add_ivar::<usize>(GPUI_VIEW_IVAR);
            declaration.add_protocol(
                Protocol::get("NSDraggingSource")
                    .expect("NSDraggingSource protocol is unavailable"),
            );
            declaration.add_method(
                sel!(draggingSession:sourceOperationMaskForDraggingContext:),
                source_operation_mask as extern "C" fn(&Object, Sel, id, NSUInteger) -> NSUInteger,
            );
            declaration.add_method(
                sel!(draggingSourceOperationMaskForLocal:),
                source_operation_mask_local as extern "C" fn(&Object, Sel, BOOL) -> NSUInteger,
            );
            declaration.add_method(
                sel!(draggingSession:endedAtPoint:operation:),
                dragging_session_ended as extern "C" fn(&Object, Sel, id, NSPoint, NSUInteger),
            );
            declaration.register()
        })
    }

    extern "C" fn source_operation_mask(_: &Object, _: Sel, _: id, _: NSUInteger) -> NSUInteger {
        NS_DRAG_OPERATION_COPY
    }

    extern "C" fn source_operation_mask_local(_: &Object, _: Sel, _: BOOL) -> NSUInteger {
        NS_DRAG_OPERATION_COPY
    }

    extern "C" fn dragging_session_ended(
        this: &Object,
        _: Sel,
        _: id,
        ended_at_point: NSPoint,
        _: NSUInteger,
    ) {
        let end = DragEnd {
            screen_x: ended_at_point.x,
            screen_y: ended_at_point.y,
            released: !left_mouse_button_pressed(),
        };
        let view = unsafe { *this.get_ivar::<usize>(GPUI_VIEW_IVAR) as id };
        unsafe {
            finish_gpui_mouse_drag(view);
        }
        run_finish_callback(end);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn zero_sized_drag_preview_uses_nonzero_fallback() {
            let size = valid_drag_preview_size(NSSize::new(0.0, 0.0));

            assert_eq!(size.width, DRAG_PREVIEW_WIDTH);
            assert_eq!(size.height, DRAG_PREVIEW_HEIGHT);
        }
    }
}
