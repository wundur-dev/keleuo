//! A compact strip of tabs that can be renamed in place: double-click, F2 or the context menu
//! turn the title into a text field with the text selected; Enter or clicking away saves,
//! Escape cancels. A manual name replaces the automatic one until "Reset name". Names are
//! saved in the shared [`NameStore`].

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::tabs::NameStore;

#[derive(Clone, Debug)]
pub struct TabSpec {
    pub id: u64,
    /// Key in the name store.
    pub key: String,
    pub auto_name: String,
    pub dirty: bool,
    pub tooltip: Option<String>,
    /// Shown with a terminal icon.
    pub terminal: bool,
}

/// A tab being dragged (to split the area or move it to the other half).
#[derive(Clone, Debug)]
pub struct DraggedTab {
    pub id: u64,
    pub name: String,
}

struct DragPreview(String);

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .text_xs()
            .bg(theme.secondary)
            .border_1()
            .border_color(theme.ring)
            .text_color(theme.foreground)
            .child(self.0.clone())
    }
}

pub enum TabStripEvent {
    Activate(u64),
    Close(u64),
}

pub struct TabStrip {
    id: SharedString,
    pub tabs: Vec<TabSpec>,
    pub active: Option<u64>,
    names: Rc<RefCell<NameStore>>,
    editing: Option<(u64, Entity<InputState>)>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<TabStripEvent> for TabStrip {}

impl TabStrip {
    pub fn new(id: &str, names: Rc<RefCell<NameStore>>) -> Self {
        Self {
            id: id.to_string().into(),
            tabs: Vec::new(),
            active: None,
            names,
            editing: None,
            _subs: Vec::new(),
        }
    }

    pub fn display_name(&self, tab: &TabSpec) -> String {
        self.names
            .borrow()
            .get(&tab.key)
            .map(str::to_string)
            .unwrap_or_else(|| tab.auto_name.clone())
    }

    pub fn is_renamed(&self, tab: &TabSpec) -> bool {
        self.names.borrow().get(&tab.key).is_some()
    }

    pub fn set_tabs(&mut self, tabs: Vec<TabSpec>, active: Option<u64>, cx: &mut Context<Self>) {
        self.tabs = tabs;
        self.active = active;
        cx.notify();
    }

    pub fn start_rename(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        let name = self.display_name(&tab);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |s, cx| {
            s.focus(window, cx);
            s.select_all(window, cx);
        });
        let sub = cx.subscribe_in(
            &input,
            window,
            move |strip, _, ev: &InputEvent, _window, cx| match ev {
                InputEvent::PressEnter { .. } | InputEvent::Blur => strip.commit(cx),
                _ => {}
            },
        );
        self._subs = vec![sub];
        self.editing = Some((id, input));
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        let Some((id, input)) = self.editing.take() else {
            return;
        };
        let text = input.read(cx).value().to_string();
        if let Some(tab) = self.tabs.iter().find(|t| t.id == id) {
            let text = text.trim();
            // Typing the automatic name back is the same as resetting.
            let value = if text == tab.auto_name { "" } else { text };
            let _ = self.names.borrow_mut().set(&tab.key, value);
        }
        self._subs.clear();
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self._subs.clear();
        cx.notify();
    }

    pub fn reset_name(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.iter().find(|t| t.id == id) {
            let _ = self.names.borrow_mut().reset(&tab.key);
        }
        cx.notify();
    }
}

impl Render for TabStrip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, active_bg, border, hover) = (
            theme.foreground,
            theme.muted_foreground,
            theme.tab_active,
            theme.border,
            theme.secondary_hover,
        );
        let entity = cx.entity().downgrade();
        let mut row = div()
            .id(self.id.clone())
            .h_full()
            .flex()
            .items_center()
            .overflow_x_scroll()
            .min_w_0();
        for tab in &self.tabs {
            let id = tab.id;
            let is_active = self.active == Some(id);
            let name = self.display_name(tab);
            let renamed = self.is_renamed(tab);
            let editing = self
                .editing
                .as_ref()
                .filter(|(eid, _)| *eid == id)
                .map(|(_, input)| input.clone());
            let e1 = entity.clone();
            let e2 = entity.clone();
            let e_menu = entity.clone();
            let mut el = div()
                .id(SharedString::from(format!("{}-{id}", self.id)))
                .group("tab")
                .h_full()
                .flex_none()
                .px_2()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .border_r_1()
                .border_color(border)
                .text_color(if is_active { fg } else { muted })
                .when(is_active, |d| d.bg(active_bg))
                .hover(|d| d.bg(hover))
                .on_drag(
                    DraggedTab {
                        id,
                        name: name.clone(),
                    },
                    |tab, _, _, cx| cx.new(|_| DragPreview(tab.name.clone())),
                )
                .when_some(tab.tooltip.clone(), |d, tip| {
                    d.tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
                })
                .on_click(move |ev: &ClickEvent, window, cx| {
                    let _ = e1.update(cx, |strip, cx| {
                        if ev.click_count() >= 2 {
                            // The strip sits in the title bar: keep the double click from zooming the window.
                            cx.stop_propagation();
                            strip.start_rename(id, window, cx);
                        } else {
                            cx.emit(TabStripEvent::Activate(id));
                        }
                    });
                });
            el = match editing {
                Some(input) => el.child(
                    div()
                        .w(px(140.))
                        .on_key_down(cx.listener(|strip, ev: &KeyDownEvent, _, cx| {
                            if ev.keystroke.key == "escape" {
                                strip.cancel(cx);
                                cx.stop_propagation();
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(Input::new(&input).xsmall()),
                ),
                None => el
                    .when(tab.terminal, |d| {
                        d.child(
                            Icon::new(IconName::SquareTerminal)
                                .xsmall()
                                .text_color(muted),
                        )
                    })
                    .child(
                        div()
                            .max_w(px(220.))
                            .truncate()
                            .when(renamed, |d| d.italic())
                            .child(name),
                    )
                    .when(tab.dirty, |d| {
                        d.child(div().size(px(6.)).rounded_full().bg(fg))
                    }),
            };
            el = el.child(
                div()
                    .id(SharedString::from(format!("{}-close-{id}", self.id)))
                    .rounded_sm()
                    .invisible()
                    .group_hover("tab", |d| d.visible())
                    .when(is_active, |d| d.visible())
                    .hover(|d| d.bg(border))
                    .child(Icon::new(IconName::Close).xsmall().text_color(muted))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = e2.update(cx, |_, cx| cx.emit(TabStripEvent::Close(id)));
                    }),
            );
            let el = el.context_menu(move |menu, _window, _cx| {
                let (a, b, c) = (e_menu.clone(), e_menu.clone(), e_menu.clone());
                menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                    let _ = a.update(cx, |s, cx| s.start_rename(id, window, cx));
                }))
                .item(PopupMenuItem::new("Reset name").on_click(move |_, _, cx| {
                    let _ = b.update(cx, |s, cx| s.reset_name(id, cx));
                }))
                .separator()
                .item(PopupMenuItem::new("Close").on_click(move |_, _, cx| {
                    let _ = c.update(cx, |_, cx| cx.emit(TabStripEvent::Close(id)));
                }))
            });
            row = row.child(el);
        }
        row
    }
}
