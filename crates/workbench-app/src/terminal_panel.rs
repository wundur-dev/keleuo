//! The bottom panel: terminal tabs. Each tab's automatic name is the program in its foreground,
//! or the folder and git branch when the shell is idle; a manual name replaces it.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use workbench_core::host::Host;
use workbench_core::tabs::{NameStore, auto_name};

use crate::tab_strip::{TabSpec, TabStrip, TabStripEvent};
use crate::terminal_view::TerminalView;

struct Tab {
    id: u64,
    view: Entity<TerminalView>,
    _obs: Subscription,
}

pub struct TerminalPanel {
    host: Arc<dyn Host>,
    root: PathBuf,
    tabs: Vec<Tab>,
    active: Option<u64>,
    next_id: u64,
    pub strip: Entity<TabStrip>,
    branch: Option<String>,
    shell: String,
    _strip_sub: Subscription,
}

impl TerminalPanel {
    pub fn new(host: Arc<dyn Host>, root: PathBuf, names: Rc<RefCell<NameStore>>, cx: &mut Context<Self>) -> Self {
        let strip = cx.new(|_| TabStrip::new("term-tabs", names));
        let sub = cx.subscribe(&strip, |panel, _, ev: &TabStripEvent, cx| match ev {
            TabStripEvent::Activate(id) => {
                panel.active = Some(*id);
                panel.sync_strip(cx);
            }
            TabStripEvent::Close(id) => panel.close(*id, cx),
        });
        let mut this = Self {
            branch: None,
            shell: std::env::var("SHELL").unwrap_or_default(),
            host,
            root,
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            strip,
            _strip_sub: sub,
        };
        this.refresh_branch(cx);
        this.new_tab(cx);
        this
    }

    fn refresh_branch(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let root = self.root.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let branch = cx.background_spawn(async move { host.git_branch(&root) }).await;
            let _ = this.update(cx, |p, cx| {
                p.branch = branch;
                p.sync_strip(cx);
            });
        })
        .detach();
    }

    pub fn new_tab(&mut self, cx: &mut Context<Self>) {
        let spec = self.host.shell_command(&self.root);
        let view = cx.new(|cx| TerminalView::new(spec, cx));
        // Re-derive automatic names when the terminal redraws (the foreground program changes).
        let obs = cx.observe(&view, |panel, _, cx| panel.sync_strip(cx));
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab { id, view, _obs: obs });
        self.active = Some(id);
        self.sync_strip(cx);
    }

    fn close(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.tabs.iter().position(|t| t.id == id) else { return };
        self.tabs.remove(ix);
        if self.active == Some(id) {
            self.active = self.tabs.get(ix.min(self.tabs.len().saturating_sub(1))).map(|t| t.id);
        }
        self.sync_strip(cx);
        cx.notify();
    }

    fn sync_strip(&mut self, cx: &mut Context<Self>) {
        let tabs: Vec<TabSpec> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let view = t.view.read(cx);
                let program = view.pty.as_ref().and_then(|p| p.foreground_program());
                let mut name = auto_name(&self.root, self.branch.as_deref(), program.as_deref(), &self.shell);
                if view.exited {
                    name.push_str(" (exited)");
                }
                TabSpec {
                    id: t.id,
                    // Slot number, so a renamed first terminal keeps its name next time.
                    key: NameStore::key(&self.root, &format!("terminal:{}", i + 1)),
                    auto_name: name,
                    dirty: false,
                    tooltip: view.title.clone(),
                }
            })
            .collect();
        let changed = {
            let s = self.strip.read(cx);
            s.active != self.active
                || s.tabs.len() != tabs.len()
                || s.tabs.iter().zip(&tabs).any(|(a, b)| a.auto_name != b.auto_name || a.tooltip != b.tooltip)
        };
        if changed {
            let active = self.active;
            self.strip.update(cx, |s, cx| s.set_tabs(tabs, active, cx));
            cx.notify();
        }
    }

    pub fn active_view(&self) -> Option<Entity<TerminalView>> {
        self.tabs.iter().find(|t| Some(t.id) == self.active).map(|t| t.view.clone())
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        if let Some(view) = self.active_view() {
            let handle = view.read(cx).focus.clone();
            window.focus(&handle, cx);
        }
    }

    pub fn contains_focus(&self, window: &Window, cx: &App) -> bool {
        self.tabs.iter().any(|t| t.view.read(cx).focus.contains_focused(window, cx))
    }

    pub fn rename_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active {
            self.strip.update(cx, |s, cx| s.start_rename(id, window, cx));
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let header = div()
            .h(px(22.))
            .flex_none()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.tab_bar)
            .child(self.strip.clone())
            .child(
                div()
                    .id("new-terminal")
                    .px_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .hover(|d| d.bg(theme.secondary_hover))
                    .child(Icon::new(IconName::Plus).xsmall().text_color(theme.muted_foreground))
                    .on_click(cx.listener(|panel, _, window, cx| {
                        panel.new_tab(cx);
                        panel.focus(window, cx);
                    })),
            )
            .child(div().flex_1());
        let body = match self.active_view() {
            Some(view) => div().flex_1().min_h_0().child(view).into_any_element(),
            None => div()
                .flex_1()
                .p_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("No terminal. Press + to start one.")
                .into_any_element(),
        };
        div().size_full().flex().flex_col().child(header).child(body)
    }
}
