//! Window actions and their keys.

use gpui_kit::*;

actions!(
    workbench,
    [
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
        MoveTabToOtherHalf,
        GoBack,
        GoForward,
    ]
);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
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
        KeyBinding::new("cmd-shift-w", CloseSession, None),
        KeyBinding::new("cmd-\\", SplitRight, None),
        KeyBinding::new("cmd-shift-\\", SplitDown, None),
        KeyBinding::new("cmd-alt-\\", MoveTabToOtherHalf, None),
        KeyBinding::new("ctrl--", GoBack, None),
        KeyBinding::new("ctrl-shift--", GoForward, None),
    ]);
}

pub fn menus() -> Vec<Menu> {
    vec![
        Menu::new("Workbench").items([MenuItem::action("Quit Workbench", Quit)]),
        Menu::new("File").items([
            MenuItem::action("Open Folder...", OpenFolder),
            MenuItem::action("Save", Save),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Rename Tab", RenameTab),
            MenuItem::separator(),
            MenuItem::action("New Claude Session", NewSession),
            MenuItem::action("Close Claude Session", CloseSession),
            MenuItem::action("New Terminal", NewTerminal),
        ]),
        Menu::new("View").items([
            MenuItem::action("Sidebar", ToggleSidebar),
            MenuItem::action("Split Right", SplitRight),
            MenuItem::action("Split Down", SplitDown),
            MenuItem::action("Move Tab to Other Half", MoveTabToOtherHalf),
            MenuItem::separator(),
            MenuItem::action("Focus on Files", FocusRight),
            MenuItem::action("Focus on Claude", FocusClaude),
            MenuItem::action("Markdown: Edit or Preview", TogglePreview),
            MenuItem::action("Ask Claude", AskClaude),
            MenuItem::separator(),
            MenuItem::action("Back", GoBack),
            MenuItem::action("Forward", GoForward),
        ]),
    ]
}
