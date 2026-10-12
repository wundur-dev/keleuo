//! Terminal colours: the 16 ANSI colours (a dark theme), the 6x6x6 cube and the grey ramp.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

const ANSI: [u32; 16] = [
    0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, // normal
    0x666666, 0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea, // bright
];

pub const FOREGROUND: u32 = 0xd4d4d4;
pub const BACKGROUND: u32 = 0x18191b;
pub const CURSOR: u32 = 0xc0c0c0;
pub const SELECTION: u32 = 0x3a4a64;

pub fn indexed(i: u8) -> u32 {
    match i {
        0..=15 => ANSI[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v as u32 * 40 };
            let (r, g, b) = (level(i / 36), level((i / 6) % 6), level(i % 6));
            (r << 16) | (g << 8) | b
        }
        _ => {
            let v = 8 + (i as u32 - 232) * 10;
            (v << 16) | (v << 8) | v
        }
    }
}

fn rgb(c: Rgb) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32
}

/// Resolve a cell colour, honouring colours the program changed with OSC 4/10/11.
pub fn resolve(color: Color, overrides: &Colors, is_fg: bool) -> u32 {
    match color {
        Color::Spec(c) => rgb(c),
        Color::Indexed(i) => overrides[i as usize].map(rgb).unwrap_or_else(|| indexed(i)),
        Color::Named(n) => {
            if let Some(c) = overrides[n] {
                return rgb(c);
            }
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => FOREGROUND,
                NamedColor::Background => BACKGROUND,
                NamedColor::Cursor => CURSOR,
                NamedColor::DimForeground => 0x9a9a9a,
                n if (n as usize) < 16 => ANSI[n as usize],
                // Dim colours: halve the normal ones.
                n if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize)
                    .contains(&(n as usize)) =>
                {
                    let base = ANSI[n as usize - NamedColor::DimBlack as usize];
                    let half = |s: u32| ((base >> s) & 0xff) * 2 / 3;
                    (half(16) << 16) | (half(8) << 8) | half(0)
                }
                _ => {
                    if is_fg {
                        FOREGROUND
                    } else {
                        BACKGROUND
                    }
                }
            }
        }
    }
}

/// Bold text uses the bright variant of the eight basic colours, like most terminals.
pub fn brighten(color: Color) -> Color {
    match color {
        Color::Named(n) if (n as usize) < 8 => Color::Named(n.to_bright()),
        Color::Indexed(i) if i < 8 => Color::Indexed(i + 8),
        c => c,
    }
}
