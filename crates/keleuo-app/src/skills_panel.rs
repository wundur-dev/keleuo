//! The skills panel: the groups of skills, agents, commands and instruction files the session's
//! backend reads for this project. Scanned on a background thread through the Host.

use std::collections::HashSet;
use crate::panels::{LIST_TEXT, ROW_HEIGHT};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::host::Host;
use keleuo_core::agent::Backend;
use keleuo_core::skills::{Catalog, Entry, Kind};

use crate::icons::{self, Source};

pub enum SkillsEvent {
    Open(Entry),
}

pub struct SkillsPanel {
    focus: FocusHandle,
    host: Arc<dyn Host>,
    backend: Arc<dyn Backend>,
    project: PathBuf,
    catalog: Option<Arc<Catalog>>,
    collapsed: HashSet<String>,
    pub selected: Option<PathBuf>,
    _task: Option<Task<()>>,
}

impl EventEmitter<SkillsEvent> for SkillsPanel {}

impl SkillsPanel {
    pub fn new(host: Arc<dyn Host>, backend: Arc<dyn Backend>, project: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            focus: cx.focus_handle(),
            host,
            backend,
            project,
            catalog: None,
            // Plugin groups start folded: there can be dozens of skills.
            collapsed: HashSet::new(),
            selected: None,
            _task: None,
        };
        this.refresh(None, cx);
        this
    }

    /// Rescan; when `open` is set, open that file once the scan is done.
    pub fn refresh(&mut self, open: Option<PathBuf>, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let backend = self.backend.clone();
        let project = self.project.clone();
        self._task = Some(
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let catalog = cx
                    .background_spawn(async move { backend.skills(host.as_ref(), &project) })
                    .await;
                let _ = this.update(cx, |panel, cx| {
                    if panel.catalog.is_none() {
                        for g in catalog.groups.iter().filter(|g| g.folded) {
                            panel.collapsed.insert(g.key.clone());
                        }
                    }
                    if let Some(path) = open
                        && let Some(entry) = catalog.all().find(|e| e.path == path).cloned()
                    {
                        panel.selected = Some(path);
                        cx.emit(SkillsEvent::Open(entry));
                    }
                    panel.catalog = Some(Arc::new(catalog));
                    cx.notify();
                });
            }),
        );
    }

    pub fn new_skill(&mut self, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let backend = self.backend.clone();
        let project = self.project.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let made = cx
                .background_spawn(async move { backend.create_skill(host.as_ref(), &project, "new skill") })
                .await;
            let _ = this.update(cx, |panel, cx| match made {
                Ok(path) => panel.refresh(Some(path), cx),
                Err(e) => eprintln!("could not create skill: {e}"),
            });
        })
        .detach();
    }

    fn group(
        &self,
        first: bool,
        key: &str,
        title: String,
        entries: &[Entry],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (fg, sel_bg, hover) = (
            theme.sidebar_foreground,
            theme.accent,
            theme.secondary_hover,
        );
        let source = Source::of_group(key);
        let tint = source.color(cx);
        let folded = self.collapsed.contains(key);
        let key_owned = key.to_string();
        let header = crate::panels::section_header(
            SharedString::from(format!("group-{key}")),
            Some(Icon::new(source.icon()).xsmall().text_color(tint.opacity(0.85))),
            title,
            Some(entries.len().to_string()),
            Some(!folded),
            first,
            cx,
        )
        .on_click(cx.listener(move |panel, _, _, cx| {
            if !panel.collapsed.remove(&key_owned) {
                panel.collapsed.insert(key_owned.clone());
            }
            cx.notify();
        }));
        let mut col = div().flex().flex_col().child(header);
        if !folded {
            for e in entries {
                let is_sel = self.selected.as_ref() == Some(&e.path);
                let entry = e.clone();
                let label = match e.kind {
                    Kind::Command => format!("/{}", e.name),
                    _ => e.name.clone(),
                };
                let description = SharedString::from(e.description.clone());
                col = col.child(
                    div()
                        .id(SharedString::from(e.path.to_string_lossy().to_string()))
                        .h(px(ROW_HEIGHT))
                        .pl(px(18.))
                        .pr_1()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_size(px(LIST_TEXT))
                        .text_color(if is_sel { theme.foreground } else { fg })
                        .when(is_sel, |d| d.bg(sel_bg))
                        .hover(|d| d.bg(hover))
                        .child(Icon::new(icons::entry(e.kind)).xsmall().text_color(tint))
                        .child(div().flex_1().min_w_0().truncate().child(label))
                        .when(!description.is_empty(), |d| {
                            d.tooltip(move |w, cx| Tooltip::new(description.clone()).build(w, cx))
                        })
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel.selected = Some(entry.path.clone());
                            cx.emit(SkillsEvent::Open(entry.clone()));
                            cx.notify();
                        })),
                );
            }
        }
        col.into_any_element()
    }
}

impl Focusable for SkillsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SkillsPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let mut list = div()
            .id("skills-list")
            .flex()
            .flex_col()
            .flex_1()
            .overflow_y_scroll();
        match self.catalog.clone() {
            None => {
                list = list.child(
                    div()
                        .p_2()
                        .text_size(px(LIST_TEXT))
                        .text_color(muted)
                        .child("Scanning skills..."),
                )
            }
            Some(cat) => {
                for (i, g) in cat.groups.iter().enumerate() {
                    list = list.child(self.group(i == 0, &g.key, g.title.clone(), &g.entries, cx));
                }
            }
        }
        let bg = crate::theme::tool_ground(cx);
        list.size_full().bg(bg)
    }
}
