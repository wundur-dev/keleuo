//! Window actions and their keys.

use gpui_kit::*;

actions!(
    workbench,
    [
        Quit,
        OpenFolder,
        Save,
        CloseTab,
        ToggleSkills,
        ToggleFiles,
        ToggleClaude,
        ToggleTerminal,
        FocusEditor,
        FocusClaude,
        TogglePreview,
        RenameTab,
        AskClaude,
        NewTerminal,
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
        KeyBinding::new("cmd-1", ToggleSkills, None),
        KeyBinding::new("cmd-2", ToggleFiles, None),
        KeyBinding::new("cmd-3", ToggleClaude, None),
        KeyBinding::new("cmd-j", ToggleTerminal, None),
        KeyBinding::new("ctrl-`", ToggleTerminal, None),
        KeyBinding::new("cmd-shift-e", FocusEditor, None),
        KeyBinding::new("cmd-shift-c", FocusClaude, None),
        KeyBinding::new("cmd-e", TogglePreview, None),
        KeyBinding::new("f2", RenameTab, None),
        KeyBinding::new("cmd-l", AskClaude, None),
        KeyBinding::new("cmd-t", NewTerminal, None),
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
            MenuItem::action("New Terminal", NewTerminal),
        ]),
        Menu::new("View").items([
            MenuItem::action("Skills", ToggleSkills),
            MenuItem::action("Files", ToggleFiles),
            MenuItem::action("Claude", ToggleClaude),
            MenuItem::action("Terminal", ToggleTerminal),
            MenuItem::separator(),
            MenuItem::action("Focus on Editor", FocusEditor),
            MenuItem::action("Focus on Claude", FocusClaude),
            MenuItem::action("Markdown: Edit or Preview", TogglePreview),
            MenuItem::action("Ask Claude", AskClaude),
            MenuItem::separator(),
            MenuItem::action("Back", GoBack),
            MenuItem::action("Forward", GoForward),
        ]),
    ]
}
