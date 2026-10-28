//! The window's icons: the Lucide icons it uses beyond gpui-component's default set (embedded
//! one by one, not the whole catalog), and the icon and colour of a file, a folder and a
//! skill's source. Colour carries meaning here: file types, skill sources and status; the one
//! accent (the theme's ring) is left for focus.

use std::borrow::Cow;

use gpui_kit::assets::{IconName as Lucide, icon_assets};
use gpui_kit::component::{ActiveTheme, Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{App, AssetSource, Hsla, SharedString};
use keleuo_core::skills::Kind;

icon_assets!(
    pub ExtraIcons,
    [
        FileCode,
        FileText,
        FileBraces,
        FileCog,
        FileImage,
        FileTerminal,
        FileArchive,
        FileLock,
        Folder,
        FolderOpen,
        FolderGit,
        Sparkles,
        Puzzle,
        UserRound,
        SquareSlash,
        ScrollText,
        FolderCode,
        Workflow,
        Paperclip,
        ArrowUp,
        Square,
        Copy,
        Monitor,
        FilePlus,
        FolderPlus,
        FileDiff,
        MessageCircleQuestionMark,
        CornerDownRight,
        Download,
        Pencil,
        Trash,
        RefreshCw,
        GitBranch,
        GitFork,
        CornerDownLeft,
        // Paste in the Files menu, the message box's 🎤, Settings > Tools, Retry.
        Clipboard,
        Mic,
        Toolbox,
        Redo,
        // Send page to chat, in the web panel.
        MessageSquareShare,
        // Settings > Tools > Packs: a pack, its services, where keys are kept.
        Package,
        Image,
        AudioLines,
        Box,
        Lock,
    ]
);

/// gpui-component's icons plus [`ExtraIcons`].
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> gpui_kit::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> gpui_kit::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

fn hex(rgb: u32) -> Hsla {
    gpui_kit::rgb(rgb).into()
}

/// A colour tuned for the dark theme, made deep enough to read on a light one.
fn tone(rgb: u32, cx: &App) -> Hsla {
    let c = hex(rgb);
    if cx.theme().is_dark() { c } else { Hsla { l: (c.l * 0.62).min(0.42), s: (c.s * 1.1).min(1.0), ..c } }
}

/// Folders: one warm tone, distinct from every file colour.
pub fn folder(open: bool, cx: &App) -> Icon {
    Icon::new(if open { Lucide::FolderOpen } else { Lucide::Folder })
        .xsmall()
        .text_color(tone(0xc9a766, cx))
}

/// A file's icon by its type, coloured by language or kind.
pub fn file(name: &str, cx: &App) -> Icon {
    let (icon, color) = file_kind(name);
    let color = color.map(|c| tone(c, cx)).unwrap_or(cx.theme().muted_foreground);
    Icon::new(icon).xsmall().text_color(color)
}

/// The icon and colour (`None`: the quiet text colour) for a file name.
pub fn file_kind(name: &str) -> (Lucide, Option<u32>) {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".lock") || lower == "package-lock.json" {
        return (Lucide::FileLock, None);
    }
    if matches!(lower.as_str(), "makefile" | "dockerfile" | "justfile" | ".gitignore" | ".editorconfig") {
        return (Lucide::FileCog, Some(0x9aa5b1));
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "rs" => (Lucide::FileCode, Some(0xdc8d5f)),
        "py" | "pyi" => (Lucide::FileCode, Some(0xd8c06a)),
        "js" | "mjs" | "cjs" | "jsx" => (Lucide::FileCode, Some(0xd8c86a)),
        "ts" | "tsx" | "mts" => (Lucide::FileCode, Some(0x6fa6d6)),
        "go" => (Lucide::FileCode, Some(0x6cc6d6)),
        "c" | "h" | "cc" | "cpp" | "hpp" | "m" | "mm" => (Lucide::FileCode, Some(0x8fa9c9)),
        "cs" | "fs" => (Lucide::FileCode, Some(0xa486d6)),
        "swift" => (Lucide::FileCode, Some(0xe2805f)),
        "java" | "kt" | "kts" | "scala" => (Lucide::FileCode, Some(0xc9925f)),
        "rb" | "erb" => (Lucide::FileCode, Some(0xd56c6c)),
        "html" | "htm" | "vue" | "svelte" => (Lucide::FileCode, Some(0xe0805f)),
        "css" | "scss" | "less" => (Lucide::FileCode, Some(0x6fa6d6)),
        "lua" | "zig" | "nim" | "ex" | "exs" | "hs" | "ml" | "sql" | "php" | "dart" | "r" => {
            (Lucide::FileCode, Some(0x8fb7c8))
        }
        "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" => (Lucide::FileTerminal, Some(0x8cc471)),
        "json" | "jsonc" | "json5" => (Lucide::FileBraces, Some(0xcdb768)),
        "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "plist" | "xml" | "env" => (Lucide::FileCog, Some(0x9aa5b1)),
        "md" | "mdx" | "markdown" | "rst" | "adoc" => (Lucide::FileText, Some(0x8fb4dc)),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "tiff" | "pdf" => {
            (Lucide::FileImage, Some(0xc58bd6))
        }
        "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "7z" | "dmg" => (Lucide::FileArchive, None),
        _ => (Lucide::FileText, None),
    }
}

/// Where a skill comes from, by its group's key: the project, the user, a plugin, or the
/// project's instruction files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Project,
    User,
    Plugin,
    Docs,
}

impl Source {
    pub fn of_group(key: &str) -> Self {
        match key {
            "project" => Source::Project,
            "user" => Source::User,
            "docs" => Source::Docs,
            _ => Source::Plugin,
        }
    }

    /// The source's colour: green for the project, cyan for the user, violet for plugins.
    pub fn color(self, cx: &App) -> Hsla {
        let theme = cx.theme();
        match self {
            Source::Project | Source::Docs => theme.green,
            Source::User => theme.cyan,
            Source::Plugin => theme.magenta,
        }
    }

    /// The group header's icon.
    pub fn icon(self) -> Lucide {
        match self {
            Source::Project => Lucide::FolderCode,
            Source::User => Lucide::UserRound,
            Source::Plugin => Lucide::Puzzle,
            Source::Docs => Lucide::ScrollText,
        }
    }
}

/// An entry's icon by its kind: a skill, a subagent definition, a command, instructions.
pub fn entry(kind: Kind) -> Lucide {
    match kind {
        Kind::Skill => Lucide::Sparkles,
        Kind::Agent => Lucide::Bot,
        Kind::Command => Lucide::SquareSlash,
        Kind::Instructions => Lucide::ScrollText,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn files_get_an_icon_and_colour_by_type() {
        assert!(matches!(file_kind("main.rs"), (Lucide::FileCode, Some(_))));
        assert!(matches!(file_kind("README.md"), (Lucide::FileText, Some(_))));
        assert!(matches!(file_kind("Cargo.toml"), (Lucide::FileCog, Some(_))));
        assert!(matches!(file_kind("Cargo.lock"), (Lucide::FileLock, None)));
        assert!(matches!(file_kind("logo.PNG"), (Lucide::FileImage, Some(_))));
        assert!(matches!(file_kind("notes"), (Lucide::FileText, None)));
    }

    #[::core::prelude::v1::test]
    fn every_extra_icon_is_embedded() {
        for icon in [Lucide::FileCode, Lucide::Sparkles, Lucide::Puzzle, Lucide::FolderOpen, Lucide::Download, Lucide::Pencil, Lucide::Trash, Lucide::RefreshCw] {
            let path = icon.path();
            assert!(AppAssets.load(&path).unwrap().is_some(), "{path}");
        }
        // gpui-component's own icons still load.
        assert!(AppAssets.load("icons/chevron-down.svg").unwrap().is_some());
    }
}
