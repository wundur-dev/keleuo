//! A spinner the system animates: on macOS an `NSProgressIndicator` laid over the window, which
//! Core Animation turns without the app doing anything. A spinner drawn by GPUI rebuilds and
//! repaints the whole window on every step (GPUI has no animation of its own outside a frame),
//! so the working line's spinner alone kept the app at around ten per cent of a core while an
//! agent worked.
//!
//! Placed where it is painted: each frame that shows it keeps a guard in one of its mouse
//! listeners (as the web panel does for its page), and when the last goes the spinner hides.
//! Elsewhere than macOS [`NativeSpinner::available`] is false and the caller draws its own.

use std::cell::Cell;
use std::rc::{Rc, Weak};

use gpui_kit::{Bounds, MouseMoveEvent, Pixels, Window};

#[derive(Clone)]
pub struct NativeSpinner(Rc<Inner>);

struct Inner {
    present: Cell<u32>,
    #[cfg(target_os = "macos")]
    view: std::cell::RefCell<Option<objc2::rc::Retained<objc2::runtime::AnyObject>>>,
    placed: Cell<Option<Bounds<Pixels>>>,
    shown: Cell<bool>,
}

impl NativeSpinner {
    pub fn new() -> Self {
        Self(Rc::new(Inner {
            present: Cell::new(0),
            #[cfg(target_os = "macos")]
            view: Default::default(),
            placed: Cell::new(None),
            shown: Cell::new(false),
        }))
    }

    /// Whether this system has one; without it the caller draws a spinner of its own.
    pub fn available() -> bool {
        cfg!(target_os = "macos")
    }

    /// Call while painting the place it goes: it shows there (window coordinates) until a frame
    /// is drawn without it.
    pub fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let guard = Guard::new(&self.0);
        window.on_mouse_event(move |_: &MouseMoveEvent, _, _, _| {
            let _ = &guard;
        });
        // The window's own overlays (a dialog, the peeking list) draw below native views.
        if crate::web_panel::is_covered() {
            self.0.hide();
        } else {
            self.0.show(bounds, window);
        }
    }
}

impl Default for NativeSpinner {
    fn default() -> Self {
        Self::new()
    }
}

struct Guard(Weak<Inner>);

impl Guard {
    fn new(inner: &Rc<Inner>) -> Self {
        inner.present.set(inner.present.get() + 1);
        Self(Rc::downgrade(inner))
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(inner) = self.0.upgrade() {
            let left = inner.present.get().saturating_sub(1);
            inner.present.set(left);
            if left == 0 {
                inner.hide();
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Inner {
    fn show(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        use objc2::runtime::AnyObject;
        use objc2::msg_send;
        use objc2_foundation::{NSPoint, NSRect, NSSize};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        if self.view.borrow().is_none() {
            let Ok(handle) = window.window_handle() else { return };
            let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
            let parent = h.ns_view.as_ptr() as *mut AnyObject;
            // SAFETY: on the main thread; an NSProgressIndicator made, set up and added to the
            // window's content view, which keeps it; ours is a second reference.
            unsafe {
                let class = objc2::class!(NSProgressIndicator);
                let made: *mut AnyObject = msg_send![class, alloc];
                let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(16., 16.));
                let made: *mut AnyObject = msg_send![made, initWithFrame: frame];
                if made.is_null() {
                    return;
                }
                // Spinning, small, turning on its own.
                let _: () = msg_send![made, setStyle: 1usize];
                let _: () = msg_send![made, setControlSize: 1usize];
                let _: () = msg_send![made, setIndeterminate: true];
                let _: () = msg_send![made, setDisplayedWhenStopped: false];
                let _: () = msg_send![made, setHidden: true];
                let _: () = msg_send![parent, addSubview: made];
                let _: () = msg_send![made, startAnimation: std::ptr::null::<AnyObject>()];
                *self.view.borrow_mut() = objc2::rc::Retained::from_raw(made);
            }
        }
        let view = self.view.borrow();
        let Some(view) = view.as_ref() else { return };
        if self.placed.get() != Some(bounds) {
            self.placed.set(Some(bounds));
            // SAFETY: plain getters and setters of the views, on the main thread.
            unsafe {
                let parent: *mut AnyObject = msg_send![&**view, superview];
                if parent.is_null() {
                    return;
                }
                let flipped: bool = msg_send![parent, isFlipped];
                let outer: NSRect = msg_send![parent, bounds];
                let (x, y, w, h) = (
                    f64::from(f32::from(bounds.origin.x)),
                    f64::from(f32::from(bounds.origin.y)),
                    f64::from(f32::from(bounds.size.width)),
                    f64::from(f32::from(bounds.size.height)),
                );
                let y = if flipped { y } else { outer.size.height - y - h };
                let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
                let _: () = msg_send![&**view, setFrame: frame];
            }
        }
        if !self.shown.replace(true) {
            // SAFETY: as above.
            unsafe {
                let _: () = msg_send![&**view, setHidden: false];
            }
        }
    }

    fn hide(&self) {
        if self.shown.replace(false)
            && let Some(view) = self.view.borrow().as_ref()
        {
            // SAFETY: a setter on the main thread.
            unsafe {
                let _: () = objc2::msg_send![&**view, setHidden: true];
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(view) = self.view.borrow_mut().take() {
            // SAFETY: taken off the window on the main thread, where it was added.
            unsafe {
                let _: () = objc2::msg_send![&*view, removeFromSuperview];
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl Inner {
    fn show(&self, _: Bounds<Pixels>, _: &mut Window) {}
    fn hide(&self) {}
}
