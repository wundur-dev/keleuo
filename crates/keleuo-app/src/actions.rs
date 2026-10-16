//! Window actions and their keys.

use gpui_kit::*;

actions!(
    keleuo,
    [
        ShowAbout,
        ShowSettings,
        Quit,
        OpenFolder,
        Save,
        CloseTab,
        ToggleSidebar,
        FocusRight,
        FocusClaude,
        TogglePreview,
        RenameTab,
        AskClaude,
        NewTerminal,
        NewSession,
        CloseSession,
        SplitRight,
        SplitDown,
        GoBack,
        GoForward,
        ResetLayout,
        NextSession,
        PrevSession,
        RenameSession,
        RenameFolder,
        ToggleSessions,
        AppearanceSystem,
        AppearanceLight,
        AppearanceDark,
        ToggleNotifyWaiting,
        ToggleNotifyFinished,
        ToggleCodeFont,
        CopyConversation,
        FindInChat,
        OpenWebPage,
        // In a web page (see web_panel.rs).
        FocusAddress,
        PageCopy,
        PageCut,
        PagePaste,
        PageSelectAll,
        PageUndo,
        PageRedo,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", ShowSettings, None),
        KeyBinding::new("cmd-o", OpenFolder, None),
        KeyBinding::new("cmd-s", Save, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("cmd-b", ToggleSidebar, None),
        KeyBinding::new("cmd-shift-e", FocusRight, None),
        KeyBinding::new("cmd-shift-c", FocusClaude, None),
        KeyBinding::new("cmd-e", TogglePreview, None),
        KeyBinding::new("f2", RenameTab, None),
        KeyBinding::new("cmd-l", AskClaude, None),
        KeyBinding::new("cmd-t", NewTerminal, None),
        KeyBinding::new("cmd-n", NewSession, None),
        // In a terminal Tab is the program's (completion, a TUI's fields), not the window's move
        // to the next control: unbound there, it reaches the terminal, which sends it on.
        KeyBinding::new("tab", gpui_kit::NoAction, Some("Terminal")),
        KeyBinding::new("shift-tab", gpui_kit::NoAction, Some("Terminal")),
        KeyBinding::new("cmd-shift-w", CloseSession, None),
        KeyBinding::new("cmd-\\", SplitRight, None),
        KeyBinding::new("cmd-shift-\\", SplitDown, None),
        KeyBinding::new("cmd-shift-b", ToggleSessions, None),
        KeyBinding::new("cmd-alt-c", CopyConversation, None),
        // Find in the conversation: Cmd+F on macOS (and Ctrl+F, as elsewhere), Ctrl+F elsewhere.
        KeyBinding::new("secondary-f", FindInChat, Some("Chat")),
        KeyBinding::new("ctrl-f", FindInChat, Some("Chat")),
        // In a terminal Ctrl+F is the program's (the shell's cursor), not Find.
        KeyBinding::new("ctrl-f", gpui_kit::NoAction, Some("Terminal")),
        KeyBinding::new("ctrl-tab", NextSession, None),
        KeyBinding::new("ctrl-shift-tab", PrevSession, None),
        KeyBinding::new("ctrl--", GoBack, None),
        KeyBinding::new("ctrl-shift--", GoForward, None),
        // In a web panel, over the window's own (Cmd+L is Ask Claude elsewhere).
        KeyBinding::new("cmd-l", FocusAddress, Some("WebPanel")),
        KeyBinding::new("cmd-c", PageCopy, Some("WebPanel")),
        KeyBinding::new("cmd-x", PageCut, Some("WebPanel")),
        KeyBinding::new("cmd-v", PagePaste, Some("WebPanel")),
        KeyBinding::new("cmd-a", PageSelectAll, Some("WebPanel")),
        KeyBinding::new("cmd-z", PageUndo, Some("WebPanel")),
        KeyBinding::new("cmd-shift-z", PageRedo, Some("WebPanel")),
    ]);
}

/// The menu bar; its checked items follow the settings (set it again when they change).
pub fn menus(cx: &App) -> Vec<Menu> {
    let always_proportional = cx.global::<crate::AppSettings>().0.borrow().always_proportional;
    let notify = cx.global::<crate::AppSettings>().0.borrow().notify.clone();
    let appearance = cx.global::<crate::AppSettings>().0.borrow().appearance.clone();
    vec![
        Menu::new("Keleuo").items([
            MenuItem::action("About Keleuo", ShowAbout),
            MenuItem::separator(),
            MenuItem::action("Settings…", ShowSettings),
            MenuItem::separator(),
            MenuItem::action("Alert When a Session Asks", ToggleNotifyWaiting).checked(notify.waiting),
            MenuItem::action("Alert When a Turn Finishes", ToggleNotifyFinished).checked(notify.finished),
            MenuItem::separator(),
            MenuItem::action("Quit Keleuo", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("Open Folder...", OpenFolder),
            MenuItem::action("Save", Save),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Rename Tab", RenameTab),
            MenuItem::separator(),
            MenuItem::action("New Session", NewSession),
            MenuItem::action("Close Session", CloseSession),
            MenuItem::action("Rename Session", RenameSession),
            MenuItem::action("Rename Folder…", RenameFolder),
            MenuItem::action("Copy Conversation", CopyConversation),
            MenuItem::action("New Terminal", NewTerminal),
            MenuItem::action("Open Web Page…", OpenWebPage),
        ]),
        Menu::new("View").items([
            MenuItem::action("Session List", ToggleSessions),
            MenuItem::action("Sidebar", ToggleSidebar),
            MenuItem::action("Split Right", SplitRight),
            MenuItem::action("Split Down", SplitDown),
            MenuItem::action("Reset Layout", ResetLayout),
            MenuItem::separator(),
            MenuItem::action("Appearance: System", AppearanceSystem).checked(!matches!(appearance.as_str(), "light" | "dark")),
            MenuItem::action("Appearance: Light", AppearanceLight).checked(appearance == "light"),
            MenuItem::action("Appearance: Dark", AppearanceDark).checked(appearance == "dark"),
            MenuItem::separator(),
            MenuItem::action("Monospace When a Message Has Code", ToggleCodeFont).checked(!always_proportional),
            MenuItem::separator(),
            MenuItem::action("Zoom Files and Terminals", FocusRight),
            MenuItem::action("Zoom Claude", FocusClaude),
            MenuItem::action("Markdown: Edit or Preview", TogglePreview),
            MenuItem::action("Ask Claude", AskClaude),
            MenuItem::separator(),
            MenuItem::action("Next Session", NextSession),
            MenuItem::action("Previous Session", PrevSession),
            MenuItem::action("Back", GoBack),
            MenuItem::action("Forward", GoForward),
        ]),
    ]
}

/// The menu bar as one menu, for the title bar's menu button (every system; on macOS the menu
/// bar is there too): each of its menus a submenu, built when it opens.
pub fn menu_popup(
    menu: gpui_kit::component::menu::PopupMenu,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
) -> gpui_kit::component::menu::PopupMenu {
    let names: Vec<SharedString> = menus(cx).into_iter().map(|m| m.name).collect();
    names.into_iter().enumerate().fold(menu, |menu, (ix, name)| {
        menu.submenu(name, window, cx, move |sub, _, cx| {
            let items = menus(cx).into_iter().nth(ix).map(|m| m.items).unwrap_or_default();
            items.into_iter().fold(sub, |sub, item| match item {
                MenuItem::Separator => sub.separator(),
                MenuItem::Action { name, action, checked, disabled, .. } => sub.menu_with_check_and_disabled(name, checked, action, disabled),
                MenuItem::Submenu(_) | MenuItem::SystemMenu(_) => sub,
            })
        })
    })
}
