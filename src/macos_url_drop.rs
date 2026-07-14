use std::{
    ffi::CStr,
    mem,
    sync::{Mutex, Once, OnceLock},
};

use cocoa::{
    appkit::{NSFilenamesPboardType, NSPasteboardTypeString, NSStringPboardType, NSURLPboardType},
    base::{BOOL, NO, YES, id, nil},
    foundation::{NSArray, NSString, NSUInteger},
};
use futures::channel::mpsc;
use gpui::Window;
use objc::{
    class, msg_send,
    runtime::{Class, Imp, Method, Object, Sel, method_setImplementation},
    sel, sel_impl,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

type NSDragOperation = NSUInteger;

const NS_DRAG_OPERATION_COPY: NSDragOperation = 1;

static INSTALL_METHODS: Once = Once::new();
static ORIGINAL_DRAG_METHODS: OnceLock<OriginalDragMethods> = OnceLock::new();
static URL_DROP_SENDER: OnceLock<Mutex<Option<mpsc::UnboundedSender<String>>>> = OnceLock::new();

type DragOperationFn = extern "C" fn(&Object, Sel, id) -> NSDragOperation;
type BoolDragFn = extern "C" fn(&Object, Sel, id) -> BOOL;
type VoidDragFn = extern "C" fn(&Object, Sel, id);

struct OriginalDragMethods {
    entered: DragOperationFn,
    updated: DragOperationFn,
    exited: VoidDragFn,
    perform: BoolDragFn,
    conclude: VoidDragFn,
}

pub fn install(window: &mut Window) -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded();
    let sender = URL_DROP_SENDER.get_or_init(|| Mutex::new(None));
    if let Ok(mut sender) = sender.lock() {
        *sender = Some(tx);
    }

    let Ok(handle) = window.window_handle() else {
        return rx;
    };
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return rx;
    };

    unsafe {
        let view = handle.ns_view.as_ptr() as id;
        if view == nil {
            return rx;
        }
        let native_window: id = msg_send![view, window];
        if native_window == nil {
            return rx;
        }

        let window_class = (&*native_window).class();
        INSTALL_METHODS.call_once(|| install_window_drag_methods(window_class));
        let public_url = NSString::alloc(nil).init_str("public.url");
        let public_text = NSString::alloc(nil).init_str("public.text");
        let public_utf8_text = NSString::alloc(nil).init_str("public.utf8-plain-text");
        let types = NSArray::arrayWithObjects(
            nil,
            &[
                NSFilenamesPboardType,
                public_url,
                public_text,
                public_utf8_text,
                NSURLPboardType,
                NSPasteboardTypeString,
                NSStringPboardType,
            ],
        );
        let _: () = msg_send![native_window, registerForDraggedTypes: types];
        let _: () = msg_send![public_url, release];
        let _: () = msg_send![public_text, release];
        let _: () = msg_send![public_utf8_text, release];
        debug_url_drop(|| {
            format!(
                "installed on window_class={} registered_types={}",
                window_class.name(),
                registered_drag_types(native_window)
            )
        });
    }

    rx
}

unsafe fn install_window_drag_methods(window_class: &Class) {
    unsafe {
        let entered = replace_method(
            window_class,
            sel!(draggingEntered:),
            drag_operation_imp(dragging_entered),
        );
        let updated = replace_method(
            window_class,
            sel!(draggingUpdated:),
            drag_operation_imp(dragging_updated),
        );
        let exited = replace_method(
            window_class,
            sel!(draggingExited:),
            void_drag_imp(dragging_exited),
        );
        let perform = replace_method(
            window_class,
            sel!(performDragOperation:),
            bool_drag_imp(perform_drag_operation),
        );
        let conclude = replace_method(
            window_class,
            sel!(concludeDragOperation:),
            void_drag_imp(conclude_drag_operation),
        );
        ORIGINAL_DRAG_METHODS
            .set(OriginalDragMethods {
                entered: mem::transmute::<Imp, DragOperationFn>(entered),
                updated: mem::transmute::<Imp, DragOperationFn>(updated),
                exited: mem::transmute::<Imp, VoidDragFn>(exited),
                perform: mem::transmute::<Imp, BoolDragFn>(perform),
                conclude: mem::transmute::<Imp, VoidDragFn>(conclude),
            })
            .unwrap_or_else(|_| panic!("native drag methods installed twice"));
    }
}

unsafe fn replace_method(window_class: &Class, selector: Sel, replacement: Imp) -> Imp {
    let method = window_class
        .instance_method(selector)
        .expect("GPUI window must implement native drag destination methods");
    unsafe { method_setImplementation(method as *const Method as *mut Method, replacement) }
}

fn original_drag_methods() -> &'static OriginalDragMethods {
    ORIGINAL_DRAG_METHODS
        .get()
        .expect("native drag methods must be installed before dispatch")
}

fn drag_operation_imp(function: extern "C" fn(&Object, Sel, id) -> NSDragOperation) -> Imp {
    unsafe { mem::transmute(function) }
}

fn bool_drag_imp(function: extern "C" fn(&Object, Sel, id) -> BOOL) -> Imp {
    unsafe { mem::transmute(function) }
}

fn void_drag_imp(function: extern "C" fn(&Object, Sel, id)) -> Imp {
    unsafe { mem::transmute(function) }
}

extern "C" fn dragging_entered(this: &Object, _: Sel, dragging_info: id) -> NSDragOperation {
    if dropped_youtube_url(dragging_info).is_some() {
        debug_url_drop(|| "entered youtube url drag".to_string());
        NS_DRAG_OPERATION_COPY
    } else {
        debug_url_drop(|| {
            format!(
                "forwarded non-url drag {}",
                pasteboard_debug_summary(dragging_info)
            )
        });
        let operation =
            (original_drag_methods().entered)(this, sel!(draggingEntered:), dragging_info);
        debug_url_drop(|| format!("GPUI file drag entered operation={operation}"));
        operation
    }
}

extern "C" fn dragging_updated(this: &Object, _: Sel, dragging_info: id) -> NSDragOperation {
    if dropped_youtube_url(dragging_info).is_some() {
        NS_DRAG_OPERATION_COPY
    } else {
        (original_drag_methods().updated)(this, sel!(draggingUpdated:), dragging_info)
    }
}

extern "C" fn dragging_exited(this: &Object, _: Sel, dragging_info: id) {
    if dropped_youtube_url(dragging_info).is_none() {
        (original_drag_methods().exited)(this, sel!(draggingExited:), dragging_info);
    }
}

extern "C" fn perform_drag_operation(this: &Object, _: Sel, dragging_info: id) -> BOOL {
    let Some(url) = dropped_youtube_url(dragging_info) else {
        debug_url_drop(|| "forwarded non-url drop".to_string());
        let accepted =
            (original_drag_methods().perform)(this, sel!(performDragOperation:), dragging_info);
        debug_url_drop(|| format!("GPUI file drop accepted={accepted:?}"));
        return accepted;
    };
    if let Some(sender) = URL_DROP_SENDER.get()
        && let Ok(sender) = sender.lock()
        && let Some(sender) = sender.as_ref()
    {
        debug_url_drop(|| format!("drop submitted url={url}"));
        let _ = sender.unbounded_send(url);
        return YES;
    }
    debug_url_drop(|| "drop rejected without receiver".to_string());
    NO
}

extern "C" fn conclude_drag_operation(this: &Object, _: Sel, dragging_info: id) {
    if dropped_youtube_url(dragging_info).is_none() {
        (original_drag_methods().conclude)(this, sel!(concludeDragOperation:), dragging_info);
    }
}

fn dropped_youtube_url(dragging_info: id) -> Option<String> {
    let candidates = unsafe {
        let pasteboard: id = msg_send![dragging_info, draggingPasteboard];
        pasteboard_candidate_texts(pasteboard)
    };
    candidates
        .into_iter()
        .find_map(|text| crate::downloader::extract_youtube_url(&text).ok())
}

unsafe fn pasteboard_candidate_texts(pasteboard: id) -> Vec<String> {
    if pasteboard == nil {
        return Vec::new();
    }

    let public_url = unsafe { NSString::alloc(nil).init_str("public.url") };
    let public_text = unsafe { NSString::alloc(nil).init_str("public.text") };
    let public_utf8_text = unsafe { NSString::alloc(nil).init_str("public.utf8-plain-text") };
    let pasteboard_types = [
        public_url,
        unsafe { NSURLPboardType },
        unsafe { NSPasteboardTypeString },
        unsafe { NSStringPboardType },
        public_text,
        public_utf8_text,
    ];

    let mut texts = Vec::new();
    for pasteboard_type in pasteboard_types {
        if let Some(text) = unsafe { pasteboard_string(pasteboard, pasteboard_type) } {
            texts.push(text);
        }
        let property_list: id =
            unsafe { msg_send![pasteboard, propertyListForType: pasteboard_type] };
        unsafe { append_object_texts(property_list, &mut texts) };
    }

    unsafe {
        let _: () = msg_send![public_url, release];
        let _: () = msg_send![public_text, release];
        let _: () = msg_send![public_utf8_text, release];
    }

    texts
}

unsafe fn append_object_texts(object: id, texts: &mut Vec<String>) {
    if object == nil {
        return;
    }

    let is_string: BOOL = unsafe { msg_send![object, isKindOfClass: class!(NSString)] };
    if is_string == YES {
        if let Some(text) = unsafe { ns_string_to_string(object) } {
            texts.push(text);
        }
        return;
    }

    let is_url: BOOL = unsafe { msg_send![object, isKindOfClass: class!(NSURL)] };
    if is_url == YES {
        let absolute_string: id = unsafe { msg_send![object, absoluteString] };
        if let Some(text) = unsafe { ns_string_to_string(absolute_string) } {
            texts.push(text);
        }
        return;
    }

    let is_array: BOOL = unsafe { msg_send![object, isKindOfClass: class!(NSArray)] };
    if is_array == YES {
        let count = unsafe { NSArray::count(object) };
        for index in 0..count {
            let item = unsafe { NSArray::objectAtIndex(object, index) };
            unsafe { append_object_texts(item, texts) };
        }
    }
}

unsafe fn pasteboard_string(pasteboard: id, pasteboard_type: id) -> Option<String> {
    if pasteboard == nil || pasteboard_type == nil {
        return None;
    }
    let value: id = unsafe { msg_send![pasteboard, stringForType: pasteboard_type] };
    unsafe { ns_string_to_string(value) }
}

unsafe fn ns_string_to_string(value: id) -> Option<String> {
    if value == nil {
        return None;
    }
    let ptr = unsafe { NSString::UTF8String(value) };
    if ptr.is_null() {
        return None;
    }
    Some(
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn pasteboard_debug_summary(dragging_info: id) -> String {
    unsafe {
        let pasteboard: id = msg_send![dragging_info, draggingPasteboard];
        if pasteboard == nil {
            return "types=<none> candidates=<none>".to_string();
        }
        let types: id = msg_send![pasteboard, types];
        let type_names = ns_array_strings(types).join(",");
        let candidates = pasteboard_candidate_texts(pasteboard)
            .into_iter()
            .map(|candidate| {
                let mut candidate = candidate.replace('\n', "\\n");
                if candidate.len() > 120 {
                    candidate.truncate(120);
                    candidate.push_str("...");
                }
                candidate
            })
            .collect::<Vec<_>>()
            .join(" | ");
        format!("types={type_names} candidates={candidates}")
    }
}

fn registered_drag_types(native_window: id) -> String {
    unsafe {
        let types: id = msg_send![native_window, registeredDraggedTypes];
        ns_array_strings(types).join(",")
    }
}

unsafe fn ns_array_strings(array: id) -> Vec<String> {
    if array == nil {
        return Vec::new();
    }
    let count = unsafe { NSArray::count(array) };
    let mut strings = Vec::new();
    for index in 0..count {
        let item = unsafe { NSArray::objectAtIndex(array, index) };
        if let Some(text) = unsafe { ns_string_to_string(item) } {
            strings.push(text);
        }
    }
    strings
}

fn debug_url_drop(details: impl FnOnce() -> String) {
    crate::diagnostics::debug("url-drop", details);
}
