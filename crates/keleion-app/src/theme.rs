//! The look: distinct surface tones so each region reads as itself (sidebar darkest, Claude,
//! then the editor), clear dividers, an obvious active tab and quiet inactive ones.

use gpui_kit::component::Theme;
use gpui_kit::*;

fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

pub fn apply(cx: &mut App) {
    Theme::update(cx, |t| {
        let c = &mut t.colors;
        // Surfaces, darkest to lightest: sidebar, tab bars, Claude, editor.
        c.sidebar = hex(0x111214);
        c.tab_bar = hex(0x16171a);
        c.background = hex(0x1c1d21);
        c.secondary = hex(0x24262b);
        c.secondary_hover = hex(0x2c2f35);
        // Lines between regions: visible but quiet.
        c.border = hex(0x30333a);
        // The active tab is lifted and bright; inactive tabs recede.
        c.tab_active = hex(0x24262b);
        c.tab_active_foreground = hex(0xf2f3f5);
        c.tab_foreground = hex(0x8b8f98);
        c.muted_foreground = hex(0x8b8f98);
        c.foreground = hex(0xdfe1e5);
        c.accent = hex(0x2a3a55);
        c.ring = hex(0x5b8def);
    });
}
