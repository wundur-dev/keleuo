//! The working line's spinner, turned by the system: on macOS a view laid over the window whose
//! Core Animation layer draws our own spinner (an open circle in the theme's grey, as the loader
//! icon) and spins it. Core Animation turns it without the app doing anything; a spinner drawn
//! by GPUI rebuilds and repaints the whole window on every step (GPUI has no animation of its
//! own outside a frame), and that alone kept the app at around ten per cent of a core while an
//! agent worked.
//!
//! Placed where it is painted: each frame that shows it keeps a guard in one of its mouse
//! listeners (as the web panel does for its page), and when the last goes the spinner hides.
//! Elsewhere than macOS [`NativeSpinner::available`] is false and the caller draws its own.

use std::cell::Cell;
use std::rc::{Rc, Weak};

use gpui_kit::{Bounds, Hsla, MouseMoveEvent, Pixels, Window};

#[derive(Clone)]
pub struct NativeSpinner(Rc<Inner>);

struct Inner {
    present: Cell<u32>,
    #[cfg(target_os = "macos")]
    view: std::cell::RefCell<Option<mac::Spinner>>,
    #[cfg(target_os = "macos")]
    placed: Cell<Option<Bounds<Pixels>>>,
    #[cfg(target_os = "macos")]
    shown: Cell<bool>,
    #[cfg(target_os = "macos")]
    color: Cell<Option<Hsla>>,
}

impl NativeSpinner {
    pub fn new() -> Self {
        Self(Rc::new(Inner {
            present: Cell::new(0),
            #[cfg(target_os = "macos")]
            view: Default::default(),
            #[cfg(target_os = "macos")]
            placed: Cell::new(None),
            #[cfg(target_os = "macos")]
            shown: Cell::new(false),
            #[cfg(target_os = "macos")]
            color: Cell::new(None),
        }))
    }

    /// Whether this system has one; without it the caller draws a spinner of its own.
    pub fn available() -> bool {
        cfg!(target_os = "macos")
    }

    /// Call while painting the place it goes: it shows there (window coordinates), in `color`,
    /// until a frame is drawn without it.
    pub fn paint(&self, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
        let guard = Guard::new(&self.0);
        window.on_mouse_event(move |_: &MouseMoveEvent, _, _, _| {
            let _ = &guard;
        });
        // The window's own overlays (a dialog, the peeking list) draw below native views.
        if crate::web_panel::is_covered() {
            self.0.hide();
        } else {
            self.0.show(bounds, color, window);
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
    fn show(&self, bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
        if self.view.borrow().is_none() {
            let made = mac::Spinner::new(window);
            *self.view.borrow_mut() = made;
        }
        let view = self.view.borrow();
        let Some(view) = view.as_ref() else { return };
        if self.color.get() != Some(color) {
            self.color.set(Some(color));
            view.set_color(color);
        }
        if self.placed.get() != Some(bounds) {
            self.placed.set(Some(bounds));
            view.place(bounds);
        }
        if !self.shown.replace(true) {
            view.set_hidden(false);
        }
    }

    fn hide(&self) {
        if self.shown.replace(false)
            && let Some(view) = self.view.borrow().as_ref()
        {
            view.set_hidden(true);
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl Inner {
    fn show(&self, _: Bounds<Pixels>, _: Hsla, _: &mut Window) {}
    fn hide(&self) {}
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    use gpui_kit::{Bounds, Hsla, Pixels, Rgba, Window};
    use objc2::encode::{Encoding, RefEncode};
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{class, msg_send};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// The spinner's size, its circle's stroke and the time one turn takes.
    const SIZE: f64 = 16.;
    const STROKE: f64 = 1.6;
    const TURN_SECONDS: f64 = 0.9;

    #[repr(C)]
    struct CGPath(c_void);
    // SAFETY: CGPathRef's encoding, as Core Animation's methods declare it.
    unsafe impl RefEncode for CGPath {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGPath", &[]));
    }
    #[repr(C)]
    struct CGColor(c_void);
    // SAFETY: CGColorRef's encoding.
    unsafe impl RefEncode for CGColor {
        const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPathCreateMutable() -> *mut CGPath;
        fn CGPathAddArc(path: *mut CGPath, m: *const c_void, x: f64, y: f64, radius: f64, start: f64, end: f64, clockwise: bool);
        fn CGPathRelease(path: *mut CGPath);
        fn CGColorCreateSRGB(r: f64, g: f64, b: f64, a: f64) -> *mut CGColor;
        fn CGColorRelease(color: *mut CGColor);
    }
    #[link(name = "QuartzCore", kind = "framework")]
    unsafe extern "C" {}

    /// A layer-backed view on the window, holding the spinning shape layer.
    pub struct Spinner {
        view: Retained<AnyObject>,
        shape: Retained<AnyObject>,
    }

    impl Spinner {
        pub fn new(window: &mut Window) -> Option<Self> {
            let handle = window.window_handle().ok()?;
            let RawWindowHandle::AppKit(h) = handle.as_raw() else { return None };
            let parent = h.ns_view.as_ptr() as *mut AnyObject;
            // SAFETY: on the main thread; a plain NSView and a CAShapeLayer made and set up as
            // their classes document, the view added to the window's content view (which keeps
            // it), ours a second reference; the path and colours released after the layer took
            // its own references.
            unsafe {
                let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(SIZE, SIZE));
                let view: *mut AnyObject = msg_send![class!(NSView), alloc];
                let view: *mut AnyObject = msg_send![view, initWithFrame: frame];
                let view = Retained::from_raw(view)?;
                let _: () = msg_send![&*view, setWantsLayer: true];
                let _: () = msg_send![&*view, setHidden: true];
                let host: *mut AnyObject = msg_send![&*view, layer];

                let shape: *mut AnyObject = msg_send![class!(CAShapeLayer), layer];
                let shape = Retained::retain(shape)?;
                let _: () = msg_send![&*shape, setFrame: frame];
                // Three quarters of a circle, open at the top right as the loader icon is.
                let path = CGPathCreateMutable();
                let r = (SIZE - STROKE) / 2. - 1.;
                CGPathAddArc(path, std::ptr::null(), SIZE / 2., SIZE / 2., r, 0., std::f64::consts::PI * 1.5, false);
                let _: () = msg_send![&*shape, setPath: path as *const CGPath];
                CGPathRelease(path);
                let _: () = msg_send![&*shape, setFillColor: std::ptr::null::<CGColor>()];
                let _: () = msg_send![&*shape, setLineWidth: STROKE];
                let _: () = msg_send![&*shape, setLineCap: &*NSString::from_str("round")];
                let _: () = msg_send![host, addSublayer: &*shape];

                // One turn every TURN_SECONDS, for ever, turned by Core Animation.
                let key = NSString::from_str("transform.rotation.z");
                let spin: *mut AnyObject = msg_send![class!(CABasicAnimation), animationWithKeyPath: &*key];
                let from: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: 0.0f64];
                let to: *mut AnyObject = msg_send![class!(NSNumber), numberWithDouble: -std::f64::consts::TAU];
                let _: () = msg_send![spin, setFromValue: from];
                let _: () = msg_send![spin, setToValue: to];
                let _: () = msg_send![spin, setDuration: TURN_SECONDS];
                let _: () = msg_send![spin, setRepeatCount: f32::INFINITY];
                let _: () = msg_send![spin, setRemovedOnCompletion: false];
                let _: () = msg_send![&*shape, addAnimation: spin, forKey: &*NSString::from_str("spin")];

                let _: () = msg_send![parent, addSubview: &*view];
                Some(Self { view, shape })
            }
        }

        pub fn set_color(&self, color: Hsla) {
            let c = Rgba::from(color);
            // SAFETY: a colour made, handed to the layer (which retains it) and released.
            unsafe {
                let cg = CGColorCreateSRGB(c.r as f64, c.g as f64, c.b as f64, c.a as f64);
                let _: () = msg_send![&*self.shape, setStrokeColor: cg as *const CGColor];
                CGColorRelease(cg);
            }
        }

        /// At `bounds`, in the window's coordinates (top left origin).
        pub fn place(&self, bounds: Bounds<Pixels>) {
            // SAFETY: plain getters and setters of the views, on the main thread.
            unsafe {
                let parent: *mut AnyObject = msg_send![&*self.view, superview];
                if parent.is_null() {
                    return;
                }
                let flipped: bool = msg_send![parent, isFlipped];
                let outer: NSRect = msg_send![parent, bounds];
                let (x, y) = (f64::from(f32::from(bounds.origin.x)), f64::from(f32::from(bounds.origin.y)));
                // Centred in the place it is given.
                let (w, h) = (f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)));
                let (x, y) = (x + (w - SIZE) / 2., y + (h - SIZE) / 2.);
                let y = if flipped { y } else { outer.size.height - y - SIZE };
                let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(SIZE, SIZE));
                let _: () = msg_send![&*self.view, setFrame: frame];
            }
        }

        pub fn set_hidden(&self, hidden: bool) {
            // SAFETY: a setter on the main thread.
            unsafe {
                let _: () = msg_send![&*self.view, setHidden: hidden];
            }
        }
    }

    impl Drop for Spinner {
        fn drop(&mut self) {
            // SAFETY: taken off the window on the main thread, where it was added.
            unsafe {
                let _: () = msg_send![&*self.view, removeFromSuperview];
            }
        }
    }
}
