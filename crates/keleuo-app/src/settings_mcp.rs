//! Settings > MCP servers: the tool servers every agent gets ([`keleuo_core::mcp`]), for the
//! whole app or for one folder. On the folder's scope the app's show first ("Inherited from
//! App", each a switch for this folder), then the folder's own. A server is added with its
//! command or address, its secrets (kept in the system's key store, never in the settings
//! file) and the folders it writes in; Claude Code's own servers can be imported.

use std::path::{Path, PathBuf};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use keleuo_core::mcp::{self, Found, Server};

use crate::settings_window::{SettingsPage, change};

/// A server card's switch, and its Remove.
type Toggle = Box<dyn Fn(bool, &mut App)>;
type Remove = Option<Box<dyn Fn(&mut App)>>;

/// The page's own state, kept on the Settings page.
#[derive(Default)]
pub struct McpState {
    /// The folder whose servers show; `None`: the app's.
    pub scope: Option<PathBuf>,
    /// Folders to choose from: the shown session's first, then the recent ones.
    pub folders: Vec<PathBuf>,
    form: Option<Form>,
    /// Servers found in Claude Code's settings, while the import list shows.
    found: Option<Vec<Found>>,
    error: Option<String>,
}

/// The Add server form.
struct Form {
    name: Entity<InputState>,
    remote: bool,
    /// A local server's command line, or a remote one's address.
    command: Entity<InputState>,
    /// Secrets: name and value (a remote server's token is `TOKEN`, sent as a bearer token).
    secrets: Vec<(Entity<InputState>, Entity<InputState>)>,
    /// Folders it writes in, separated by commas.
    writes: Entity<InputState>,
}

pub(crate) fn input(window: &mut Window, cx: &mut App, placeholder: &str, masked: bool) -> Entity<InputState> {
    let placeholder = placeholder.to_string();
    cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(masked))
}

impl McpState {
    /// The page offering `folders` (the shown session's first), on the app's scope.
    pub fn new(folders: Vec<PathBuf>) -> Self {
        Self { folders, ..Default::default() }
    }

    fn open_form(&mut self, window: &mut Window, cx: &mut App) {
        self.error = None;
        self.form = Some(Form {
            name: input(window, cx, "github", false),
            remote: false,
            command: input(window, cx, "npx -y @modelcontextprotocol/server-github", false),
            secrets: vec![(input(window, cx, "GITHUB_TOKEN", false), input(window, cx, "value (kept in the key store)", true))],
            writes: input(window, cx, "~/.cache/github-mcp (optional)", false),
        });
    }
}

/// The servers on the shown scope (the app's, or the folder's own).
fn scope_servers(scope: &Option<PathBuf>, cx: &App) -> Vec<Server> {
    let s = cx.global::<crate::AppSettings>().0.borrow();
    match scope {
        None => s.mcp_servers.clone(),
        Some(f) => s.folder_mcp.get(f).map(|f| f.servers.clone()).unwrap_or_default(),
    }
}

/// Change the shown scope's servers.
fn change_scope(scope: &Option<PathBuf>, cx: &mut App, f: impl FnOnce(&mut Vec<Server>)) {
    let scope = scope.clone();
    change(cx, move |s| match scope {
        None => f(&mut s.mcp_servers),
        Some(folder) => f(&mut s.folder_mcp.entry(folder).or_default().servers),
    });
}

pub(crate) fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}

/// A command line split into words (a word in quotes keeps its spaces).
pub(crate) fn words(line: &str) -> Vec<String> {
    let (mut out, mut word, mut quote) = (Vec::new(), String::new(), None::<char>);
    for c in line.trim().chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, c) if c.is_whitespace() => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
            }
            (_, c) => word.push(c),
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// Save the form as a server on the shown scope; its secrets go to the key store. Why not,
/// when it cannot be saved.
fn save(page: &WeakEntity<SettingsPage>, cx: &mut App) -> Result<(), String> {
    let page = page.upgrade().ok_or("Settings closed")?;
    let (scope, form_values) = {
        let p = page.read(cx);
        let Some(f) = &p.mcp.form else { return Ok(()) };
        let v = |e: &Entity<InputState>| e.read(cx).value().trim().to_string();
        let secrets: Vec<(String, String)> =
            f.secrets.iter().map(|(n, val)| (v(n).to_ascii_uppercase().replace(['-', ' '], "_"), v(val))).filter(|(n, _)| !n.is_empty()).collect();
        (p.mcp.scope.clone(), (v(&f.name), f.remote, v(&f.command), secrets, v(&f.writes)))
    };
    let (name, remote, command, secrets, writes) = form_values;
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("Give it a short name: letters, digits, - and _.".into());
    }
    if scope_servers(&scope, cx).iter().any(|s| s.name == name) {
        return Err(format!("There is a server called {name} here already."));
    }
    if command.is_empty() {
        return Err(if remote { "Give its address.".into() } else { "Give the command that starts it.".into() });
    }
    if secrets.iter().any(|(_, v)| !v.is_empty()) {
        keleuo_core::secrets::supported()?;
    }
    for (n, value) in &secrets {
        if !value.is_empty() {
            keleuo_core::secrets::set(&mcp::secret_account(&name, n), value).map_err(|e| format!("Could not keep {n}: {e}"))?;
        }
    }
    let mut server = Server { name: name.clone(), enabled: true, secrets: secrets.iter().map(|(n, _)| n.clone()).collect(), ..Default::default() };
    if remote {
        server.url = command;
        // A remote server's secret is its bearer token.
        if let Some((n, _)) = secrets.first() {
            server.headers.insert("Authorization".into(), format!("Bearer ${{{n}}}"));
        }
    } else {
        let mut w = words(&command);
        server.command = w.remove(0);
        server.args = w;
        server.env = secrets.iter().map(|(n, _)| (n.clone(), format!("${{{n}}}"))).collect();
    }
    let home = dirs::home_dir();
    server.writes = writes
        .split(',')
        .map(str::trim)
        .filter(|w| !w.is_empty() && !w.starts_with('('))
        .map(|w| match (w.strip_prefix("~/"), &home) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => PathBuf::from(w),
        })
        .collect();
    change_scope(&scope, cx, |list| list.push(server));
    page.update(cx, |p, cx| {
        p.mcp.form = None;
        p.mcp.error = None;
        cx.notify();
    });
    Ok(())
}

/// Bring a server from Claude Code's settings onto the shown scope, its values to the key store.
fn import(page: &WeakEntity<SettingsPage>, found: Found, cx: &mut App) {
    let Some(page) = page.upgrade() else { return };
    let scope = page.read(cx).mcp.scope.clone();
    let name = found.server.name.clone();
    let stored: Result<(), String> = found.values.iter().try_for_each(|(n, v)| {
        keleuo_core::secrets::set(&mcp::secret_account(&name, n), v).map_err(|e| format!("Could not keep {n} for {name}: {e}"))
    });
    page.update(cx, |p, cx| {
        match stored {
            Ok(()) => {
                if let Some(list) = p.mcp.found.as_mut() {
                    list.retain(|f| f.server.name != name);
                }
                p.mcp.error = None;
            }
            Err(e) => p.mcp.error = Some(e),
        }
        cx.notify();
    });
    if page.read(cx).mcp.error.is_none() {
        change_scope(&scope, cx, |list| list.push(found.server));
    }
}

/// The page: the scope switch, the servers, adding and importing.
pub fn render(page: &WeakEntity<SettingsPage>, window: &mut Window, cx: &mut App) -> AnyElement {
    let Some(entity) = page.upgrade() else { return div().into_any_element() };
    let theme = cx.theme();
    let (fg, muted, border, accent, ring, danger) = (theme.foreground, theme.muted_foreground, theme.border, theme.accent, theme.ring, theme.danger);
    let state = &entity.read(cx).mcp;
    let scope = state.scope.clone();
    let settings = cx.global::<crate::AppSettings>().0.borrow().clone();

    // App / Folder: name ▾.
    let p1 = page.clone();
    let folders = state.folders.clone();
    let p2 = page.clone();
    let scope_bar = div()
        .flex()
        .items_center()
        .gap_2()
        .child(
            Button::new("mcp-scope-app")
                .small()
                .label("App")
                .when(scope.is_none(), |b| b.primary())
                .when(scope.is_some(), |b| b.ghost())
                .on_click(move |_, _, cx| {
                    let _ = p1.update(cx, |p, cx| {
                        p.mcp.scope = None;
                        cx.notify();
                    });
                }),
        )
        .child(
            Button::new("mcp-scope-folder")
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
                                p.mcp.scope = Some(f2.clone());
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

    // One server's card.
    let card = |s: &Server, tag: &str, own: bool, on: bool, dim: bool, note: Option<String>, toggle: Toggle, remove: Remove| {
        let kind = if s.is_remote() { format!("Remote · {}", s.url) } else { format!("Local · {} {}", s.command, s.args.join(" ")) };
        let auth: Vec<String> = s
            .secrets
            .iter()
            .map(|n| {
                let kept = keleuo_core::secrets::get(&mcp::secret_account(&s.name, n)).is_some();
                format!("{n} {}", if kept { "in the key store ✓" } else { "missing" })
            })
            .collect();
        let id = SharedString::from(format!("mcp-{}-{}", tag, s.name));
        div()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(border)
            .when(dim, |d| d.opacity(0.6))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(s.name.clone()))
                    .child(div().px_1p5().rounded_sm().text_xs().when(own, |d| d.bg(accent).text_color(ring)).when(!own, |d| d.bg(border.opacity(0.5)).text_color(muted)).child(tag.to_string()))
                    .child(div().flex_1())
                    .children(remove.map(|remove| {
                        Button::new(SharedString::from(format!("{id}-remove"))).xsmall().ghost().icon(gpui_kit::assets::IconName::Trash).tooltip("Remove").on_click(move |_, _, cx| remove(cx))
                    }))
                    .child(Switch::new(SharedString::from(format!("{id}-on"))).checked(on).on_click(move |on: &bool, _, cx| toggle(*on, cx))),
            )
            .child(div().text_sm().text_color(muted).font_family("Menlo").child(kind))
            .when(!auth.is_empty(), |d| d.child(div().text_sm().text_color(muted).child(format!("Secrets: {}", auth.join(", ")))))
            .when(!s.writes.is_empty(), |d| {
                d.child(div().text_sm().text_color(muted).child(format!("Writes in: {}", s.writes.iter().map(|w| w.display().to_string()).collect::<Vec<_>>().join(", "))))
            })
            .children(note.map(|n| div().text_xs().text_color(muted).child(n)))
            .into_any_element()
    };

    let section = |t: &str| div().pt_2().text_sm().text_color(muted).child(t.to_string());
    let mut list = div().flex().flex_col().gap_2();
    match &scope {
        None => {
            if settings.mcp_servers.is_empty() {
                list = list.child(div().text_sm().text_color(muted).child("No servers yet. Add one, or import Claude Code's."));
            }
            for s in &settings.mcp_servers {
                let (n1, n2) = (s.name.clone(), s.name.clone());
                list = list.child(card(
                    s,
                    "App",
                    true,
                    s.enabled,
                    false,
                    None,
                    Box::new(move |on, cx| change(cx, |st| st.mcp_servers.iter_mut().filter(|x| x.name == n1).for_each(|x| x.enabled = on))),
                    Some(Box::new(move |cx| {
                        change(cx, |st| st.mcp_servers.retain(|x| x.name != n2));
                    })),
                ));
            }
        }
        Some(folder) => {
            let own = settings.folder_mcp.get(folder).cloned().unwrap_or_default();
            list = list.child(section("Inherited from App"));
            if settings.mcp_servers.is_empty() {
                list = list.child(div().text_sm().text_color(muted).child("None."));
            }
            for s in &settings.mcp_servers {
                let replaced = own.servers.iter().any(|o| o.name == s.name);
                let off_here = own.off.contains(&s.name);
                let on = s.enabled && !off_here && !replaced;
                let note = if replaced {
                    Some("Replaced in this folder by its own".to_string())
                } else if off_here {
                    Some("Off in this folder".to_string())
                } else if !s.enabled {
                    Some("Off for the whole app".to_string())
                } else {
                    None
                };
                let (f, n) = (folder.clone(), s.name.clone());
                list = list.child(card(
                    s,
                    "App",
                    false,
                    on,
                    !on,
                    note,
                    Box::new(move |on, cx| {
                        let (f, n) = (f.clone(), n.clone());
                        change(cx, move |st| {
                            let fs = st.folder_mcp.entry(f).or_default();
                            fs.off.retain(|x| *x != n);
                            if !on {
                                fs.off.push(n);
                            }
                        })
                    }),
                    None,
                ));
            }
            list = list.child(section("This folder"));
            if own.servers.is_empty() {
                list = list.child(div().text_sm().text_color(muted).child("None of its own."));
            }
            for s in &own.servers {
                let (f1, f2, n1, n2) = (folder.clone(), folder.clone(), s.name.clone(), s.name.clone());
                list = list.child(card(
                    s,
                    "This folder",
                    true,
                    s.enabled,
                    false,
                    None,
                    Box::new(move |on, cx| {
                        let (f, n) = (f1.clone(), n1.clone());
                        change(cx, move |st| st.folder_mcp.entry(f).or_default().servers.iter_mut().filter(|x| x.name == n).for_each(|x| x.enabled = on))
                    }),
                    Some(Box::new(move |cx| {
                        let (f, n) = (f2.clone(), n2.clone());
                        change(cx, move |st| st.folder_mcp.entry(f).or_default().servers.retain(|x| x.name != n))
                    })),
                ));
            }
        }
    }

    // Adding: the form, or the buttons that open it and the import list.
    let state = &entity.read(cx).mcp;
    let mut add = div().flex().flex_col().gap_2();
    if let Some(form) = &state.form {
        let (p_local, p_remote, p_more, p_save, p_cancel) = (page.clone(), page.clone(), page.clone(), page.clone(), page.clone());
        let field = |label: &str, el: AnyElement| div().flex().flex_col().gap_1().child(div().text_sm().text_color(fg).child(label.to_string())).child(el);
        let mut secrets = div().flex().flex_col().gap_1();
        for (n, v) in &form.secrets {
            secrets = secrets.child(
                div()
                    .flex()
                    .gap_2()
                    .child(div().w(px(200.)).child(Input::new(n).small()))
                    .child(div().flex_1().child(Input::new(v).small())),
            );
        }
        add = add.child(
            div()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(border)
                .flex()
                .flex_col()
                .gap_3()
                .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Add server"))
                .child(field("Name", Input::new(&form.name).small().into_any_element()))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(Button::new("mcp-kind-local").small().label("Local command").when(!form.remote, |b| b.primary()).when(form.remote, |b| b.ghost()).on_click(move |_, _, cx| {
                            let _ = p_local.update(cx, |p, cx| {
                                if let Some(f) = p.mcp.form.as_mut() {
                                    f.remote = false;
                                }
                                cx.notify();
                            });
                        }))
                        .child(Button::new("mcp-kind-remote").small().label("Remote address").when(form.remote, |b| b.primary()).when(!form.remote, |b| b.ghost()).on_click(move |_, _, cx| {
                            let _ = p_remote.update(cx, |p, cx| {
                                if let Some(f) = p.mcp.form.as_mut() {
                                    f.remote = true;
                                }
                                cx.notify();
                            });
                        })),
                )
                .child(field(if form.remote { "Address" } else { "Command" }, Input::new(&form.command).small().into_any_element()))
                .child(field(
                    if form.remote { "Token (sent as a bearer token)" } else { "Secrets it gets as environment variables" },
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(secrets)
                        .when(!form.remote, |d| {
                            d.child(div().child(Button::new("mcp-secret-more").xsmall().ghost().icon(IconName::Plus).label("Another").on_click(move |_, window, cx| {
                                let _ = p_more.update(cx, |p, cx| {
                                    let pair = (input(window, cx, "NAME", false), input(window, cx, "value", true));
                                    if let Some(f) = p.mcp.form.as_mut() {
                                        f.secrets.push(pair);
                                    }
                                    cx.notify();
                                });
                            })))
                        })
                        .into_any_element(),
                ))
                .when(!form.remote, |d| d.child(field("Folders it writes in", Input::new(&form.writes).small().into_any_element())))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Button::new("mcp-save").small().primary().label("Add").on_click(move |_, _, cx| {
                            if let Err(e) = save(&p_save, cx) {
                                let _ = p_save.update(cx, |p, cx| {
                                    p.mcp.error = Some(e);
                                    cx.notify();
                                });
                            }
                        }))
                        .child(Button::new("mcp-cancel").small().ghost().label("Cancel").on_click(move |_, _, cx| {
                            let _ = p_cancel.update(cx, |p, cx| {
                                p.mcp.form = None;
                                p.mcp.error = None;
                                cx.notify();
                            });
                        })),
                ),
        );
    } else {
        let (p_add, p_import) = (page.clone(), page.clone());
        let folder = scope.clone();
        add = add.child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(Button::new("mcp-add").small().outline().icon(IconName::Plus).label("Add server").on_click(move |_, window, cx| {
                    let _ = p_add.update(cx, |p, cx| {
                        p.mcp.open_form(window, cx);
                        cx.notify();
                    });
                }))
                .child(Button::new("mcp-import").small().ghost().label("Import from Claude Code…").on_click(move |_, _, cx| {
                    let found = dirs::home_dir().map(|h| mcp::found_in_claude(&h, folder.as_deref())).unwrap_or_default();
                    let _ = p_import.update(cx, |p, cx| {
                        p.mcp.found = Some(found);
                        cx.notify();
                    });
                })),
        );
    }
    if let Some(found) = &state.found {
        let here: Vec<String> = scope_servers(&scope, cx).into_iter().map(|s| s.name).collect();
        let mut rows = div().flex().flex_col().gap_1().child(div().text_sm().text_color(muted).child("In Claude Code's settings:"));
        let fresh: Vec<&Found> = found.iter().filter(|f| !here.contains(&f.server.name)).collect();
        if fresh.is_empty() {
            rows = rows.child(div().text_sm().text_color(muted).child("Nothing new to import."));
        }
        for f in fresh {
            let (p, f2) = (page.clone(), Found { server: f.server.clone(), values: f.values.clone() });
            let what = if f.server.is_remote() { f.server.url.clone() } else { format!("{} {}", f.server.command, f.server.args.join(" ")) };
            rows = rows.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(f.server.name.clone()))
                    .child(div().flex_1().min_w_0().truncate().text_sm().text_color(muted).child(what))
                    .child(Button::new(SharedString::from(format!("import-{}", f.server.name))).xsmall().outline().label("Import").on_click(move |_, _, cx| {
                        import(&p, Found { server: f2.server.clone(), values: f2.values.clone() }, cx)
                    })),
            );
        }
        add = add.child(rows);
    }
    let error = state.error.clone();
    let _ = window;
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_3()
        .child(scope_bar)
        .child(div().text_sm().text_color(muted).child("Tool servers every agent gets at launch, each in its own way (Claude Code, Cursor, Codex). Nothing in their own settings changes."))
        .child(list)
        .child(add)
        .children(error.map(|e| div().text_sm().text_color(danger).child(e)))
        .child(div().text_xs().text_color(muted).child(match keleuo_core::secrets::supported() {
            Ok(()) => "Secrets are kept in the system's key store and given to servers as environment variables when an agent starts. Servers that sign in through a browser (OAuth) aren't shared yet.".to_string(),
            Err(why) => why,
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn a_command_line_splits_into_words_keeping_quoted_spaces() {
        assert_eq!(words("npx -y @mcp/server \"my dir\""), ["npx", "-y", "@mcp/server", "my dir"]);
        assert_eq!(words("  uvx  mcp-server-postgres "), ["uvx", "mcp-server-postgres"]);
    }
}
