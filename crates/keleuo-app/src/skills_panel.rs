//! The skills panel: the groups of skills, agents, commands and instruction files the session's
//! backend reads for this project. Scanned on a background thread through the Host.

use std::collections::HashSet;
use crate::panels::{LIST_TEXT, ROW_HEIGHT};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
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
    /// What Keleuo installed (to say where a skill came from, and to update or remove it).
    installed: Arc<keleuo_core::skill_install::Record>,
    collapsed: HashSet<String>,
    pub selected: Option<PathBuf>,
    /// A name being typed: for a new skill, or to rename one (its SKILL.md).
    naming: Option<Naming>,
    _task: Option<Task<()>>,
}

impl EventEmitter<SkillsEvent> for SkillsPanel {}

struct Naming {
    input: Entity<InputState>,
    /// `None`: a new skill; else the SKILL.md of the one renamed.
    renaming: Option<PathBuf>,
    problem: Option<String>,
    _events: Subscription,
}

impl SkillsPanel {
    pub fn new(host: Arc<dyn Host>, backend: Arc<dyn Backend>, project: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            focus: cx.focus_handle(),
            installed: Arc::new(keleuo_core::skill_install::Record::default()),
            naming: None,
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
                let (catalog, installed) = cx
                    .background_spawn(async move {
                        (backend.skills(host.as_ref(), &project), keleuo_core::skill_install::Record::load())
                    })
                    .await;
                let _ = this.update(cx, |panel, cx| {
                    panel.installed = Arc::new(installed);
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

    /// Open the installer for this project (`source`: fetch it at once, to update a skill).
    pub fn install_from_git(&mut self, source: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let me = cx.entity().downgrade();
        crate::skill_installer::open(
            self.host.clone(),
            self.project.clone(),
            source,
            move |cx| {
                let _ = me.update(cx, |p, cx| p.refresh(None, cx));
            },
            window,
            cx,
        );
    }

    /// The machine this panel's skills are on, as the install record names it.
    fn host_name(&self) -> String {
        if self.host.is_remote() { self.host.label() } else { "local".into() }
    }

    /// Ask for a name (a field at the top of the list), for a new skill or (`renaming`: its
    /// SKILL.md) to rename one.
    pub fn ask_name(&mut self, renaming: Option<PathBuf>, current: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("The skill's name").default_value(current.to_string()));
        let events = cx.subscribe_in(&input, window, |this: &mut Self, _, ev: &InputEvent, window, cx| match ev {
            InputEvent::PressEnter { .. } => this.commit_name(window, cx),
            InputEvent::Blur => {
                this.naming = None;
                cx.notify();
            }
            _ => {}
        });
        input.update(cx, |s, cx| s.focus(window, cx));
        self.naming = Some(Naming { input, renaming, problem: None, _events: events });
        cx.notify();
    }

    fn commit_name(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(naming) = &self.naming else { return };
        let name = naming.input.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.naming = None;
            cx.notify();
            return;
        }
        let renaming = naming.renaming.clone();
        let (host, backend, project) = (self.host.clone(), self.backend.clone(), self.project.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let made = cx
                .background_spawn(async move {
                    match renaming {
                        Some(skill_md) => keleuo_core::skill_install::rename_skill(host.as_ref(), &skill_md, &name),
                        None => backend.create_skill(host.as_ref(), &project, &name).map_err(anyhow::Error::from),
                    }
                })
                .await;
            let _ = this.update(cx, |panel, cx| match made {
                Ok(path) => {
                    panel.naming = None;
                    panel.refresh(Some(path), cx);
                }
                Err(e) => {
                    if let Some(n) = &mut panel.naming {
                        n.problem = Some(format!("{e:#}"));
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn new_skill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask_name(None, "", window, cx);
    }

    /// Take a skill away, once the person says so: into the Trash (deleted over SSH).
    fn discard(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        let remote = self.host.is_remote();
        let folder = entry.path.parent().map(|p| p.display().to_string()).unwrap_or_default();
        let detail = if remote {
            format!("Deletes {folder} on {} (there is no Trash there).", self.host.label())
        } else {
            format!("Moves {folder} to the Trash.")
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove the {} skill?", entry.name),
            Some(&detail),
            &[PromptButton::Ok("Remove".into()), PromptButton::Cancel("Cancel".into())],
            cx,
        );
        let host = self.host.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            if answer.await.ok() != Some(0) {
                return;
            }
            let path = entry.path.clone();
            let result = cx.background_spawn(async move { keleuo_core::skill_install::discard_skill(host.as_ref(), &path) }).await;
            let _ = this.update(cx, |panel, cx| {
                if let Err(e) = result {
                    eprintln!("keleuo: could not remove the skill: {e:#}");
                }
                panel.refresh(None, cx);
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
                let from = self.installed.holding(&self.host_name(), &e.path).cloned();
                let description = SharedString::from(match &from {
                    Some(i) => format!("{}\n\nInstalled from {} on {}", e.description, i.source, i.installed).trim().to_string(),
                    None => e.description.clone(),
                });
                let me = cx.entity().downgrade();
                // A skill of the person's own: icons on hover to rename and remove it (and update it,
                // when Keleuo installed it). Right-click has the same.
                let row_group = SharedString::from(format!("skill-row-{}", e.path.display()));
                let hover_icons = (e.kind == Kind::Skill && !e.read_only).then(|| {
                    let icon = |id: &str, name: gpui_kit::assets::IconName, tip: String| {
                        Button::new(SharedString::from(format!("{id}-{}", e.path.display()))).icon(name).xsmall().ghost().tooltip(tip)
                    };
                    let (a, b, c) = (me.clone(), me.clone(), me.clone());
                    let (ren, rem) = (e.clone(), e.clone());
                    div()
                        .flex_none()
                        .flex()
                        .invisible()
                        .group_hover(row_group.clone(), |d| d.visible())
                        .children(from.clone().map(|installed| {
                            let source = update_source(&installed);
                            icon("skill-update", gpui_kit::assets::IconName::RefreshCw, format!("Update from {}", installed.source)).on_click(
                                move |_, window, cx| {
                                    cx.stop_propagation();
                                    let _ = c.update(cx, |p, cx| p.install_from_git(Some(source.clone()), window, cx));
                                },
                            )
                        }))
                        .child(icon("skill-rename", gpui_kit::assets::IconName::Pencil, "Rename".into()).on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let _ = a.update(cx, |p, cx| p.ask_name(Some(ren.path.clone()), &ren.name, window, cx));
                        }))
                        .child(icon("skill-remove", gpui_kit::assets::IconName::Trash, "Remove".into()).on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            let _ = b.update(cx, |p, cx| p.discard(rem.clone(), window, cx));
                        }))
                });
                let row = div()
                        .group(row_group)
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
                        .children(hover_icons)
                        .when(!description.is_empty(), |d| {
                            d.tooltip(move |w, cx| Tooltip::new(description.clone()).build(w, cx))
                        })
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel.selected = Some(entry.path.clone());
                            cx.emit(SkillsEvent::Open(entry.clone()));
                            cx.notify();
                        }))
                        ;
                // A skill of the person's own: rename it, remove it; installed by Keleuo: update it
                // from its source too.
                let own = e.kind == Kind::Skill && !e.read_only;
                let menu_entry = e.clone();
                col = col.child(if !own {
                    row.into_any_element()
                } else {
                    row.context_menu(move |menu, _, _| {
                        let (a, b, c) = (me.clone(), me.clone(), me.clone());
                        let (ren, rem) = (menu_entry.clone(), menu_entry.clone());
                        let menu = match from.clone() {
                            Some(installed) => {
                                let source = update_source(&installed);
                                menu.item(PopupMenuItem::new(format!("Update from {}…", installed.source)).on_click(move |_, window, cx| {
                                    let _ = c.update(cx, |p, cx| p.install_from_git(Some(source.clone()), window, cx));
                                }))
                                .separator()
                            }
                            None => menu,
                        };
                        menu.item(PopupMenuItem::new("Rename…").on_click(move |_, window, cx| {
                            let _ = a.update(cx, |p, cx| p.ask_name(Some(ren.path.clone()), &ren.name, window, cx));
                        }))
                        .item(PopupMenuItem::new("Remove…").on_click(move |_, window, cx| {
                            let _ = b.update(cx, |p, cx| p.discard(rem.clone(), window, cx));
                        }))
                    })
                    .into_any_element()
                });
            }
        }
        col.into_any_element()
    }
}

/// What to fetch to update `skill`: its repository and folder (`owner/repo/path` on GitHub).
fn update_source(skill: &keleuo_core::skill_install::Installed) -> String {
    match skill.url.strip_prefix("https://github.com/") {
        Some(repo) => {
            let repo = repo.trim_end_matches(".git");
            if skill.path.is_empty() { repo.to_string() } else { format!("{repo}/{}", skill.path) }
        }
        None => skill.url.clone(),
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
        let red = cx.theme().red;
        // What can be done here, said in words: a new skill, or skills from a repository.
        let actions = div()
            .flex_none()
            .px(px(8.))
            .pt(px(6.))
            .pb(px(2.))
            .flex()
            .gap_1()
            .child(
                Button::new("skills-new")
                    .label("New skill")
                    .icon(IconName::Plus)
                    .xsmall()
                    .ghost()
                    .on_click(cx.listener(|p, _, window, cx| p.new_skill(window, cx))),
            )
            .child(
                Button::new("skills-install")
                    .label("Install from GitHub…")
                    .icon(gpui_kit::assets::IconName::Download)
                    .xsmall()
                    .ghost()
                    .tooltip("Any git repository: a URL, owner/repo, or a GitHub folder link")
                    .on_click(cx.listener(|p, _, window, cx| p.install_from_git(None, window, cx))),
            );
        let naming = self.naming.as_ref().map(|n| {
            let title = if n.renaming.is_some() { "Rename to" } else { "New skill" };
            div()
                .flex_none()
                .px(px(10.))
                .py(px(4.))
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_size(px(12.)).text_color(muted).child(format!("{title} (Enter to save, Esc to cancel)")))
                .child(
                    div()
                        .capture_key_down(cx.listener(|p, ev: &KeyDownEvent, _, cx| {
                            if ev.keystroke.key == "escape" {
                                p.naming = None;
                                cx.stop_propagation();
                                cx.notify();
                            }
                        }))
                        .child(Input::new(&n.input).small()),
                )
                .children(n.problem.clone().map(|p| div().text_size(px(12.)).text_color(red).child(p)))
        });
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
        div().size_full().flex().flex_col().bg(bg).child(actions).children(naming).child(list)
    }
}
