//! Whether something is on screen now, for a view that can skip work when it is not: each
//! drawn frame that shows it keeps a guard, inside one of that frame's mouse listeners. gpui
//! carries the listener into the next frame when it reuses the drawing and drops it with the
//! frame otherwise, so when the last guard goes nothing on screen shows the view (a tab
//! switched away, another session shown). The web panel does the same for its page.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{MouseMoveEvent, Window};

#[derive(Clone, Default)]
pub struct OnScreen(Rc<Cell<u32>>);

impl OnScreen {
    pub fn now(&self) -> bool {
        self.0.get() > 0
    }

    /// Call while painting what shows it.
    pub fn mark(&self, window: &mut Window) {
        let guard = Guard::new(self.0.clone());
        window.on_mouse_event(move |_: &MouseMoveEvent, _, _, _| {
            let _ = &guard;
        });
    }
}

struct Guard(Rc<Cell<u32>>);

impl Guard {
    fn new(count: Rc<Cell<u32>>) -> Self {
        count.set(count.get() + 1);
        Self(count)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}
