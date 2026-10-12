//! Everything in the window is a dock panel: the Files, Skills, Terminals and Agent (subagents)
//! lists, the session's chat, every open file, every terminal and every web page. This module
//! gives each the two dock traits (behavior from gpui-base, presentation from gpui-component),
//! the names shown on their tabs, and in-place renaming of a tab.

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{BasePanel, Panel, PanelEvent, PanelInfo, PanelState};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::PopupMenu;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::tabs::NameStore;

use crate::chat_panel::{ChatPanel, ChatPanelEvent};
use crate::agent_side::AgentSide;
use crate::doc_panel::{DocEvent, DocPanel};
use crate::file_tree::FileTree;
use crate::skills_panel::SkillsPanel;
use crate::terminal_view::{TerminalEvent, TerminalView};
use crate::web_panel::{WebEvent, WebPanel};

/// The tab names people gave, shared by every window.
pub struct Names(pub Rc<RefCell<NameStore>>);

impl Global for Names {}

/// A tab being renamed in place: its name key, automatic name and field.
pub struct TabRename {
    pub key: String,
    pub auto: String,
    pub input: Entity<InputState>,
    _sub: Subscription,
}

#[derive(Default)]
pub struct Renaming(pub Option<TabRename>);

impl Global for Renaming {}

/// Start renaming the tab with this name key: its title becomes a text field.
pub fn start_tab_rename(
    key: String,
    auto: String,
    current: String,
    window: &mut Window,
    cx: &mut App,
) {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
    input.update(cx, |s, cx| {
        s.focus(window, cx);
        s.select_all(window, cx);
    });
    let sub = window.subscribe(&input, cx, |input, ev: &InputEvent, window, cx| match ev {
        InputEvent::PressEnter { .. } | InputEvent::Blur => {
            let Some(r) = cx.global_mut::<Renaming>().0.take() else {
                return;
            };
            let text = input.read(cx).value().trim().to_string();
            let value = if text == r.auto { String::new() } else { text };
            let _ = cx.global::<Names>().0.borrow_mut().set(&r.key, &value);
            window.refresh();
        }
        _ => {}
    });
    cx.global_mut::<Renaming>().0 = Some(TabRename {
        key,
        auto,
        input,
        _sub: sub,
    });
    window.refresh();
}

pub fn reset_tab_name(key: &str, window: &mut Window, cx: &mut App) {
    let _ = cx.global::<Names>().0.borrow_mut().reset(key);
    window.refresh();
}

/// The name on a tab: the given one, else the automatic one.
pub fn tab_label(key: &str, auto: String, cx: &App) -> String {
    cx.global::<Names>()
        .0
        .borrow()
        .get(key)
        .map(str::to_string)
        .unwrap_or(auto)
}

/// A tab title: the name (italic when given by hand), an optional dot, or the rename field.
pub fn title_element(
    key: &str,
    auto: String,
    dot: Option<Hsla>,
    icon: Option<Icon>,
    cx: &App,
) -> AnyElement {
    if let Some(r) = cx.global::<Renaming>().0.as_ref().filter(|r| r.key == key) {
        return div()
            .w(px(150.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(Input::new(&r.input).xsmall())
            .into_any_element();
    }
    let renamed = cx.global::<Names>().0.borrow().get(key).is_some();
    let label = tab_label(key, auto, cx);
    div()
        .flex()
        .items_center()
        .gap_1()
        .children(icon)
        .child(div().when(renamed, |d| d.italic()).child(label))
        .children(dot.map(|c| div().size(px(6.)).rounded_full().bg(c)))
        .into_any_element()
}

/// "Rename" and "Reset name" in a tab's ellipsis menu.
fn rename_menu(menu: PopupMenu, key: String, auto: String, cx: &App) -> PopupMenu {
    let current = tab_label(&key, auto.clone(), cx);
    let (k1, k2) = (key.clone(), key);
    menu.item(
        gpui_kit::component::menu::PopupMenuItem::new("Rename (F2)").on_click(
            move |_, window, cx| {
                start_tab_rename(k1.clone(), auto.clone(), current.clone(), window, cx)
            },
        ),
    )
    .item(
        gpui_kit::component::menu::PopupMenuItem::new("Reset name")
            .on_click(move |_, window, cx| reset_tab_name(&k2, window, cx)),
    )
}

// ----- the four list panels -----

macro_rules! list_panel {
    ($ty:ty, $name:literal, $title:literal, $icon:expr) => {
        impl EventEmitter<PanelEvent> for $ty {}

        impl BasePanel for $ty {
            fn panel_name(&self) -> &'static str {
                $name
            }
            fn closable(&self, _: &App) -> bool {
                false
            }
            fn zoomable(&self, _: &App) -> bool {
                false
            }
        }
    };
}

list_panel!(FileTree, "Files", "Files", IconName::FolderOpen);
list_panel!(SkillsPanel, "Skills", "Skills", IconName::BookOpen);
list_panel!(AgentSide, "Agent", "Agent", IconName::Bot);

/// A quiet section header in a sidebar list: an icon, the title, a faint count and, when the
/// section folds, a chevron. Space above it separates groups (none for the first); the rows
/// below stay tight.
pub fn section_header(
    id: SharedString,
    icon: Option<Icon>,
    title: impl Into<SharedString>,
    count: Option<String>,
    open: Option<bool>,
    first: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    let (muted, fg, hover) = (theme.muted_foreground, theme.foreground, theme.secondary_hover);
    div()
        .id(id)
        .h(px(20.))
        .mt(px(if first { 3. } else { 9. }))
        .pl(px(6.))
        .pr(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(10.5))
        .font_weight(FontWeight::MEDIUM)
        .text_color(muted.opacity(0.85))
        .hover(|d| d.bg(hover).text_color(fg))
        .children(icon)
        .child(div().flex_1().min_w_0().truncate().child(title.into()))
        .children(count.map(|c| div().flex_none().text_color(muted.opacity(0.6)).child(c)))
        .children(open.map(|open| {
            Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                .xsmall()
                .text_color(muted.opacity(0.6))
        }))
}

/// A session id, short (its first 8 characters) and quiet; the whole id in the tooltip, and a
/// click copies it.
pub fn short_id(id: String, key: impl Into<ElementId>, cx: &App) -> Stateful<Div> {
    let muted = cx.theme().muted_foreground;
    let short: String = id.chars().take(8).collect();
    let tip = SharedString::from(format!("Session {id}. Click to copy."));
    div()
        .id(key.into())
        .flex_none()
        .px(px(3.))
        .rounded_sm()
        .font_family(crate::terminal_view::FONT_FAMILY)
        .text_size(px(10.))
        .text_color(muted.opacity(0.75))
        .cursor_pointer()
        .hover(|d| d.text_color(cx.theme().foreground))
        .child(short)
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            cx.write_to_clipboard(ClipboardItem::new_string(id.clone()));
            gpui_kit::component::WindowExt::push_notification(window, "Session id copied", cx);
        })
}

fn list_title(icon: IconName, title: &'static str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(Icon::new(icon).xsmall())
        .child(title)
        .into_any_element()
}

impl Panel for FileTree {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        list_title(IconName::FolderOpen, "Files")
    }
    fn zoom_control(&self, _: &App) -> Option<gpui_kit::component::dock::PanelControl> {
        None
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Panel for SkillsPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        list_title(IconName::BookOpen, "Skills")
    }
    fn toolbar_buttons(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<Vec<Button>> {
        Some(vec![
            Button::new("new-skill")
                .ghost()
                .xsmall()
                .icon(IconName::Plus)
                .tooltip("New project skill")
                .on_click(cx.listener(|s, _, _, cx| s.new_skill(cx))),
        ])
    }
    fn zoom_control(&self, _: &App) -> Option<gpui_kit::component::dock::PanelControl> {
        None
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Panel for AgentSide {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        list_title(IconName::Bot, "Agent")
    }
    fn zoom_control(&self, _: &App) -> Option<gpui_kit::component::dock::PanelControl> {
        None
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

// ----- sessions, files, terminals -----

/// The name key of a session tab.
pub fn session_key(number: usize) -> String {
    format!("session-tab:{number}")
}

impl EventEmitter<PanelEvent> for ChatPanel {}

impl BasePanel for ChatPanel {
    fn panel_name(&self) -> &'static str {
        if self.agent.is_some() { "Subagent" } else { "Chat" }
    }
    /// The session's conversation stays: close the session tab instead. A watched subagent
    /// is a view of its work, so it closes.
    fn closable(&self, _: &App) -> bool {
        self.agent.is_some()
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(ChatPanelEvent::Closed);
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if active {
            cx.emit(ChatPanelEvent::Activated);
        }
    }
    fn dump(&self, cx: &App) -> PanelState {
        let mut s = PanelState::new(self.panel_name());
        if let Some(id) = &self.agent {
            s.info = PanelInfo::panel(serde_json::json!({ "tool_use_id": id }));
            return s;
        }
        let session = self.session.read(cx);
        // The backend is saved with the session, so it reopens with the same agent.
        s.info = PanelInfo::panel(serde_json::json!({
            "session_id": session.chat.session_id,
            "backend": session.backend.id(),
            "number": self.number,
        }));
        s
    }
}

impl Panel for ChatPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(id) = self.agent.clone() {
            let label = self
                .session
                .read(cx)
                .chat
                .agent(&id)
                .map(|a| a.description.clone())
                .unwrap_or_else(|| "Subagent".into());
            return div().flex().items_center().gap_1().child(Icon::new(IconName::Bot).xsmall()).child(label);
        }
        let theme = cx.theme();
        let (fg, blue, amber) = (theme.foreground, theme.blue, theme.warning);
        let s = self.session.read(cx);
        let dot = if s.chat.needs_input() {
            Some(amber)
        } else if s.chat.busy {
            Some(blue)
        } else {
            None
        };
        let _ = fg;
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(Icon::new(IconName::Bot).xsmall())
            .child(s.backend.name())
            .children(dot.map(|c| div().size(px(6.)).rounded_full().bg(c)))
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for DocPanel {}

impl BasePanel for DocPanel {
    fn panel_name(&self) -> &'static str {
        "Doc"
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DocEvent::Closed);
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if active {
            cx.emit(DocEvent::Activated);
        }
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut s = PanelState::new(self.panel_name());
        s.info =
            PanelInfo::panel(serde_json::json!({ "path": self.path, "read_only": self.read_only }));
        s
    }
}

pub fn doc_key(doc: &DocPanel) -> String {
    NameStore::key(&doc.root(), &format!("file:{}", doc.path.display()))
}

impl Panel for DocPanel {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dot = self.dirty.then(|| cx.theme().foreground);
        let icon = self.path.file_name().map(|n| crate::icons::file(&n.to_string_lossy(), cx));
        title_element(&doc_key(self), self.auto_name(), dot, icon, cx)
    }
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        rename_menu(menu, doc_key(self), self.auto_name(), cx)
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for TerminalView {}

pub fn term_key(t: &TerminalView) -> String {
    NameStore::key(&t.folder, &format!("terminal:{}", t.number))
}

impl BasePanel for TerminalView {
    fn panel_name(&self) -> &'static str {
        "Terminal"
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(TerminalEvent::Closed);
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if active {
            self.unseen = false;
            cx.emit(TerminalEvent::Activated);
        }
    }
}

impl Panel for TerminalView {
    fn title(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let dot = self.unseen.then(|| cx.theme().foreground);
        title_element(
            &term_key(self),
            self.auto_name(),
            dot,
            Some(Icon::new(IconName::SquareTerminal).xsmall()),
            cx,
        )
    }
    fn dropdown_menu(
        &mut self,
        menu: PopupMenu,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        rename_menu(menu, term_key(self), self.auto_name(), cx)
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl EventEmitter<PanelEvent> for WebPanel {}

impl BasePanel for WebPanel {
    fn panel_name(&self) -> &'static str {
        "Web"
    }
    fn on_removed(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.closed();
        cx.emit(WebEvent::Closed);
    }
    fn set_active(&mut self, active: bool, _: &mut Window, cx: &mut Context<Self>) {
        if active {
            cx.emit(WebEvent::Activated);
        }
    }
    fn dump(&self, _: &App) -> PanelState {
        let mut s = PanelState::new(self.panel_name());
        s.info = PanelInfo::panel(serde_json::json!({ "url": self.url }));
        s
    }
}

impl Panel for WebPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(Icon::new(IconName::Globe).xsmall())
            .child(div().max_w(px(180.)).truncate().child(self.auto_name()))
    }
    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}
