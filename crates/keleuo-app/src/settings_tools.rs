//! Settings > Tools: tools made from command-line programs ([`keleuo_core::cli_tools`]), which
//! every agent gets beside Keleuo's own, for the whole app or for one folder (as the MCP servers
//! page has it). A tool is its command, with `{name}` placeholders where the agent's values go;
//! each placeholder is a parameter, described here for the agent: its kind, what it is for,
//! whether it is needed.

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::cli_tools::{self, Kind, Output, Param, Tool};

use crate::settings_mcp::{input, name_of, words};
use crate::settings_window::{SettingsPage, change};

/// A tool card's switch, its Edit and its Remove.
type Toggle = Box<dyn Fn(bool, &mut App)>;
type Edit = Option<Box<dyn Fn(&mut Window, &mut App)>>;
type Remove = Option<Box<dyn Fn(&mut App)>>;

/// The page's own state, kept on the Settings page.
#[derive(Default)]
pub struct ToolsState {
    /// The folder whose tools show; `None`: the app's.
    pub scope: Option<PathBuf>,
    pub folders: Vec<PathBuf>,
    form: Option<Form>,
    error: Option<String>,
}

/// The Add (or Edit) tool form.
struct Form {
    /// The tool being changed (its name before), when it is not a new one.
    editing: Option<String>,
    name: Entity<InputState>,
    description: Entity<TextareaState>,
    command: Entity<InputState>,
    /// One per placeholder in the command, in its order.
    params: Vec<ParamForm>,
    output: Output,
    /// Secrets: name and value (left empty when editing: the kept one stays).
    secrets: Vec<(Entity<InputState>, Entity<InputState>)>,
    writes: Entity<InputState>,
    timeout: Entity<InputState>,
    ask: bool,
}

struct ParamForm {
    name: String,
    kind: Kind,
    required: bool,
    description: Entity<InputState>,
    /// A choice's values, separated by commas.
    choices: Entity<InputState>,
}

impl ParamForm {
    fn new(p: &Param, window: &mut Window, cx: &mut App) -> Self {
        let description = input(window, cx, "what the agent should give here", false);
        let choices = input(window, cx, "first, second, third", false);
        description.update(cx, |i, cx| i.set_value(p.description.clone(), window, cx));
        choices.update(cx, |i, cx| i.set_value(p.choices.join(", "), window, cx));
        Self { name: p.name.clone(), kind: p.kind, required: p.required, description, choices }
    }
}

/// A command's words as one line, a word with spaces in quotes.
fn line(command: &[String]) -> String {
    command.iter().map(|w| if w.contains(' ') || w.is_empty() { format!("\"{w}\"") } else { w.clone() }).collect::<Vec<_>>().join(" ")
}

fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::Text => "Text",
        Kind::Number => "Number",
        Kind::Flag => "Yes / no",
        Kind::Choice => "Choice",
    }
}

impl ToolsState {
    pub fn new(folders: Vec<PathBuf>) -> Self {
        Self { folders, ..Default::default() }
    }

    /// Open the form, empty or with `tool` to change.
    fn open_form(&mut self, tool: Option<&Tool>, window: &mut Window, cx: &mut App) {
        self.error = None;
        let name = input(window, cx, "make_image", false);
        let description = cx.new(|cx| TextareaState::new(window, cx).auto_grow(2, 6).placeholder("What it does and when to use it: the agent reads this to decide."));
        let command = input(window, cx, "python3 ~/tools/image.py --prompt {prompt} --size {size} {hd?--hd}", false);
        let secrets = vec![(input(window, cx, "OPENAI_API_KEY", false), input(window, cx, "value (kept in the key store)", true))];
        let writes = input(window, cx, "~/Pictures/generated (optional)", false);
        let timeout = input(window, cx, "120", false);
        let mut form = Form { editing: None, name, description, command, params: Vec::new(), output: Output::Text, secrets, writes, timeout, ask: false };
        if let Some(t) = tool {
            let set = |e: &Entity<InputState>, v: String, window: &mut Window, cx: &mut App| e.update(cx, |i, cx| i.set_value(v, window, cx));
            set(&form.name, t.name.clone(), window, cx);
            form.description.update(cx, |i, cx| i.set_value(t.description.clone(), window, cx));
            set(&form.command, line(&t.command), window, cx);
            set(&form.writes, t.writes.iter().map(|w| w.display().to_string()).collect::<Vec<_>>().join(", "), window, cx);
            if t.timeout_secs > 0 {
                set(&form.timeout, t.timeout_secs.to_string(), window, cx);
            }
            form.params = t.params.iter().map(|p| ParamForm::new(p, window, cx)).collect();
            form.secrets = t
                .secrets
                .iter()
                .map(|n| {
                    let (name, value) = (input(window, cx, "NAME", false), input(window, cx, "kept; type to replace", true));
                    set(&name, n.clone(), window, cx);
                    (name, value)
                })
                .collect();
            if form.secrets.is_empty() {
                form.secrets.push((input(window, cx, "NAME", false), input(window, cx, "value (kept in the key store)", true)));
            }
            form.output = t.output;
            form.ask = t.ask;
            form.editing = Some(t.name.clone());
        }
        self.form = Some(form);
    }

    /// The form's parameters made to follow its command's placeholders (new ones added, gone
    /// ones dropped, the rest kept as described).
    fn follow_command(&mut self, window: &mut Window, cx: &mut App) {
        let Some(form) = self.form.as_mut() else { return };
        let names = cli_tools::placeholders(&words(&form.command.read(cx).value()));
        if names.iter().eq(form.params.iter().map(|p| &p.name)) {
            return;
        }
        let mut old: Vec<ParamForm> = std::mem::take(&mut form.params);
        form.params = names
            .into_iter()
            .map(|n| match old.iter().position(|p| p.name == n) {
                Some(i) => old.remove(i),
                None => ParamForm::new(&Param { name: n, required: true, ..Default::default() }, window, cx),
            })
            .collect();
    }
}

fn scope_tools(scope: &Option<PathBuf>, cx: &App) -> Vec<Tool> {
    let s = cx.global::<crate::AppSettings>().0.borrow();
    match scope {
        None => s.cli_tools.clone(),
        Some(f) => s.folder_tools.get(f).map(|f| f.tools.clone()).unwrap_or_default(),
    }
}

fn change_scope(scope: &Option<PathBuf>, cx: &mut App, f: impl FnOnce(&mut Vec<Tool>)) {
    let scope = scope.clone();
    change(cx, move |s| match scope {
        None => f(&mut s.cli_tools),
        Some(folder) => f(&mut s.folder_tools.entry(folder).or_default().tools),
    });
}

/// Save the form as a tool on the shown scope (in place of the one edited); its secrets go to
/// the key store. Why not, when it cannot be saved.
fn save(page: &WeakEntity<SettingsPage>, cx: &mut App) -> Result<(), String> {
    let page = page.upgrade().ok_or("Settings closed")?;
    let (scope, editing, tool, secrets) = {
        let p = page.read(cx);
        let Some(f) = &p.tools.form else { return Ok(()) };
        let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let secrets: Vec<(String, String)> =
            f.secrets.iter().map(|(n, val)| (v(n).to_ascii_uppercase().replace(['-', ' '], "_"), v(val))).filter(|(n, _)| !n.is_empty()).collect();
        let home = dirs::home_dir();
        let tool = Tool {
            name: v(&f.name),
            description: f.description.read(cx).value().trim().to_string(),
            command: words(&v(&f.command)),
            params: f
                .params
                .iter()
                .map(|pf| Param {
                    name: pf.name.clone(),
                    kind: pf.kind,
                    description: v(&pf.description),
                    required: pf.required && pf.kind != Kind::Flag,
                    choices: if pf.kind == Kind::Choice { v(&pf.choices).split(',').map(str::trim).filter(|c| !c.is_empty()).map(String::from).collect() } else { Vec::new() },
                })
                .collect(),
            secrets: secrets.iter().map(|(n, _)| n.clone()).collect(),
            writes: v(&f.writes)
                .split(',')
                .map(str::trim)
                .filter(|w| !w.is_empty() && !w.starts_with('('))
                .map(|w| match (w.strip_prefix("~/"), &home) {
                    (Some(rest), Some(h)) => h.join(rest),
                    _ => PathBuf::from(w),
                })
                .collect(),
            output: f.output,
            timeout_secs: v(&f.timeout).parse().unwrap_or(0),
            ask: f.ask,
            enabled: true,
        };
        (p.tools.scope.clone(), f.editing.clone(), tool, secrets)
    };
    let name = tool.name.clone();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') || name.len() > 60 {
        return Err("Give it a short name, as the agent will call it: letters, digits, _ and -.".into());
    }
    if cli_tools::RESERVED.contains(&name.as_str()) {
        return Err(format!("{name} is one of Keleuo's own tools; give it another name."));
    }
    if editing.as_deref() != Some(name.as_str()) && scope_tools(&scope, cx).iter().any(|t| t.name == name) {
        return Err(format!("There is a tool called {name} here already."));
    }
    if tool.description.is_empty() {
        return Err("Describe it: the agent uses the description to know when to call it.".into());
    }
    if tool.command.is_empty() {
        return Err("Give the command it runs.".into());
    }
    if let Some(p) = tool.params.iter().find(|p| p.kind == Kind::Choice && p.choices.is_empty()) {
        return Err(format!("Give {}'s choices, separated by commas.", p.name));
    }
    if secrets.iter().any(|(_, v)| !v.is_empty()) {
        keleuo_core::secrets::supported()?;
    }
    for (n, value) in &secrets {
        if !value.is_empty() {
            keleuo_core::secrets::set(&cli_tools::secret_account(&name, n), value).map_err(|e| format!("Could not keep {n}: {e}"))?;
        }
    }
    // A renamed tool's secrets move to its new name.
    if let Some(old) = editing.as_deref().filter(|o| *o != name) {
        for (n, value) in &secrets {
            if value.is_empty()
                && let Some(kept) = keleuo_core::secrets::get(&cli_tools::secret_account(old, n))
            {
                keleuo_core::secrets::set(&cli_tools::secret_account(&name, n), &kept).map_err(|e| format!("Could not keep {n}: {e}"))?;
            }
        }
    }
    change_scope(&scope, cx, |list| match editing.and_then(|old| list.iter().position(|t| t.name == old)) {
        Some(i) => {
            let enabled = list[i].enabled;
            list[i] = Tool { enabled, ..tool };
        }
        None => list.push(tool),
    });
    page.update(cx, |p, cx| {
        p.tools.form = None;
        p.tools.error = None;
        cx.notify();
    });
    Ok(())
}

/// Change the open form.
fn with_form(page: &WeakEntity<SettingsPage>, cx: &mut App, f: impl FnOnce(&mut Form)) {
    let _ = page.update(cx, |p, cx| {
        if let Some(form) = p.tools.form.as_mut() {
            f(form);
        }
        cx.notify();
    });
}

/// The page: the scope switch, the tools, the form.
pub fn render(page: &WeakEntity<SettingsPage>, window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(entity) = page.upgrade() else { return div().into_any_element() };
    entity.update(cx, |p, cx| p.tools.follow_command(window, cx));
    let theme = cx.theme();
    let (fg, muted, border, accent, ring, danger) = (theme.foreground, theme.muted_foreground, theme.border, theme.accent, theme.ring, theme.danger);
    let state = &entity.read(cx).tools;
    let scope = state.scope.clone();
    let settings = cx.global::<crate::AppSettings>().0.borrow().clone();

    let (p1, p2, folders) = (page.clone(), page.clone(), state.folders.clone());
    let scope_bar = div()
        .flex()
        .items_center()
        .gap_2()
        .child(Button::new("tools-scope-app").small().label("App").when(scope.is_none(), |b| b.primary()).when(scope.is_some(), |b| b.ghost()).on_click(move |_, _, cx| {
            let _ = p1.update(cx, |p, cx| {
                p.tools.scope = None;
                p.tools.form = None;
                cx.notify();
            });
        }))
        .child(
            Button::new("tools-scope-folder")
                .small()
                .icon(IconName::Folder)
                .label(match &scope {
                    Some(f) => format!("Folder: {}", name_of(f)),
                    None => "Folder…".into(),
                })
                .when(scope.is_some(), |b| b.primary())
                .when(scope.is_none(), |b| b.ghost())
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    for f in folders.clone() {
                        let (p, f2) = (p2.clone(), f.clone());
                        menu = menu.item(PopupMenuItem::new(name_of(&f)).on_click(move |_, _, cx| {
                            let _ = p.update(cx, |p, cx| {
                                p.tools.scope = Some(f2.clone());
                                p.tools.form = None;
                                cx.notify();
                            });
                        }));
                    }
                    menu
                }),
        )
        .child(div().text_xs().text_color(muted).child(match &scope {
            Some(f) => format!("Only for sessions in {} · the rest follows App", name_of(f)),
            None => "Every agent Keleuo starts gets these, unless a folder turns one off".into(),
        }));

    // One tool's card.
    let card = |t: &Tool, tag: &str, own: bool, on: bool, note: Option<String>, toggle: Toggle, edit: Edit, remove: Remove| {
        let id = SharedString::from(format!("tool-{}-{}", tag, t.name));
        let params: Vec<String> = t.params.iter().map(|p| format!("{}{} ({})", p.name, if p.required { "" } else { "?" }, kind_label(p.kind).to_lowercase())).collect();
        let secrets: Vec<String> = t
            .secrets
            .iter()
            .map(|n| format!("{n} {}", if keleuo_core::secrets::get(&cli_tools::secret_account(&t.name, n)).is_some() { "in the key store ✓" } else { "missing" }))
            .collect();
        let mut how = vec![format!("stops after {}s", t.timeout().as_secs())];
        if t.ask {
            how.insert(0, "asks before each run".into());
        }
        if t.output == Output::Images {
            how.push("sends back images it prints the paths of".into());
        }
        div()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(border)
            .when(!on, |d| d.opacity(0.6))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(t.name.clone()))
                    .child(div().px_1p5().rounded_sm().text_xs().when(own, |d| d.bg(accent).text_color(ring)).when(!own, |d| d.bg(border.opacity(0.5)).text_color(muted)).child(tag.to_string()))
                    .child(div().flex_1())
                    .children(edit.map(|edit| Button::new(SharedString::from(format!("{id}-edit"))).xsmall().ghost().label("Edit").on_click(move |_, window, cx| edit(window, cx))))
                    .children(remove.map(|remove| {
                        Button::new(SharedString::from(format!("{id}-remove"))).xsmall().ghost().icon(gpui_kit::assets::IconName::Trash).tooltip("Remove").on_click(move |_, _, cx| remove(cx))
                    }))
                    .child(Switch::new(SharedString::from(format!("{id}-on"))).checked(on).on_click(move |on: &bool, _, cx| toggle(*on, cx))),
            )
            .when(!t.description.is_empty(), |d| d.child(div().text_sm().text_color(fg).child(t.description.clone())))
            .child(div().text_sm().text_color(muted).font_family("Menlo").child(line(&t.command)))
            .when(!params.is_empty(), |d| d.child(div().text_sm().text_color(muted).child(format!("Takes: {}", params.join(", ")))))
            .when(!secrets.is_empty(), |d| d.child(div().text_sm().text_color(muted).child(format!("Secrets: {}", secrets.join(", ")))))
            .when(!t.writes.is_empty(), |d| {
                d.child(div().text_sm().text_color(muted).child(format!("Writes in: {}", t.writes.iter().map(|w| w.display().to_string()).collect::<Vec<_>>().join(", "))))
            })
            .child(div().text_xs().text_color(muted).child(how.join(" · ")))
            .children(note.map(|n| div().text_xs().text_color(muted).child(n)))
            .into_any_element()
    };
    let edit_fn = |t: &Tool| -> Edit {
        let (p, t) = (page.clone(), t.clone());
        Some(Box::new(move |window, cx| {
            let _ = p.update(cx, |p, cx| {
                p.tools.open_form(Some(&t), window, cx);
                cx.notify();
            });
        }))
    };

    let section = |t: &str| div().pt_2().text_sm().text_color(muted).child(t.to_string());
    let mut list = div().flex().flex_col().gap_2();
    match &scope {
        None => {
            if settings.cli_tools.is_empty() {
                list = list.child(div().text_sm().text_color(muted).child("No tools yet."));
            }
            for t in &settings.cli_tools {
                let (n1, n2) = (t.name.clone(), t.name.clone());
                list = list.child(card(
                    t,
                    "App",
                    true,
                    t.enabled,
                    None,
                    Box::new(move |on, cx| change(cx, |st| st.cli_tools.iter_mut().filter(|x| x.name == n1).for_each(|x| x.enabled = on))),
                    edit_fn(t),
                    Some(Box::new(move |cx| change(cx, |st| st.cli_tools.retain(|x| x.name != n2)))),
                ));
            }
        }
        Some(folder) => {
            let own = settings.folder_tools.get(folder).cloned().unwrap_or_default();
            list = list.child(section("Inherited from App"));
            if settings.cli_tools.is_empty() {
                list = list.child(div().text_sm().text_color(muted).child("None."));
            }
            for t in &settings.cli_tools {
                let replaced = own.tools.iter().any(|o| o.name == t.name);
                let off_here = own.off.contains(&t.name);
                let on = t.enabled && !off_here && !replaced;
                let note = if replaced {
                    Some("Replaced in this folder by its own".to_string())
                } else if off_here {
                    Some("Off in this folder".to_string())
                } else if !t.enabled {
                    Some("Off for the whole app".to_string())
                } else {
                    None
                };
                let (f, n) = (folder.clone(), t.name.clone());
                list = list.child(card(
                    t,
                    "App",
                    false,
                    on,
                    note,
                    Box::new(move |on, cx| {
                        let (f, n) = (f.clone(), n.clone());
                        change(cx, move |st| {
                            let ft = st.folder_tools.entry(f).or_default();
                            ft.off.retain(|x| *x != n);
                            if !on {
                                ft.off.push(n);
                            }
                        })
                    }),
                    None,
                    None,
                ));
            }
            list = list.child(section("This folder"));
            if own.tools.is_empty() {
                list = list.child(div().text_sm().text_color(muted).child("None of its own."));
            }
            for t in &own.tools {
                let (f1, f2, n1, n2) = (folder.clone(), folder.clone(), t.name.clone(), t.name.clone());
                list = list.child(card(
                    t,
                    "This folder",
                    true,
                    t.enabled,
                    None,
                    Box::new(move |on, cx| {
                        let (f, n) = (f1.clone(), n1.clone());
                        change(cx, move |st| st.folder_tools.entry(f).or_default().tools.iter_mut().filter(|x| x.name == n).for_each(|x| x.enabled = on))
                    }),
                    edit_fn(t),
                    Some(Box::new(move |cx| {
                        let (f, n) = (f2.clone(), n2.clone());
                        change(cx, move |st| st.folder_tools.entry(f).or_default().tools.retain(|x| x.name != n))
                    })),
                ));
            }
        }
    }

    let state = &entity.read(cx).tools;
    let mut add = div().flex().flex_col().gap_2();
    if let Some(form) = &state.form {
        let field = |label: &str, hint: Option<&str>, el: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().text_color(fg).child(label.to_string()))
                .child(el)
                .children(hint.map(|h| div().text_xs().text_color(muted).child(h.to_string())))
        };
        // The parameters, one row each.
        let mut params = div().flex().flex_col().gap_2();
        if form.params.is_empty() {
            params = params.child(div().text_sm().text_color(muted).child("None: put {name} in the command where a value goes."));
        }
        for (i, pf) in form.params.iter().enumerate() {
            let (pk, pr) = (page.clone(), page.clone());
            let kinds = [Kind::Text, Kind::Number, Kind::Flag, Kind::Choice];
            params = params.child(
                div()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(border.opacity(0.6))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().w(px(120.)).truncate().font_family("Menlo").text_sm().text_color(fg).child(pf.name.clone()))
                            .child(
                                Button::new(SharedString::from(format!("tool-param-{i}-kind")))
                                    .xsmall()
                                    .outline()
                                    .label(kind_label(pf.kind))
                                    .dropdown_caret(true)
                                    .dropdown_menu(move |mut menu, _, _| {
                                        for k in kinds {
                                            let p = pk.clone();
                                            menu = menu.item(PopupMenuItem::new(kind_label(k)).on_click(move |_, _, cx| with_form(&p, cx, |f| f.params[i].kind = k)));
                                        }
                                        menu
                                    }),
                            )
                            .child(div().flex_1().child(Input::new(&pf.description).small()))
                            .when(pf.kind != Kind::Flag, |d| {
                                d.child(div().flex_none().text_xs().text_color(muted).child("Required")).child(
                                    Switch::new(SharedString::from(format!("tool-param-{i}-req")))
                                        .checked(pf.required)
                                        .small()
                                        .on_click(move |on: &bool, _, cx| {
                                            let on = *on;
                                            with_form(&pr, cx, |f| f.params[i].required = on)
                                        }),
                                )
                            }),
                    )
                    .when(pf.kind == Kind::Choice, |d| {
                        d.child(div().flex().items_center().gap_2().child(div().w(px(120.)).text_xs().text_color(muted).child("Choices")).child(div().flex_1().child(Input::new(&pf.choices).small())))
                    }),
            );
        }
        let mut secrets = div().flex().flex_col().gap_1();
        for (n, v) in &form.secrets {
            secrets = secrets.child(div().flex().gap_2().child(div().w(px(200.)).child(Input::new(n).small())).child(div().flex_1().child(Input::new(v).small())));
        }
        let (p_text, p_images, p_more, p_ask, p_save, p_cancel) = (page.clone(), page.clone(), page.clone(), page.clone(), page.clone(), page.clone());
        add = add.child(
            div()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(border)
                .flex()
                .flex_col()
                .gap_3()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(if form.editing.is_some() { "Edit tool" } else { "Add tool" }))
                .child(field("Name", Some("What the agent calls it."), Input::new(&form.name).small().into_any_element()))
                .child(field("Description", None, Textarea::new(&form.description).small().into_any_element()))
                .child(field(
                    "Command",
                    Some("Run as it is, without a shell. {name} is replaced by the value given; {name?--flag} puts --flag in when name is yes or given. A {name} not given is left out, with a --option just before it."),
                    Input::new(&form.command).small().into_any_element(),
                ))
                .child(field("Parameters", None, params.into_any_element()))
                .child(field(
                    "What the agent gets back",
                    None,
                    div()
                        .flex()
                        .gap_2()
                        .child(Button::new("tool-out-text").small().label("What it prints").when(form.output == Output::Text, |b| b.primary()).when(form.output != Output::Text, |b| b.ghost()).on_click(move |_, _, cx| {
                            with_form(&p_text, cx, |f| f.output = Output::Text)
                        }))
                        .child(Button::new("tool-out-images").small().label("…and the images it prints the paths of").when(form.output == Output::Images, |b| b.primary()).when(form.output != Output::Images, |b| b.ghost()).on_click(
                            move |_, _, cx| with_form(&p_images, cx, |f| f.output = Output::Images),
                        ))
                        .into_any_element(),
                ))
                .child(field(
                    "Secrets it gets as environment variables",
                    None,
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(secrets)
                        .child(div().child(Button::new("tool-secret-more").xsmall().ghost().icon(IconName::Plus).label("Another").on_click(move |_, window, cx| {
                            let pair = (input(window, cx, "NAME", false), input(window, cx, "value", true));
                            with_form(&p_more, cx, |f| f.secrets.push(pair))
                        })))
                        .into_any_element(),
                ))
                .child(field("Folders it writes in, beside the session's", None, Input::new(&form.writes).small().into_any_element()))
                .child(div().flex().items_center().gap_4().child(field("Stop it after (seconds)", None, div().w(px(100.)).child(Input::new(&form.timeout).small()).into_any_element())).child(
                    div().flex().items_center().gap_2().pt_4().child(Switch::new("tool-ask").checked(form.ask).small().on_click(move |on: &bool, _, cx| {
                        let on = *on;
                        with_form(&p_ask, cx, |f| f.ask = on)
                    })).child(div().text_sm().text_color(fg).child("Ask me before each run")),
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Button::new("tool-save").small().primary().label(if form.editing.is_some() { "Save" } else { "Add" }).on_click(move |_, _, cx| {
                            if let Err(e) = save(&p_save, cx) {
                                let _ = p_save.update(cx, |p, cx| {
                                    p.tools.error = Some(e);
                                    cx.notify();
                                });
                            }
                        }))
                        .child(Button::new("tool-cancel").small().ghost().label("Cancel").on_click(move |_, _, cx| {
                            let _ = p_cancel.update(cx, |p, cx| {
                                p.tools.form = None;
                                p.tools.error = None;
                                cx.notify();
                            });
                        })),
                ),
        );
    } else {
        let p_add = page.clone();
        add = add.child(div().child(Button::new("tool-add").small().outline().icon(IconName::Plus).label("Add tool").on_click(move |_, window, cx| {
            let _ = p_add.update(cx, |p, cx| {
                p.tools.open_form(None, window, cx);
                cx.notify();
            });
        })));
    }
    let error = state.error.clone();
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .child(scope_bar)
        .child(div().text_sm().text_color(muted).child(
            "Your own tools, made from command-line programs: every agent gets them beside Keleuo's (send_message and the rest). Keleuo runs them, in the session's folder, with their secrets from the key store: the agent never sees those.",
        ))
        .child(list)
        .child(add)
        .children(error.map(|e| div().text_sm().text_color(danger).child(e)))
        .child(div().text_xs().text_color(muted).child(
            "Agents started before a change get it when they next list their tools (most do so at start: restart the session if one doesn't see it). With \"Keep agents' writes inside the session folder\" on, a tool writes only there and in the folders it names.",
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn a_command_goes_back_to_a_line_it_splits_from() {
        let command = vec!["python3".to_string(), "my tool.py".into(), "--prompt".into(), "{prompt}".into()];
        assert_eq!(words(&line(&command)), command);
    }
}
