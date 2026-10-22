// HANE-PATCH: Not part of upstream accesskit_windows 0.34.0.
//
// Calls the COM provider interfaces directly (in-process) with a Context whose
// window handle is valid, destroyed, or bogus, and checks the HRESULT that
// comes back through the COM boundary.

use accesskit::{
    ActionHandler, ActionRequest, Node, NodeId, Rect, Role, TextDirection, TextPosition,
    TextSelection, Tree, TreeId, TreeUpdate,
};
use accesskit_consumer::Tree as ConsumerTree;
use std::sync::Arc;
use windows::{
    Win32::{
        Foundation::*,
        System::{
            Com::SAFEARRAY,
            Ole::{SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound},
        },
        UI::{Accessibility::*, WindowsAndMessaging::*},
    },
    core::*,
};

use super::MUTEX;
use crate::{
    context::{ActionHandlerWrapper, Context},
    filters::filter,
    window_handle::WindowHandle,
};

const ROOT_ID: NodeId = NodeId(0);
const BUTTON_ID: NodeId = NodeId(1);
const TEXT_INPUT_ID: NodeId = NodeId(2);
const TEXT_RUN_ID: NodeId = NodeId(3);

struct NullActionHandler;

impl ActionHandler for NullActionHandler {
    fn do_action(&mut self, _request: ActionRequest) {}
}

fn initial_state() -> TreeUpdate {
    let bounds = Rect::new(0.0, 0.0, 100.0, 50.0);

    let mut root = Node::new(Role::Window);
    root.set_bounds(bounds);
    root.set_children(vec![BUTTON_ID, TEXT_INPUT_ID]);

    let mut button = Node::new(Role::Button);
    button.set_label("Button");
    button.set_bounds(bounds);

    let mut text_input = Node::new(Role::MultilineTextInput);
    text_input.set_bounds(bounds);
    text_input.set_children(vec![TEXT_RUN_ID]);
    text_input.set_text_selection(TextSelection {
        anchor: TextPosition {
            node: TEXT_RUN_ID,
            character_index: 0,
        },
        focus: TextPosition {
            node: TEXT_RUN_ID,
            character_index: 0,
        },
    });

    let mut text_run = Node::new(Role::TextRun);
    text_run.set_bounds(Rect::new(0.0, 0.0, 50.0, 20.0));
    text_run.set_value("hello");
    text_run.set_character_lengths(vec![1u8; 5]);
    text_run.set_character_positions(vec![0.0f32, 10.0, 20.0, 30.0, 40.0]);
    text_run.set_character_widths(vec![10.0f32; 5]);
    text_run.set_word_starts(vec![0u8]);
    // Without a text direction the consumer produces no bounding boxes, and
    // `GetBoundingRectangles` returns null before reaching `client_top_left`.
    text_run.set_text_direction(TextDirection::LeftToRight);

    TreeUpdate {
        nodes: vec![
            (ROOT_ID, root),
            (BUTTON_ID, button),
            (TEXT_INPUT_ID, text_input),
            (TEXT_RUN_ID, text_run),
        ],
        tree: Some(Tree::new(ROOT_ID)),
        tree_id: TreeId::ROOT,
        focus: BUTTON_ID,
    }
}

fn new_context(hwnd: HWND) -> Arc<Context> {
    Context::new(
        WindowHandle(hwnd),
        ConsumerTree::new(initial_state(), false),
        Arc::new(ActionHandlerWrapper::new(NullActionHandler)),
        false,
    )
}

fn element_not_available() -> HRESULT {
    HRESULT(UIA_E_ELEMENTNOTAVAILABLE as _)
}

/// A handle that referred to a real window that has since been destroyed.
fn destroyed_hwnd() -> HWND {
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("accesskit-windows-invalid-hwnd"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            None,
            None,
        )
    }
    .unwrap();
    unsafe { DestroyWindow(hwnd) }.unwrap();
    hwnd
}

fn bogus_hwnd() -> HWND {
    HWND(0x1 as *mut core::ffi::c_void)
}

fn valid_hwnd() -> HWND {
    unsafe { GetDesktopWindow() }
}

fn invalid_hwnds() -> [HWND; 3] {
    [HWND::default(), bogus_hwnd(), destroyed_hwnd()]
}

fn fragment(context: &Arc<Context>) -> IRawElementProviderFragment {
    let id = context.read_tree().state().root_id();
    context.get_or_create_platform_node(id).into_interface()
}

fn fragment_root(context: &Arc<Context>) -> IRawElementProviderFragmentRoot {
    let id = context.read_tree().state().root_id();
    context.get_or_create_platform_node(id).into_interface()
}

fn text_provider(context: &Arc<Context>) -> ITextProvider {
    let id = {
        let tree = context.read_tree();
        let root = tree.state().root();
        root.filtered_children(&filter)
            .find(|node| node.supports_text_ranges())
            .unwrap()
            .id()
    };
    context.get_or_create_platform_node(id).into_interface()
}

#[test]
fn bounding_rectangle_reports_element_not_available_for_invalid_hwnd() {
    let _lock_guard = MUTEX.lock();
    for hwnd in invalid_hwnds() {
        let fragment = fragment(&new_context(hwnd));
        let error = unsafe { fragment.BoundingRectangle() }.unwrap_err();
        assert_eq!(error.code(), element_not_available());
    }
}

#[test]
fn element_provider_from_point_reports_element_not_available_for_invalid_hwnd() {
    let _lock_guard = MUTEX.lock();
    for hwnd in invalid_hwnds() {
        let root = fragment_root(&new_context(hwnd));
        let error = unsafe { root.ElementProviderFromPoint(10.0, 10.0) }.unwrap_err();
        assert_eq!(error.code(), element_not_available());
    }
}

#[test]
fn range_from_point_reports_element_not_available_for_invalid_hwnd() {
    let _lock_guard = MUTEX.lock();
    for hwnd in invalid_hwnds() {
        let provider = text_provider(&new_context(hwnd));
        let point = UiaPoint { x: 10.0, y: 10.0 };
        let error = unsafe { provider.RangeFromPoint(point) }.unwrap_err();
        assert_eq!(error.code(), element_not_available());
    }
}

/// Reads the `[left, top, width, height]` quadruples out of the SAFEARRAY
/// returned by `GetBoundingRectangles` and destroys the array.
fn take_rectangles(array: *mut SAFEARRAY) -> Vec<f64> {
    assert!(!array.is_null());
    let lower = unsafe { SafeArrayGetLBound(array, 1) }.unwrap();
    let upper = unsafe { SafeArrayGetUBound(array, 1) }.unwrap();
    let mut values = Vec::new();
    for index in lower..=upper {
        let mut value = 0.0f64;
        unsafe { SafeArrayGetElement(array, &index, &mut value as *mut f64 as *mut _) }.unwrap();
        values.push(value);
    }
    unsafe { SafeArrayDestroy(array) }.unwrap();
    values
}

#[test]
fn get_bounding_rectangles_reports_element_not_available_for_invalid_hwnd() {
    let _lock_guard = MUTEX.lock();

    // Precondition: with a valid HWND the same fixture yields a non-empty
    // rectangle list, so an empty range cannot hide the `client_top_left`
    // error path below behind the early null return.
    let valid_context = new_context(valid_hwnd());
    let valid_range = unsafe { text_provider(&valid_context).DocumentRange() }.unwrap();
    let valid_rectangles = take_rectangles(unsafe { valid_range.GetBoundingRectangles() }.unwrap());
    assert!(!valid_rectangles.is_empty());

    for hwnd in invalid_hwnds() {
        // Keep the Context alive; the provider and range only hold weak
        // references, so a dropped Context would fail `DocumentRange()` itself.
        let context = new_context(hwnd);
        let provider = text_provider(&context);
        let range = unsafe { provider.DocumentRange() }.unwrap();
        let error = unsafe { range.GetBoundingRectangles() }.unwrap_err();
        assert_eq!(error.code(), element_not_available());
    }
}

#[test]
fn get_bounding_rectangles_reports_element_not_available_after_window_destroyed() {
    let _lock_guard = MUTEX.lock();
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            w!("STATIC"),
            w!("accesskit-windows-invalid-hwnd-live"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            None,
            None,
        )
    }
    .unwrap();
    let context = new_context(hwnd);
    let range = unsafe { text_provider(&context).DocumentRange() }.unwrap();
    let rectangles = take_rectangles(unsafe { range.GetBoundingRectangles() }.unwrap());
    assert!(!rectangles.is_empty());

    unsafe { DestroyWindow(hwnd) }.unwrap();
    let error = unsafe { range.GetBoundingRectangles() }.unwrap_err();
    assert_eq!(error.code(), element_not_available());
}

#[test]
fn valid_hwnd_keeps_geometry_and_hit_testing() {
    let _lock_guard = MUTEX.lock();
    let context = new_context(valid_hwnd());
    let top_left = context.client_top_left().unwrap();

    let rect = unsafe { fragment(&context).BoundingRectangle() }.unwrap();
    assert_eq!(rect.left, top_left.x);
    assert_eq!(rect.top, top_left.y);
    assert_eq!(rect.width, 100.0);
    assert_eq!(rect.height, 50.0);

    unsafe { fragment_root(&context).ElementProviderFromPoint(top_left.x + 10.0, top_left.y + 10.0) }
        .unwrap();

    let provider = text_provider(&context);
    let point = UiaPoint {
        x: top_left.x + 10.0,
        y: top_left.y + 10.0,
    };
    unsafe { provider.RangeFromPoint(point) }.unwrap();

    let range = unsafe { provider.DocumentRange() }.unwrap();
    // The text run is the only node in the range; it spans x 0..50, y 0..20.
    let rectangles = take_rectangles(unsafe { range.GetBoundingRectangles() }.unwrap());
    assert_eq!(
        rectangles,
        vec![top_left.x, top_left.y, 50.0, 20.0],
        "unexpected rectangles: {rectangles:?}"
    );
}
