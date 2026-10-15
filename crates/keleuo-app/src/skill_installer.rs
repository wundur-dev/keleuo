//! The skill installer (Skills tab, "+" › "Install from git…"): a sheet over the window. Paste
//! a git URL or `owner/repo`, see the skills in it and read each one, choose the scope and the
//! agents, install. Each chosen agent gets a copy in the folder it reads first, unless it
//! already reads another chosen agent's copy (Cursor reads Claude Code's); Keleuo records what
//! it installed from where (see [`keleuo_core::skill_install`]).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::agent::Backend;
use keleuo_core::host::Host;
use keleuo_core::skill_install::{self as install, Fetched, Found, Installed, InstalledCopy, Scope};

/// Each agent to copy into (by its place in the list) and the folder its copy goes in.
type Copies = Vec<(usize, PathBuf)>;
/// Each agent that reads another's copy, and that other.
type Covered = Vec<(usize, usize)>;

/// The open installer, if any.
#[derive(Default)]
struct Open(Option<Entity<SkillInstaller>>);
impl Global for Open {}

enum Stage {
    Idle,
    Fetching,
    Failed(String),
    Fetched { fetched: Arc<Fetched>, found: Vec<Found> },
}

pub struct SkillInstaller {
    host: Arc<dyn Host>,
    project: PathBuf,
    agents: Vec<Arc<dyn Backend>>,
    /// Each agent's skill folders, by scope (read off the UI thread when it opens).
    folders: HashMap<(usize, Scope), Vec<PathBuf>>,
    input: Entity<InputState>,
    stage: Stage,
    chosen: HashSet<usize>,
    shown: usize,
    scope: Scope,
    agents_on: Vec<bool>,
    /// Folders already there that installing would replace, once the person was told.
    replace: bool,
    conflicts: Vec<PathBuf>,
    installing: bool,
    outcome: Option<Result<String, String>>,
    on_done: Rc<dyn Fn(&mut App)>,
    _input_events: Subscription,
}

use std::rc::Rc;

/// Open the installer for the session's `project` on `host`; `on_done` runs after an install
/// (the Skills tab reads its folders again). `source`: fetch this at once (an update).
pub fn open(
    host: Arc<dyn Host>,
    project: PathBuf,
    source: Option<String>,
    on_done: impl Fn(&mut App) + 'static,
    window: &mut Window,
    cx: &mut App,
) {
    let agents: Vec<Arc<dyn Backend>> = crate::backends::all(cx);
    let entity = cx.new(|cx| {
        let input = cx.new(|cx| {
            let s = InputState::new(window, cx).placeholder("https://github.com/owner/repo, or owner/repo");
            match &source {
                Some(src) => s.default_value(src.clone()),
                None => s,
            }
        });
        let sub = cx.subscribe_in(&input, window, |this: &mut SkillInstaller, _, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { .. } = ev {
                this.fetch(window, cx);
            }
        });
        let n = agents.len();
        let mut this = SkillInstaller {
            host: host.clone(),
            project: project.clone(),
            agents: agents.clone(),
            folders: HashMap::new(),
            input,
            stage: Stage::Idle,
            chosen: HashSet::new(),
            shown: 0,
            scope: Scope::User,
            agents_on: vec![true; n],
            replace: false,
            conflicts: Vec::new(),
            installing: false,
            outcome: None,
            on_done: Rc::new(on_done),
            _input_events: sub,
        };
        this.read_folders(cx);
        this
    });
    entity.update(cx, |this, cx| {
        this.input.update(cx, |s, cx| s.focus(window, cx));
        if source.is_some() {
            this.fetch(window, cx);
        }
    });
    cx.set_global(Open(Some(entity)));
    cx.refresh_windows();
}

/// Tests: `scope project|user`, `install`, or `state` (what it shows).
pub fn test_command(arg: &str, cx: &mut App) -> String {
    let Some(entity) = cx.try_global::<Open>().and_then(|o| o.0.clone()) else { return "not open".into() };
    match arg {
        "scope project" | "scope user" => {
            entity.update(cx, |this, cx| {
                this.scope = if arg.ends_with("project") { Scope::Project } else { Scope::User };
                cx.notify();
            });
            "ok".into()
        }
        "install" => {
            entity.update(cx, |this, cx| {
                if !this.conflicts.is_empty() {
                    this.replace = true;
                }
                this.install(cx)
            });
            "ok".into()
        }
        _ => {
            let this = entity.read(cx);
            let stage = match &this.stage {
                Stage::Idle => "idle".to_string(),
                Stage::Fetching => "fetching".into(),
                Stage::Failed(e) => format!("failed: {e}"),
                Stage::Fetched { fetched, found } => format!(
                    "fetched {} at {}: {:?}",
                    fetched.spec.label,
                    &fetched.commit[..7.min(fetched.commit.len())],
                    found.iter().map(|f| f.name.as_str()).collect::<Vec<_>>()
                ),
            };
            let (copies, covered) = this.targets();
            format!(
                "{stage}; copies {:?}; covered {:?}; outcome {:?}",
                copies.iter().map(|(a, d)| format!("{}:{}", this.agents[*a].id(), d.display())).collect::<Vec<_>>(),
                covered.iter().map(|(a, by)| format!("{}<-{}", this.agents[*a].id(), this.agents[*by].id())).collect::<Vec<_>>(),
                this.outcome
            )
        }
    }
}

pub fn close(cx: &mut App) {
    cx.set_global(Open(None));
    cx.refresh_windows();
}

pub fn is_open(cx: &App) -> bool {
    cx.try_global::<Open>().is_some_and(|o| o.0.is_some())
}

/// The sheet, over everything (the window's last child).
pub fn render(cx: &App) -> Option<AnyElement> {
    let entity = cx.try_global::<Open>()?.0.clone()?;
    Some(
        div()
            .id("skill-installer-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(0.35))
            .flex()
            .items_center()
            .justify_center()
            .child(entity)
            .into_any_element(),
    )
}

impl SkillInstaller {
    fn read_folders(&mut self, cx: &mut Context<Self>) {
        let (host, project, agents) = (self.host.clone(), self.project.clone(), self.agents.clone());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let folders = cx
                .background_spawn(async move {
                    let mut out = HashMap::new();
                    for (i, a) in agents.iter().enumerate() {
                        for scope in [Scope::Project, Scope::User] {
                            out.insert((i, scope), a.skill_folders(host.as_ref(), &project, scope));
                        }
                    }
                    out
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.folders = folders;
                cx.notify();
            });
        })
        .detach();
    }

    fn fetch(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let Some(spec) = install::parse_source(&text) else {
            self.stage = Stage::Failed("That is not a git URL or owner/repo.".into());
            cx.notify();
            return;
        };
        self.stage = Stage::Fetching;
        self.outcome = None;
        self.conflicts.clear();
        self.replace = false;
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = cx
                .background_spawn(async move {
                    let fetched = install::fetch(&spec)?;
                    let found = install::find_skills(&fetched);
                    anyhow::Ok((fetched, found))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.stage = match result {
                    Ok((_, found)) if found.is_empty() => Stage::Failed("No skills there: no folder in it holds a SKILL.md.".into()),
                    Ok((fetched, found)) => {
                        this.chosen = (0..found.len()).collect();
                        this.shown = 0;
                        Stage::Fetched { fetched: Arc::new(fetched), found }
                    }
                    Err(e) => Stage::Failed(format!("Could not fetch it: {e:#}")),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Where each chosen agent's copy goes for the scope: (agent, folder) to copy into, and
    /// (agent, the agent whose copy it reads) for those that need none.
    fn targets(&self) -> (Copies, Covered) {
        let mut copies: Vec<(usize, PathBuf)> = Vec::new();
        let mut covered = Vec::new();
        for (i, on) in self.agents_on.iter().enumerate() {
            let Some(folders) = self.folders.get(&(i, self.scope)).filter(|f| !f.is_empty() && *on) else { continue };
            match copies.iter().find(|(_, dir)| folders.contains(dir)) {
                Some((by, _)) => covered.push((i, *by)),
                None => copies.push((i, folders[0].clone())),
            }
        }
        (copies, covered)
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        let Stage::Fetched { fetched, found } = &self.stage else { return };
        let skills: Vec<Found> = found.iter().enumerate().filter(|(i, _)| self.chosen.contains(i)).map(|(_, f)| f.clone()).collect();
        let (copies, _) = self.targets();
        if skills.is_empty() || copies.is_empty() {
            return;
        }
        let host = self.host.clone();
        // Folders already there: say so first, install only once the person says replace.
        if !self.replace {
            let existing: Vec<PathBuf> = skills
                .iter()
                .flat_map(|s| copies.iter().map(move |(_, dir)| dir.join(&s.folder)))
                .filter(|d| host.exists(d))
                .collect();
            if !existing.is_empty() {
                self.conflicts = existing;
                cx.notify();
                return;
            }
        }
        let fetched = fetched.clone();
        let agents: Vec<(String, String)> = self.agents.iter().map(|a| (a.id().to_string(), a.name().to_string())).collect();
        let (scope, project) = (self.scope, self.project.clone());
        self.installing = true;
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = cx
                .background_spawn(async move {
                    let mut record = install::Record::load();
                    let mut done = Vec::new();
                    for skill in &skills {
                        let mut made = Vec::new();
                        for (agent, folder) in &copies {
                            let (dir, hash) = install::copy_into(host.as_ref(), skill, folder)?;
                            made.push(InstalledCopy { agent: agents[*agent].0.clone(), dir, hash });
                        }
                        record.put(Installed {
                            name: skill.name.clone(),
                            folder: skill.folder.clone(),
                            source: fetched.spec.label.clone(),
                            url: fetched.spec.url.clone(),
                            path: skill.rel.clone(),
                            commit: fetched.commit.clone(),
                            installed: install::today(),
                            scope,
                            project: (scope == Scope::Project).then(|| project.clone()),
                            host: if host.is_remote() { host.label() } else { "local".into() },
                            copies: made,
                        });
                        done.push(skill.name.clone());
                    }
                    record.save()?;
                    let names: Vec<&str> = copies.iter().map(|(a, _)| agents[*a].1.as_str()).collect();
                    anyhow::Ok(format!(
                        "Installed {} for {}. Agents pick new skills up in their next session.",
                        done.join(", "),
                        names.join(" and ")
                    ))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.installing = false;
                this.conflicts.clear();
                this.replace = false;
                let ok = result.is_ok();
                this.outcome = Some(result.map_err(|e| format!("{e:#}")));
                if ok {
                    (this.on_done)(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn short(&self, path: &std::path::Path) -> String {
        let home = self.host.home_dir();
        if let Ok(rel) = path.strip_prefix(&self.project) {
            return rel.display().to_string();
        }
        match home.as_ref().and_then(|h| path.strip_prefix(h).ok()) {
            Some(rel) => format!("~/{}", rel.display()),
            None => path.display().to_string(),
        }
    }
}

impl Render for SkillInstaller {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, border, bg, red, green, hover, sel) = (
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.background,
            theme.red,
            theme.green,
            theme.secondary_hover,
            theme.secondary,
        );
        let warn = theme.yellow;
        let mut card = div()
            .id("skill-installer")
            .w(px(860.))
            .h(px(620.))
            .flex()
            .flex_col()
            .rounded(px(12.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .shadow_lg()
            .overflow_hidden()
            // Clicks inside stay inside.
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_none()
                    .h(px(48.))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(border)
                    .child(div().flex_1().text_size(px(17.)).font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Install skills"))
                    .child(Button::new("skill-installer-close").label("Close").icon(IconName::Close).small().ghost().on_click(|_, _, cx| close(cx))),
            )
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .pt_3()
                    .flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.input)))
                    .child(
                        Button::new("skill-installer-fetch")
                            .label("Fetch")
                            .primary()
                            .disabled(matches!(self.stage, Stage::Fetching))
                            .on_click(cx.listener(|this, _, window, cx| this.fetch(window, cx))),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .pt_1()
                    .pb_2()
                    .text_size(px(12.))
                    .text_color(muted)
                    .child("A git URL, owner/repo (GitHub), or a GitHub folder link. Add @branch for a branch or tag. Private repositories use your own git access."),
            );
        match &self.stage {
            Stage::Idle => {
                card = card.child(div().flex_1());
            }
            Stage::Fetching => {
                card = card.child(div().flex_1().px_4().text_color(muted).child("Fetching…"));
            }
            Stage::Failed(why) => {
                card = card.child(div().flex_1().px_4().text_color(red).child(why.clone()));
            }
            Stage::Fetched { fetched, found } => {
                // The skills, each to tick; the shown one in full beside them.
                let mut list = div().id("skill-installer-list").w(px(280.)).flex_none().h_full().overflow_y_scroll().border_r_1().border_color(border).py_1();
                for (i, f) in found.iter().enumerate() {
                    let on = self.chosen.contains(&i);
                    list = list.child(
                        div()
                            .id(("skill-found", i))
                            .px_3()
                            .py(px(6.))
                            .flex()
                            .gap_2()
                            .cursor_pointer()
                            .when(i == self.shown, |d| d.bg(sel))
                            .hover(|d| d.bg(hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.shown = i;
                                cx.notify();
                            }))
                            .child(Checkbox::new(("skill-pick", i)).checked(on).on_click(cx.listener(move |this, on: &bool, _, cx| {
                                if *on {
                                    this.chosen.insert(i);
                                } else {
                                    this.chosen.remove(&i);
                                }
                                cx.notify();
                            })))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(div().text_size(px(14.)).text_color(fg).truncate().child(f.name.clone()))
                                    .child(div().text_size(px(12.)).text_color(muted).line_clamp(2).child(f.description.clone())),
                            ),
                    );
                }
                let f = &found[self.shown.min(found.len() - 1)];
                let commit: String = fetched.commit.chars().take(7).collect();
                let place = if f.rel.is_empty() { String::new() } else { format!(" · {}", f.rel) };
                let mut files = div().flex().flex_wrap().gap_x_3().gap_y_1().text_size(px(12.));
                for file in &f.files {
                    let runs = f.runnable.contains(file);
                    files = files.child(
                        div()
                            .flex()
                            .gap_1()
                            .text_color(if runs { warn } else { muted })
                            .child(file.clone())
                            .when(runs, |d| d.child("(runs)")),
                    );
                }
                let preview = div()
                    .id("skill-installer-preview")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .px_4()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_size(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(fg).child(f.name.clone()))
                    .child(div().text_size(px(12.)).text_color(muted).child(format!("from {}{place} at {commit}", fetched.spec.label)))
                    .child(files)
                    .when(!f.runnable.is_empty(), |d| {
                        d.child(div().text_size(px(12.)).text_color(warn).child(
                            "It comes with files an agent could run. Installing runs nothing; an agent may run them later, under its own permissions.",
                        ))
                    })
                    .child(
                        div()
                            .p_3()
                            .rounded(px(8.))
                            .bg(sel)
                            .font_family(crate::terminal_view::FONT_FAMILY)
                            .text_size(px(12.))
                            .text_color(fg)
                            .whitespace_normal()
                            .child(f.skill_md.clone()),
                    );
                card = card.child(div().flex_1().min_h_0().flex().border_t_1().border_color(border).child(list).child(preview));
            }
        }
        // Where it goes: the scope, the agents and their folders.
        let (copies, covered) = self.targets();
        let scope_button = |id: &'static str, label: String, scope: Scope, cx: &mut Context<Self>| {
            let on = self.scope == scope;
            Button::new(id)
                .label(label)
                .small()
                .when(on, |b| b.primary())
                .when(!on, |b| b.ghost())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.scope = scope;
                    this.conflicts.clear();
                    this.replace = false;
                    cx.notify();
                }))
        };
        let project_name = self.project.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut agents_col = div().flex().flex_col().gap_1();
        for (i, a) in self.agents.iter().enumerate() {
            let folders = self.folders.get(&(i, self.scope));
            if folders.is_some_and(|f| f.is_empty()) {
                continue;
            }
            let where_ = match (copies.iter().find(|(c, _)| *c == i), covered.iter().find(|(c, _)| *c == i)) {
                (Some((_, dir)), _) => format!("into {}", self.short(dir)),
                (_, Some((_, by))) => format!("reads {}'s copy", self.agents[*by].name()),
                _ => "not this time".into(),
            };
            agents_col = agents_col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Checkbox::new(("skill-agent", i)).label(a.name()).checked(self.agents_on[i]).on_click(cx.listener(move |this, on: &bool, _, cx| {
                        this.agents_on[i] = *on;
                        this.conflicts.clear();
                        cx.notify();
                    })))
                    .child(div().text_size(px(12.)).text_color(muted).child(where_)),
            );
        }
        let can = matches!(self.stage, Stage::Fetched { .. }) && !self.chosen.is_empty() && !copies.is_empty() && !self.installing;
        let replace = !self.conflicts.is_empty();
        let footer = div()
            .flex_none()
            .border_t_1()
            .border_color(border)
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_size(px(12.)).text_color(muted).child("For"))
                            .child(
                                div()
                                    .flex()
                                    .gap_1()
                                    .child(scope_button("scope-user", "All projects".into(), Scope::User, cx))
                                    .child(scope_button("scope-project", format!("This project ({project_name})"), Scope::Project, cx)),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_size(px(12.)).text_color(muted).child("Agents"))
                            .child(agents_col),
                    )
                    .child(
                        div().flex().items_end().child(
                            Button::new("skill-installer-install")
                                .label(if self.installing {
                                    "Installing…"
                                } else if replace {
                                    "Replace and install"
                                } else {
                                    "Install"
                                })
                                .primary()
                                .disabled(!can)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if !this.conflicts.is_empty() {
                                        this.replace = true;
                                    }
                                    this.install(cx);
                                })),
                        ),
                    ),
            )
            .when(replace, |d| {
                let list: Vec<String> = self.conflicts.iter().map(|c| self.short(c)).collect();
                d.child(div().text_size(px(12.)).text_color(warn).child(format!(
                    "Already there, and would be replaced: {}. Install again to replace them.",
                    list.join(", ")
                )))
            })
            .children(self.outcome.as_ref().map(|o| match o {
                Ok(msg) => div().text_size(px(12.)).text_color(green).child(msg.clone()),
                Err(e) => div().text_size(px(12.)).text_color(red).child(format!("Could not install: {e}")),
            }));
        card.child(footer)
    }
}
