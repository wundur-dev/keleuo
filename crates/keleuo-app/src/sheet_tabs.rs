//! Sheet tabs, after an earlier app's: a strip of tabs joined to the edge of the content
//! they belong to. The shown tab takes the content's colour and joins it with no line between,
//! with small outward flares where it meets the line and a thin accent line along its free
//! edge; the others sit back a little, shorter and in a muted grey, tucked under the line. Both
//! sides are softly slanted and the tabs overlap, the shown one in front (variant B).
//!
//! Two orientations: hanging from the content above (tool panels, tabs along the bottom) and
//! standing on the content below (documents, tabs along the top).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::*;

/// The tabs' shape, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// How far each side leans in from the joined edge to the free edge.
    pub slant: f32,
    /// How much a tab overlaps the next.
    pub overlap: f32,
    /// The free corners' radius.
    pub radius: f32,
    /// Room between a side and the label.
    pub pad: f32,
    /// The shown tab's outward curves where it meets the line.
    pub flare: f32,
}

pub const GEOMETRY: Geometry = Geometry { slant: 5.0, overlap: 6.0, radius: 5.0, pad: 11.0, flare: 5.0 };

/// How far the shown tab's free edge reaches past the others'.
pub const RECESS: f32 = 2.0;
/// The accent line along the shown tab's free edge.
pub const ACCENT: f32 = 2.0;
/// Which edge of the strip the tabs join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Joined {
    /// Tabs hang from the strip's top (they sit under the content: tool panels).
    Top,
    /// Tabs stand on the strip's bottom (they sit over the content: documents).
    Bottom,
}

/// A strip's colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// The content the strip joins, which the shown tab takes, and its text.
    pub content: Hsla,
    pub content_text: Hsla,
    /// The strip's ground.
    pub ground: Hsla,
    /// The other tabs, under the pointer, and their text.
    pub face: Hsla,
    pub face_hover: Hsla,
    pub face_text: Hsla,
    /// The tabs' outline and the line along the joined edge.
    pub edge: Hsla,
    pub line: Hsla,
    pub accent: Hsla,
}

/// Mix two colours (`t` = 0 gives `a`).
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (Rgba::from(a), Rgba::from(b));
    let l = |x: f32, y: f32| x + (y - x) * t;
    Rgba { r: l(a.r, b.r), g: l(a.g, b.g), b: l(a.b, b.b), a: l(a.a, b.a) }.into()
}

impl Palette {
    /// Colours for a strip on a dark `ground` joined to content of colour `content`.
    pub fn new(ground: Hsla, text: Hsla, content: Hsla, content_text: Hsla, border: Hsla, accent: Hsla) -> Self {
        // On a dark ground the others are a muted grey a step toward the text, so the shown
        // one (as dark as the content) stands out.
        let face = mix(ground, text, 0.13);
        Palette {
            content,
            content_text,
            ground,
            face,
            face_hover: mix(face, text, 0.08),
            face_text: mix(text, face, 0.25),
            edge: mix(border, face, 0.3),
            line: border,
            accent,
        }
    }
}

/// The order to paint in: the others from the right (each one's slanted side over the next),
/// then the shown one, in front.
pub fn paint_order(selected: &[bool]) -> Vec<usize> {
    (0..selected.len())
        .rev()
        .filter(|&i| !selected[i])
        .chain((0..selected.len()).filter(|&i| selected[i]))
        .collect()
}

/// A tab's outline as `(joined left, free corners..., joined right)` in a slot `x0..x1`, with
/// the joined edge at `joined_y` and the free edge at `free_y`: the joined corners sharp, the
/// free ones rounded (quadratic curves sampled into points).
pub fn outline(x0: f32, x1: f32, joined_y: f32, free_y: f32, g: Geometry) -> Vec<Point<f32>> {
    let corners = [
        point(x0, joined_y),
        point(x0 + g.slant, free_y),
        point(x1 - g.slant, free_y),
        point(x1, joined_y),
    ];
    let mut out = vec![corners[0]];
    for i in 1..3 {
        rounded_corner(&mut out, corners[i - 1], corners[i], corners[i + 1], g.radius);
    }
    out.push(corners[3]);
    out
}

fn rounded_corner(out: &mut Vec<Point<f32>>, from: Point<f32>, at: Point<f32>, to: Point<f32>, r: f32) {
    let (ax, ay) = (from.x - at.x, from.y - at.y);
    let (bx, by) = (to.x - at.x, to.y - at.y);
    let (la, lb) = ((ax * ax + ay * ay).sqrt(), (bx * bx + by * by).sqrt());
    if r <= 0.0 || la < 0.01 || lb < 0.01 {
        out.push(at);
        return;
    }
    let t = r.min(la / 2.0).min(lb / 2.0);
    let p = point(at.x + ax / la * t, at.y + ay / la * t);
    let q = point(at.x + bx / lb * t, at.y + by / lb * t);
    const STEPS: usize = 6;
    for k in 0..=STEPS {
        let s = k as f32 / STEPS as f32;
        let u = 1.0 - s;
        out.push(point(
            u * u * p.x + 2.0 * u * s * at.x + s * s * q.x,
            u * u * p.y + 2.0 * u * s * at.y + s * s * q.y,
        ));
    }
}

/// The shown tab with its flares: from the line `flare` out to the left, curving into the left
/// side, along the outline, and out of the right side back to the line.
pub fn flared(x0: f32, x1: f32, joined_y: f32, free_y: f32, g: Geometry) -> Vec<Point<f32>> {
    let mut pts = outline(x0, x1, joined_y, free_y, g);
    let r = g.flare;
    if r <= 0.0 {
        return pts;
    }
    let dir = if free_y > joined_y { 1.0 } else { -1.0 };
    let height = (free_y - joined_y).abs();
    let lean = g.slant * r / height;
    // The side points within the flares go: the curves take them over.
    pts.retain(|p| (p.y - joined_y).abs() > r || p.y == joined_y);
    pts.remove(0);
    pts.pop();
    let curve = |corner: Point<f32>, side: Point<f32>, out: Point<f32>| -> Vec<Point<f32>> {
        (0..=6)
            .map(|k| {
                let s = k as f32 / 6.0;
                let u = 1.0 - s;
                point(
                    u * u * out.x + 2.0 * u * s * corner.x + s * s * side.x,
                    u * u * out.y + 2.0 * u * s * corner.y + s * s * side.y,
                )
            })
            .collect()
    };
    let left = curve(point(x0, joined_y), point(x0 + lean, joined_y + dir * r), point(x0 - r, joined_y));
    let mut right = curve(point(x1, joined_y), point(x1 - lean, joined_y + dir * r), point(x1 + r, joined_y));
    right.reverse();
    left.into_iter().chain(pts).chain(right).collect()
}

fn path(points: &[Point<f32>], fill: bool) -> Option<Path<Pixels>> {
    let mut b = if fill { PathBuilder::fill() } else { PathBuilder::stroke(px(1.)) };
    let mut it = points.iter();
    let first = it.next()?;
    b.move_to(point(px(first.x), px(first.y)));
    for p in it {
        b.line_to(point(px(p.x), px(p.y)));
    }
    if fill {
        b.close();
    }
    b.build().ok()
}

/// Paint one tab in `bounds` (its slot across, the strip's height down).
pub fn paint_tab(bounds: Bounds<Pixels>, joined: Joined, shown: bool, hot: bool, palette: &Palette, window: &mut Window) {
    let g = GEOMETRY;
    let x0 = f32::from(bounds.origin.x);
    let x1 = x0 + f32::from(bounds.size.width);
    let top = f32::from(bounds.origin.y);
    let bottom = top + f32::from(bounds.size.height);
    // The free edge stops short of the strip's far side; the others sit back further.
    let (joined_y, free_y, toward) = match joined {
        Joined::Top => (top, bottom - 2.0, 1.0),
        Joined::Bottom => (bottom, top + 2.0, -1.0),
    };
    if shown {
        let pts = flared(x0, x1, joined_y, free_y, g);
        if let Some(p) = path(&pts, true) {
            window.paint_path(p, palette.content);
        }
        if let Some(p) = path(&pts, false) {
            window.paint_path(p, palette.edge);
        }
        // The accent along the free edge, inside it.
        let inset = g.radius.max(3.0);
        let y = free_y - toward * (ACCENT / 2.0 + 0.5);
        let (left, right) = (x0 + g.slant + inset, x1 - g.slant - inset);
        if right > left {
            window.paint_quad(fill(
                Bounds::new(point(px(left), px(y - ACCENT / 2.0)), size(px(right - left), px(ACCENT))),
                palette.accent,
            ));
        }
    } else {
        let joined_y = joined_y + toward;
        let free_y = free_y - toward * RECESS;
        let pts = outline(x0, x1, joined_y, free_y, g);
        if let Some(p) = path(&pts, true) {
            window.paint_path(p, if hot { palette.face_hover } else { palette.face });
        }
        if let Some(p) = path(&pts, false) {
            window.paint_path(p, palette.edge);
        }
    }
}

/// What one strip remembers between its layout and its paint: each tab records its slot as it
/// is laid out, and one canvas behind them all paints the shapes in the right order.
#[derive(Default)]
pub struct StripState {
    slots: RefCell<Vec<(usize, Bounds<Pixels>)>>,
    hot: Cell<Option<usize>>,
}

impl StripState {
    /// The shapes, behind the labels: the line along the joined edge, the others from the
    /// right, the shown one last. Clears the slots for this frame's tabs to fill.
    pub fn backdrop(self: &Rc<Self>, joined: Joined, palette: Palette, shown: Option<usize>) -> impl IntoElement {
        self.slots.borrow_mut().clear();
        let state = self.clone();
        canvas(
            |_, _, _| (),
            move |b, _, window, _| {
                let y = match joined {
                    Joined::Top => b.origin.y,
                    Joined::Bottom => b.origin.y + b.size.height - px(1.),
                };
                window.paint_quad(fill(Bounds::new(point(b.origin.x, y), size(b.size.width, px(1.))), palette.line));
                let slots = state.slots.borrow().clone();
                let selected: Vec<bool> = slots.iter().map(|(ix, _)| Some(*ix) == shown).collect();
                for i in paint_order(&selected) {
                    let (ix, bounds) = slots[i];
                    // Tabs scrolled out of the strip do not paint over its neighbours.
                    if bounds.right() < b.left() || bounds.left() > b.right() {
                        continue;
                    }
                    paint_tab(bounds, joined, selected[i], state.hot.get() == Some(ix), &palette, window);
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// Put inside tab `ix`: records where it was laid out.
    pub fn recorder(self: &Rc<Self>, ix: usize) -> impl IntoElement {
        let state = self.clone();
        canvas(move |b, _, _| state.slots.borrow_mut().push((ix, b)), |_, _, _, _| {})
            .absolute()
            .top_0()
            .left_0()
            .size_full()
    }

    /// For tab `ix`'s `on_hover`: the tab under the pointer is drawn a shade lighter.
    pub fn hover(self: &Rc<Self>, ix: usize) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
        let state = self.clone();
        move |hovered, window, _| {
            let now = if *hovered {
                Some(ix)
            } else if state.hot.get() == Some(ix) {
                None
            } else {
                state.hot.get()
            };
            if now != state.hot.get() {
                state.hot.set(now);
                window.refresh();
            }
        }
    }
}

/// Room between a tab's side and its label.
pub fn label_pad() -> Pixels {
    px(GEOMETRY.pad + GEOMETRY.slant)
}

/// A tab's box in a strip: its slot recorder, label padding and overlap with the one before.
pub fn tab_box(state: &Rc<StripState>, id: impl Into<ElementId>, ix: usize, n: usize, joined: Joined) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .flex_none()
        .h_full()
        .px(label_pad())
        .when(n > 0, |d| d.ml(px(-GEOMETRY.overlap)))
        .flex()
        .items_center()
        .gap_1()
        .map(|d| match joined {
            // Labels sit in the tab's body, away from the recessed free edge.
            Joined::Top => d.pb(px(2.)),
            Joined::Bottom => d.pt(px(2.)),
        })
        .child(state.recorder(ix))
        .on_hover(state.hover(ix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn shown_tab_paints_last() {
        assert_eq!(paint_order(&[false, true, false, false]), [3, 2, 0, 1]);
    }

    #[::core::prelude::v1::test]
    fn outline_and_flares() {
        let g = GEOMETRY;
        let o = outline(0.0, 80.0, 0.0, 20.0, g);
        assert_eq!(o.first().copied(), Some(point(0.0, 0.0)));
        assert_eq!(o.last().copied(), Some(point(80.0, 0.0)));
        assert!(o.iter().all(|p| p.y >= 0.0 && p.y <= 20.0));
        let f = flared(0.0, 80.0, 0.0, 20.0, g);
        // The flares reach past the slot along the line.
        assert_eq!(f.first().map(|p| (p.x, p.y)), Some((-g.flare, 0.0)));
        assert_eq!(f.last().map(|p| (p.x, p.y)), Some((80.0 + g.flare, 0.0)));
        // Mirrored: standing on the line at the bottom.
        let m = flared(0.0, 80.0, 26.0, 2.0, g);
        assert!(m.iter().all(|p| p.y <= 26.0 && p.y >= 2.0));
    }
}
