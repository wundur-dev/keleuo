//! A web page as a dock panel, drawn by the system's web view (WKWebView on macOS, the Safari
//! engine, through wry), not a bundled browser. A toolbar (back, forward, reload, the address,
//! open in the browser) over the page.
//!
//! The page is a native view laid over the window, not something gpui draws: it follows the
//! panel's place on screen, hides when nothing shows the panel (another tab, another session,
//! a zoomed group, the tab closed) and while one of the window's own overlays is open. gpui's
//! menus and tooltips cannot draw over it.
//!
//! The keyboard: clicking the page gives it the keys (the window's first responder); a click
//! anywhere else in the window, Cmd+L or moving the focus with a key takes them back. While the
//! page has them, Cmd+C, X, V, A, Z and Shift+Z go to the page.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::web;

use crate::actions::{FocusAddress, PageCopy, PageCut, PagePaste, PageRedo, PageSelectAll, PageUndo};

/// Tells the window when the page is clicked, so the panel takes the focus with it.
const CLICK_SCRIPT: &str =
    "window.addEventListener('mousedown', () => window.ipc.postMessage('focus'), true);";

pub enum WebEvent {
    /// The title or the address changed.
    Changed,
    /// The tab was closed.
    Closed,
    /// Its tab became the shown one.
    Activated,
    /// Send the page to the chat: its title, address and visible text.
    SendToChat { title: String, url: String, text: String },
}

/// The page's visible text, as the person sees it (not its markup).
const TEXT_SCRIPT: &str = "document.body ? document.body.innerText : ''";

/// Pages longer than this are cut (with a note), to keep a message reasonable.
const MAX_PAGE_TEXT: usize = 120_000;

/// What the web view reports, from its own callbacks into the panel.
enum PageEvent {
    Clicked,
    Loaded(String),
    Title(String),
    /// A link that asked for a new window: opened here instead.
    NewWindow(String),
    /// The page's text, asked for to send it to the chat (as the script returns it: JSON).
    Text(String),
}

pub struct WebPanel {
    /// The page shown (or being loaded); empty before the first address.
    pub url: String,
    title: Option<String>,
    address: Entity<InputState>,
    native: Option<Rc<Native>>,
    error: Option<String>,
    pub focus: FocusHandle,
    events: async_channel::Sender<PageEvent>,
    _task: Task<()>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WebEvent> for WebPanel {}

impl WebPanel {
    pub fn new(url: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let address = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Address: 3000, localhost:5173, example.com")
                .default_value(url.clone())
        });
        let focus = cx.focus_handle();
        let subs = vec![
            cx.subscribe_in(&address, window, |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => this.go(window, cx),
                InputEvent::Focus => this.release_keyboard(),
                _ => {}
            }),
            // The focus moved on (a key, another panel): the page gives the keys back.
            cx.on_blur(&focus, window, |this, _, _| this.release_keyboard()),
        ];
        let (tx, rx) = async_channel::unbounded();
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                if this.update_in(cx, |p, window, cx| p.page_event(ev, window, cx)).is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            url: String::new(),
            title: None,
            address,
            native: None,
            error: None,
            focus,
            events: tx,
            _task: task,
            _subs: subs,
        };
        if let Some(url) = web::normalize_url(&url) {
            this.load(url, window, cx);
        }
        this
    }

    /// The tab's name: the page's title, else its host.
    pub fn auto_name(&self) -> String {
        match &self.title {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ if self.url.is_empty() => "Web".into(),
            _ => web::short_name(&self.url),
        }
    }

    /// Load what the address field says.
    fn go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.address.read(cx).value().to_string();
        if let Some(url) = web::normalize_url(&text) {
            self.load(url, window, cx);
        }
    }

    pub fn load(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.url = url.clone();
        self.title = None;
        self.error = None;
        let shown = url.clone();
        self.address.update(cx, |a, cx| a.set_value(shown, window, cx));
        match &self.native {
            Some(n) => {
                if let Err(e) = n.view.load_url(&url) {
                    self.error = Some(format!("Could not load {url}: {e}"));
                }
            }
            None => match self.build(&url, window) {
                Ok(n) => self.native = Some(n),
                Err(e) => self.error = Some(format!("Could not start the web view: {e}")),
            },
        }
        cx.emit(WebEvent::Changed);
        cx.notify();
    }

    /// The web view, made on the first address so an empty panel starts no WebKit processes.
    fn build(&self, url: &str, window: &mut Window) -> Result<Rc<Native>, String> {
        let send = |tx: &async_channel::Sender<PageEvent>| {
            let tx = tx.clone();
            move |ev| {
                let _ = tx.try_send(ev);
            }
        };
        let (clicked, loaded, titled, opened) =
            (send(&self.events), send(&self.events), send(&self.events), send(&self.events));
        let view = wry::WebViewBuilder::new()
            .with_url(url)
            .with_visible(false)
            .with_devtools(true)
            .with_back_forward_navigation_gestures(true)
            .with_initialization_script(CLICK_SCRIPT)
            .with_ipc_handler(move |req| {
                if req.body() == "focus" {
                    clicked(PageEvent::Clicked);
                }
            })
            .with_on_page_load_handler(move |_, url| loaded(PageEvent::Loaded(url)))
            .with_document_title_changed_handler(move |t| titled(PageEvent::Title(t)))
            .with_new_window_req_handler(move |url, _| {
                opened(PageEvent::NewWindow(url));
                wry::NewWindowResponse::Deny
            })
            .build_as_child(&*window)
            .map_err(|e| e.to_string())?;
        Ok(Native::new(view))
    }

    fn page_event(&mut self, ev: PageEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            PageEvent::Clicked => {
                if let Some(n) = &self.native {
                    n.keyboard.set(true);
                }
                window.focus(&self.focus, cx);
            }
            PageEvent::Loaded(url) => {
                // A page that focuses its own field as it loads takes the window's keys: they
                // come back unless the person clicked into the page.
                if let Some(n) = &self.native
                    && !n.keyboard.get()
                {
                    n.take_keys_back();
                }
                if url != self.url && !url.is_empty() {
                    self.url = url.clone();
                    // Not under someone typing an address.
                    if !self.address.read(cx).focus_handle(cx).is_focused(window) {
                        self.address.update(cx, |a, cx| a.set_value(url, window, cx));
                    }
                    cx.emit(WebEvent::Changed);
                }
                cx.notify();
            }
            PageEvent::Title(t) => {
                self.title = Some(t);
                cx.emit(WebEvent::Changed);
                cx.notify();
            }
            PageEvent::NewWindow(url) => self.load(url, window, cx),
            PageEvent::Text(raw) => {
                let mut text = serde_json::from_str::<String>(&raw).unwrap_or(raw);
                if text.len() > MAX_PAGE_TEXT {
                    let cut = (0..=MAX_PAGE_TEXT).rev().find(|i| text.is_char_boundary(*i)).unwrap_or(0);
                    text.truncate(cut);
                    text.push_str("\n\n[The page goes on; cut here.]");
                }
                let title = self.title.clone().unwrap_or_default();
                cx.emit(WebEvent::SendToChat { title, url: self.url.clone(), text });
            }
        }
    }

    /// Ask the page for its text; it goes to the chat when it comes (see `PageEvent::Text`).
    pub fn send_to_chat(&mut self) {
        let Some(n) = &self.native else { return };
        let tx = self.events.clone();
        let _ = n.view.evaluate_script_with_callback(TEXT_SCRIPT, move |result| {
            let _ = tx.try_send(PageEvent::Text(result));
        });
    }

    fn back(&mut self) {
        if let Some(n) = &self.native {
            let _ = n.view.go_back();
        }
    }

    fn forward(&mut self) {
        if let Some(n) = &self.native {
            let _ = n.view.go_forward();
        }
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.native {
            Some(n) => {
                let _ = n.view.reload();
            }
            None => self.go(window, cx),
        }
    }

    fn open_in_browser(&mut self, cx: &mut Context<Self>) {
        if !self.url.is_empty() {
            cx.open_url(&self.url);
        }
    }

    /// Cmd+L: the address, selected.
    pub fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.release_keyboard();
        self.address.update(cx, |a, cx| {
            a.focus(window, cx);
            a.select_all(window, cx);
        });
    }

    fn release_keyboard(&self) {
        if let Some(n) = &self.native {
            n.release_keyboard();
        }
    }

    /// The tab was closed: the page goes at once, before the panel itself is dropped.
    pub fn closed(&mut self) {
        if let Some(n) = &self.native {
            n.hide();
        }
    }

    fn page_command(&self, selector: &str) {
        if let Some(n) = &self.native {
            n.command(selector);
        }
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let has_page = self.native.is_some();
        let (can_back, can_forward) = self
            .native
            .as_ref()
            .map(|n| (n.view.can_go_back().unwrap_or(true), n.view.can_go_forward().unwrap_or(true)))
            .unwrap_or((false, false));
        div()
            .flex_none()
            .h(px(34.))
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("web-back")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ArrowLeft)
                    .tooltip("Back")
                    .disabled(!can_back)
                    .on_click(cx.listener(|p, _, _, _| p.back())),
            )
            .child(
                Button::new("web-forward")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ArrowRight)
                    .tooltip("Forward")
                    .disabled(!can_forward)
                    .on_click(cx.listener(|p, _, _, _| p.forward())),
            )
            .child(
                Button::new("web-reload")
                    .ghost()
                    .xsmall()
                    .icon(IconName::RotateCw)
                    .tooltip("Reload")
                    .on_click(cx.listener(|p, _, window, cx| p.reload(window, cx))),
            )
            .child(div().flex_1().min_w_0().child(Input::new(&self.address).small()))
            .child(
                Button::new("web-send")
                    .ghost()
                    .xsmall()
                    .icon(gpui_kit::assets::IconName::MessageSquareShare)
                    .tooltip("Send page to chat")
                    .disabled(!has_page)
                    .on_click(cx.listener(|p, _, _, _| p.send_to_chat())),
            )
            .child(
                Button::new("web-open")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ExternalLink)
                    .tooltip("Open in the browser")
                    .disabled(!has_page)
                    .on_click(cx.listener(|p, _, _, cx| p.open_in_browser(cx))),
            )
    }
}

impl Focusable for WebPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for WebPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (bg, muted) = (theme.background, theme.muted_foreground);
        let message = match (&self.error, &self.native) {
            (Some(e), _) => Some(e.clone()),
            (None, None) => Some(
                "Type an address and press Enter: a port (3000) is this machine, a name without \
                 a scheme gets https://."
                    .to_string(),
            ),
            _ => None,
        };
        let native = self.native.clone().filter(|_| self.error.is_none());
        let page = div()
            .flex_1()
            .min_h_0()
            .relative()
            .children(message.map(|m| div().p_4().text_sm().text_color(muted).child(m)))
            .children(native.map(|n| {
                canvas(|_, _, _| (), move |bounds, _, window, _| n.paint(bounds, window))
                    .absolute()
                    .size_full()
            }));
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .track_focus(&self.focus)
            .key_context("WebPanel")
            .on_action(cx.listener(|p, _: &FocusAddress, window, cx| p.focus_address(window, cx)))
            .on_action(cx.listener(|p, _: &PageCopy, _, _| p.page_command("copy:")))
            .on_action(cx.listener(|p, _: &PageCut, _, _| p.page_command("cut:")))
            .on_action(cx.listener(|p, _: &PagePaste, _, _| p.page_command("paste:")))
            .on_action(cx.listener(|p, _: &PageSelectAll, _, _| p.page_command("selectAll:")))
            .on_action(cx.listener(|p, _: &PageUndo, _, _| p.page_command("undo:")))
            .on_action(cx.listener(|p, _: &PageRedo, _, _| p.page_command("redo:")))
            .child(self.toolbar(cx))
            .child(page)
    }
}

// ----- the native view -----

thread_local! {
    /// Every web view, for [`set_covered`].
    static NATIVES: RefCell<Vec<Weak<Native>>> = const { RefCell::new(Vec::new()) };
    /// One of the window's overlays is open over the content.
    static COVERED: Cell<bool> = const { Cell::new(false) };
}

/// One of the window's own overlays (a dialog, the session list peeking out) is open or closed:
/// web views hide meanwhile, since they would draw over it.
pub fn is_covered() -> bool {
    COVERED.with(Cell::get)
}

pub fn set_covered(covered: bool) {
    if COVERED.with(|c| c.replace(covered)) == covered {
        return;
    }
    let natives: Vec<Rc<Native>> = NATIVES.with(|n| {
        let mut n = n.borrow_mut();
        n.retain(|w| w.strong_count() > 0);
        n.iter().filter_map(Weak::upgrade).collect()
    });
    for n in natives {
        if covered {
            n.hide();
        } else if n.present.get() > 0 {
            n.show();
        }
    }
}

/// The WebKit processes the open web views name: each page's own (WebContent) and the one
/// that draws for them all (GPU). The networking one does not say; see `procstats`. These are
/// WebKit's private accessors, asked only when the web view answers them.
pub fn webkit_pids() -> Vec<i32> {
    let natives: Vec<Rc<Native>> =
        NATIVES.with(|n| n.borrow().iter().filter_map(Weak::upgrade).collect());
    let mut pids = Vec::new();
    for (i, n) in natives.iter().enumerate() {
        pids.extend(n.pid("_webProcessIdentifier", false));
        if i == 0 {
            pids.extend(n.pid("_gpuProcessIdentifier", true));
        }
    }
    pids
}

/// The web view and its place on screen.
struct Native {
    view: wry::WebView,
    /// Where the panel last laid the page out, in window coordinates.
    bounds: Cell<Option<Bounds<Pixels>>>,
    /// The bounds last given to the web view.
    placed: Cell<Option<Bounds<Pixels>>>,
    shown: Cell<bool>,
    /// Drawn frames that show the page (see [`Presence`]).
    present: Cell<usize>,
    /// The page has the keyboard: the web view is the window's first responder.
    keyboard: Cell<bool>,
}

impl Native {
    fn new(view: wry::WebView) -> Rc<Self> {
        let n = Rc::new(Self {
            view,
            bounds: Cell::new(None),
            placed: Cell::new(None),
            shown: Cell::new(false),
            present: Cell::new(0),
            keyboard: Cell::new(false),
        });
        NATIVES.with(|all| all.borrow_mut().push(Rc::downgrade(&n)));
        n
    }

    /// Drawn in a frame at `bounds`: show the page there, and hide it when no frame does.
    fn paint(self: &Rc<Self>, bounds: Bounds<Pixels>, window: &mut Window) {
        let presence = Presence::new(self);
        window.on_mouse_event(move |_: &MouseDownEvent, phase, _, _| {
            // The window got a click, so it was not on the page: the page gives up the keys,
            // also when it took them itself (a page focusing its own field) rather than by a
            // click on it.
            if phase == DispatchPhase::Capture
                && let Some(n) = presence.0.upgrade()
            {
                n.take_keys_back();
            }
        });
        self.bounds.set(Some(bounds));
        self.show();
    }

    fn show(&self) {
        let Some(b) = self.bounds.get() else {
            return;
        };
        if COVERED.with(Cell::get) {
            return;
        }
        if self.placed.get() != Some(b) {
            self.placed.set(Some(b));
            let rect = wry::Rect {
                position: wry::dpi::LogicalPosition::new(f32::from(b.origin.x) as f64, f32::from(b.origin.y) as f64).into(),
                size: wry::dpi::LogicalSize::new(f32::from(b.size.width) as f64, f32::from(b.size.height) as f64).into(),
            };
            let _ = self.view.set_bounds(rect);
        }
        if !self.shown.replace(true) {
            let _ = self.view.set_visible(true);
        }
    }

    fn hide(&self) {
        if self.shown.replace(false) {
            let _ = self.view.set_visible(false);
        }
        self.release_keyboard();
    }

    /// The window's own view takes the keys back from the page.
    fn release_keyboard(&self) {
        if self.keyboard.replace(false) {
            let _ = self.view.focus_parent();
        }
    }

    /// The window's own view takes the keys, whether or not the page was known to have them.
    fn take_keys_back(&self) {
        self.keyboard.set(false);
        let _ = self.view.focus_parent();
    }

    /// An editing command (`copy:`, `paste:`…) sent to the page, when it has one by that name.
    #[cfg(target_os = "macos")]
    fn command(&self, selector: &str) {
        use objc2::runtime::{AnyObject, Sel};
        use wry::WebViewExtMacOS as _;
        let Ok(name) = std::ffi::CString::new(selector) else {
            return;
        };
        let sel = Sel::register(&name);
        let view = self.view.webview();
        // SAFETY: an NSObject asked whether it answers `sel`, then sent it with a nil sender,
        // which is how menu items send these.
        unsafe {
            let answers: objc2::runtime::Bool = objc2::msg_send![&*view, respondsToSelector: sel];
            if answers.as_bool() {
                let _: *mut AnyObject =
                    objc2::msg_send![&*view, performSelector: sel, withObject: std::ptr::null::<AnyObject>()];
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn command(&self, _: &str) {}

    /// A process id the web view (or, with `pool`, its process pool) gives by this selector.
    #[cfg(target_os = "macos")]
    fn pid(&self, selector: &str, pool: bool) -> Option<i32> {
        use objc2::runtime::{AnyObject, Sel};
        use wry::WebViewExtMacOS as _;
        let name = std::ffi::CString::new(selector).ok()?;
        let sel = Sel::register(&name);
        let view = self.view.webview();
        // SAFETY: messages to live Objective-C objects on the main thread: the view's
        // configuration and process pool (both WKWebView API), then `sel`, which returns a
        // pid_t, only when the object answers it.
        unsafe {
            let target: *mut AnyObject = if pool {
                let config: *mut AnyObject = objc2::msg_send![&*view, configuration];
                if config.is_null() {
                    return None;
                }
                objc2::msg_send![config, processPool]
            } else {
                &*view as *const _ as *mut AnyObject
            };
            if target.is_null() {
                return None;
            }
            let answers: objc2::runtime::Bool = objc2::msg_send![target, respondsToSelector: sel];
            if !answers.as_bool() {
                return None;
            }
            let pid: i32 = objc2::runtime::MessageReceiver::send_message(target, sel, ());
            (pid > 0).then_some(pid)
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn pid(&self, _: &str, _: bool) -> Option<i32> {
        None
    }
}

/// Kept by a drawn frame while it shows the page, inside one of that frame's mouse listeners:
/// gpui carries the listener into the next frame when it reuses the panel's drawing and drops
/// it with the frame otherwise. When the last one goes, nothing on screen shows the panel (a
/// tab switched away, a zoom, another session), so the page hides. A fresh drawing makes its
/// own before the old frame's goes.
struct Presence(Weak<Native>);

impl Presence {
    fn new(native: &Rc<Native>) -> Self {
        native.present.set(native.present.get() + 1);
        Self(Rc::downgrade(native))
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        if let Some(n) = self.0.upgrade() {
            let left = n.present.get().saturating_sub(1);
            n.present.set(left);
            if left == 0 {
                n.hide();
            }
        }
    }
}
