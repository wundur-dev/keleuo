//! The dock's look, after an earlier app's panel headers and sheet tabs, on top of
//! gpui-component's `DockSkin`.
//!
//! gpui-component splits its dock in two: `gpui_base::dock` owns the behaviour (the layout tree,
//! dragging, dropping, saving) and calls a renderer for everything visible. [`KeleuoSkin`] is
//! that renderer. It hands the parts it does not change (splitters, dock edges, drop
//! placeholders, the zoom and close actions) to the stock `DockSkin`, and draws tab groups
//! itself:
//!
//! - a group of tool panels (Files, Skills, Changes, Agent) gets a title bar: a grip to drag
//!   it by, the shown panel's title and actions, and its options on hover. With several panels
//!   in one group their tabs run along the bottom as sheet tabs joined to the content above;
//! - any other group (files, terminals, the chat) keeps its tabs on top, as sheet tabs standing
//!   on the content below.
//!
//! Tabs and the grip drag with the dock's own `DragPanel`, and drop through
//! `TabGroupContext::drop_panel`, so docking, splitting and saving work as before.

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dock::{
    BasePanelView, DockArea, DockAreaRenderer, DockContext, DockSkin, DragPanel, DropIndicator,
    NodeId, PanelHandle, PanelState, TabGroupContext, TabGroupRenderer,
};
use gpui_kit::component::menu::DropdownMenu;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;
use gpui_kit::base::TestSupportExt;

use crate::sheet_tabs::{self, Joined, Palette, StripState};

/// The panels that are tools (lists beside the work) rather than documents.
const TOOLS: [&str; 4] = ["Files", "Skills", "Changes", "Agent"];

/// A tool group's title bar.
pub const HEADER_HEIGHT: f32 = 34.;
/// A tool group's tabs along the bottom.
pub const BOTTOM_STRIP: f32 = 24.;
/// Space between a tool panel's content and its tabs along the bottom.
pub const STRIP_GAP_ABOVE: f32 = 5.;
/// Space below those tabs, before the panel's bottom edge.
pub const STRIP_GAP_BELOW: f32 = 4.;
/// Space above a document group's tabs, before the panel's top edge.
pub const DOC_STRIP_GAP_ABOVE: f32 = 5.;
/// How far a document group's first tab sits from the panel's left edge.
pub const DOC_STRIP_INSET: f32 = 8.;
/// A document group's tabs along the top.
pub const TOP_STRIP: f32 = 28.;

const PREVIEW: Size<Pixels> = size(px(120.), px(26.));

pub struct KeleuoSkin {
    inner: Rc<DockSkin>,
}

impl KeleuoSkin {
    /// A dock area wearing this skin, and the stock skin's handle (for its settings).
    pub fn dock_area(
        id: &'static str,
        version: Option<usize>,
        window: &mut Window,
        cx: &mut App,
    ) -> (Entity<DockArea>, Rc<DockSkin>) {
        let mut handle = None;
        let area = cx.new(|cx| {
            let inner = DockSkin::new(cx);
            handle = Some(inner.clone());
            DockArea::new(id, version, window, cx).with_renderer(Rc::new(KeleuoSkin { inner }))
        });
        (area, handle.expect("built inside the constructor"))
    }
}

/// How round the work area's corners are where it meets the session list.
pub const CORNER_RADIUS: f32 = 10.;

/// A rounded corner for whatever is under it: the corner's square less a quarter circle,
/// painted in `color` (the surface around). GPUI clips to rectangles only, so rounding an area
/// whose contents paint their own backgrounds takes a cap drawn over it. Place it last in a
/// `relative` parent, which it sits in the given corner of.
pub fn corner_cap(top: bool, left: bool, color: Hsla) -> AnyElement {
    let r = CORNER_RADIUS;
    let d = div().absolute().size(px(r));
    let d = if top { d.top_0() } else { d.bottom_0() };
    let d = if left { d.left_0() } else { d.right_0() };
    d.child(
        canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                // In the cap's square: the corner, the two ends of the arc, the arc's centre.
                let (x0, y0, w) = (b.origin.x, b.origin.y, b.size.width);
                let at = |fx: f32, fy: f32| point(x0 + w * fx, y0 + w * fy);
                let (cx_, cy_) = (if left { 1. } else { 0. }, if top { 1. } else { 0. });
                let corner = at(1. - cx_, 1. - cy_);
                let a = at(cx_, 1. - cy_);
                let z = at(1. - cx_, cy_);
                // A quarter circle as a cubic: its controls 0.5523 of the radius along the
                // tangents.
                let k = 0.5523;
                let ctrl_a = at(cx_ + (1. - 2. * cx_) * k, 1. - cy_);
                let ctrl_b = at(1. - cx_, cy_ + (1. - 2. * cy_) * k);
                let mut path = PathBuilder::fill();
                path.move_to(corner);
                path.line_to(a);
                path.cubic_bezier_to(z, ctrl_a, ctrl_b);
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .size_full(),
    )
    .into_any_element()
}

impl DockAreaRenderer for KeleuoSkin {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.frame(window, cx).bg(cx.theme().background)
    }
    fn split_frame(&self, node: NodeId, axis: Axis, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.split_frame(node, axis, window, cx)
    }
    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.center_frame(window, cx)
    }
    fn render_split_handle(
        &self,
        handle: &gpui_kit::base::ResizeHandleContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.inner.render_split_handle(handle, window, cx)
    }
    fn render_dock(&self, dock: &DockContext, content: AnyElement, window: &mut Window, cx: &mut App) -> AnyElement {
        self.inner.render_dock(dock, content, window, cx)
    }
    fn build_placeholder(&self, state: &PanelState, window: &mut Window, cx: &mut App) -> Option<Arc<dyn BasePanelView>> {
        self.inner.build_placeholder(state, window, cx)
    }
    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(SheetGroup {
            inner: self.inner.tab_group_renderer(),
            strip: Rc::new(StripState::default()),
            scroll: ScrollHandle::new(),
            last_shown: std::cell::Cell::new(None),
        })
    }
}

struct SheetGroup {
    inner: Rc<dyn TabGroupRenderer>,
    strip: Rc<StripState>,
    scroll: ScrollHandle,
    /// The shown tab last frame: when it changes, the strip scrolls it into view.
    last_shown: std::cell::Cell<Option<usize>>,
}

fn is_tool_group(group: &TabGroupContext, cx: &App) -> bool {
    !group.panels().is_empty() && group.panels().iter().all(|p| TOOLS.contains(&p.panel_name(cx)))
}

/// The session's chat: pinned in the centre (see `main`), drawn without a tab.
pub const CHAT: &str = "Chat";

fn is_chat_group(group: &TabGroupContext, cx: &App) -> bool {
    group.panels().iter().any(|p| p.panel_name(cx) == CHAT)
}

fn visible(group: &TabGroupContext, cx: &App) -> Vec<usize> {
    group
        .panels()
        .iter()
        .enumerate()
        .filter(|(_, p)| p.visible(cx))
        .map(|(ix, _)| ix)
        .collect()
}

fn shown_ix(group: &TabGroupContext, cx: &App) -> Option<usize> {
    let shown = group.active_panel()?.panel_id(cx);
    group.panels().iter().position(|p| p.panel_id(cx) == shown)
}

fn title_of(panel: &Arc<dyn BasePanelView>, window: &mut Window, cx: &mut App) -> AnyElement {
    match PanelHandle::of(panel) {
        Some(h) => h.title(window, cx),
        None => SharedString::from(panel.panel_name(cx)).into_any_element(),
    }
}

/// Whether the keyboard is in this group: its accent is brighter.
fn focused(group: &TabGroupContext, window: &Window, cx: &App) -> bool {
    group.active_panel().is_some_and(|p| p.focus_handle(cx).contains_focused(window, cx))
}

/// Start dragging the panel at `ix`, or nothing when the group cannot be rearranged.
fn drag_of(group: &TabGroupContext, ix: usize, cx: &App) -> Option<DragPanel> {
    group.is_draggable().then(|| group.drag_panel(ix, cx)).flatten()
}

fn on_drag_panel<E: StatefulInteractiveElement>(el: E, drag: Option<DragPanel>, panel: Arc<dyn BasePanelView>) -> E {
    match drag {
        Some(drag) => el.on_drag(drag, move |drag: &DragPanel, offset, _, cx| {
            cx.stop_propagation();
            drag.set_drag_offset(offset);
            drag.set_preview_size(PREVIEW);
            let panel = panel.clone();
            cx.new(|_| DragPreview { panel })
        }),
        None => el,
    }
}

/// What follows the pointer while a panel is dragged.
struct DragPreview {
    panel: Arc<dyn BasePanelView>,
}

impl Render for DragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (border, bg, fg) = (theme.ring, theme.secondary, theme.foreground);
        div()
            .h(PREVIEW.height)
            .max_w(px(220.))
            .px_2()
            .flex()
            .items_center()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(bg)
            .opacity(0.9)
            .text_xs()
            .text_color(fg)
            .whitespace_nowrap()
            .overflow_hidden()
            .child(title_of(&self.panel, window, cx))
    }
}

/// The two-by-three dots you drag a panel by.
fn grip(color: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |b, _, window, _| {
            let cx0 = f32::from(b.origin.x) + 2.;
            let cy = f32::from(b.center().y);
            for col in 0..2 {
                for row in -1..=1 {
                    let c = point(px(cx0 + col as f32 * 4.), px(cy + row as f32 * 4.));
                    window.paint_quad(fill(Bounds::centered_at(c, size(px(2.), px(2.))), color).corner_radii(px(0.6)));
                }
            }
        },
    )
    .w(px(10.))
    .h_full()
}

impl SheetGroup {
    /// A tool group's title bar: grip, title, the shown panel's actions. (No options menu:
    /// tool panels are neither closed nor zoomed, so it would be empty.)
    fn header(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> AnyElement {
        let theme = cx.theme();
        // The tool panels' light grey, as the panel below it.
        let (bar, border, fg, muted) = (crate::theme::tool_ground(cx), theme.border, theme.foreground, theme.muted_foreground);
        let Some(ix) = shown_ix(group, cx) else {
            return div().into_any_element();
        };
        let panel = group.panels()[ix].clone();
        let handle = PanelHandle::of(&panel).map(|h| h.panel());
        let buttons = handle.as_ref().and_then(|h| h.toolbar_buttons(window, cx)).unwrap_or_default();
        let title = title_of(&panel, window, cx);
        let drag = drag_of(group, ix, cx);
        let handle_el = on_drag_panel(
            div()
                .id(("panel-grip", panel.panel_id(cx).as_u64() as usize))
                .h_full()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(6.))
                .when(drag.is_some(), |d| d.cursor_grab())
                .child(grip(muted.opacity(0.7)))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        // As a sidebar's labels read (Codex's): regular weight, a little large.
                        .text_size(px(15.))
                        .text_color(fg)
                        .child(title),
                ),
            drag,
            panel.clone(),
        );
        let drop_group = group.clone();
        div()
            .id("panel-header")
            .group("panel-header")
            .h(px(HEADER_HEIGHT))
            .flex_none()
            .pl(px(10.))
            .pr(px(8.))
            .flex()
            .items_center()
            .gap_1()
            .bg(bar)
            .border_b_1()
            .border_color(border)
            .when(group.is_droppable(), |d| {
                d.drag_over::<DragPanel>(|d, _, _, cx| d.bg(cx.theme().drop_target))
                    .on_drop(move |drag: &DragPanel, window, cx| {
                        cx.stop_propagation();
                        drop_group.drop_panel(drag.clone(), None, true, window, cx)
                    })
            })
            .child(handle_el.test_support())
            .children(buttons.into_iter().map(|b| b.xsmall().ghost().tab_stop(false)))
            .into_any_element()
    }

    /// A strip of sheet tabs. `joined`: which edge meets the content.
    #[allow(clippy::too_many_arguments)]
    fn strip(
        &self,
        group: &TabGroupContext,
        joined: Joined,
        height: f32,
        ground: Hsla,
        content: Hsla,
        trailing: Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let theme = cx.theme();
        // Where typing goes: the shown tab's accent is blue in the focused group, a quiet grey in
        // the others.
        let accent = if focused(group, window, cx) { theme.ring } else { theme.border };
        let palette = Palette::new(ground, theme.foreground, content, theme.foreground, theme.border, accent);
        let documents = joined == Joined::Bottom;
        let shown = shown_ix(group, cx);
        let collapsed = group.is_collapsed();
        let backdrop = self.strip.backdrop(joined, palette, shown.filter(|_| !collapsed));

        let mut row = div()
            .id("sheet-tabs")
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_x_scroll()
            .track_scroll(&self.scroll)
            .pl(px(if documents { DOC_STRIP_INSET } else { 4. }));
        let tabs_count = group.panels().len();
        if self.last_shown.replace(shown) != shown
            && let Some(pos) = shown.and_then(|ix| visible(group, cx).iter().position(|v| *v == ix))
        {
            self.scroll.scroll_to_item(pos);
        }
        for (n, ix) in visible(group, cx).into_iter().enumerate() {
            let panel = group.panels()[ix].clone();
            let is_shown = Some(ix) == shown && !collapsed;
            let title = title_of(&panel, window, cx);
            let drag = drag_of(group, ix, cx);
            let closable = documents && group.is_panel_closable(panel.panel_id(cx), cx);
            let (select, close, drop_on, middle) = (group.clone(), group.clone(), group.clone(), group.clone());
            let id = panel.panel_id(cx);
            let (view, middle_view) = (panel.view(), panel.view());
            let tab = sheet_tabs::tab_box(&self.strip, ("sheet-tab", id.as_u64() as usize), ix, n, joined)
                .group("sheet-tab")
                .max_w(px(220.))
                .when(closable, |d| d.pr(sheet_tabs::label_pad() - px(6.)))
                .text_size(px(if documents { 12. } else { 11. }))
                .text_color(if is_shown { palette.content_text } else { palette.face_text })
                .child(div().min_w_0().truncate().child(title))
                .when(closable, |d| {
                    d.child(
                        div()
                            .id(("sheet-close", ix))
                            .flex_none()
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .when(!is_shown, |d| d.invisible().group_hover("sheet-tab", |d| d.visible()))
                            .hover(|d| d.bg(palette.face_hover))
                            .child(Icon::new(IconName::Close).xsmall())
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                let close = close.clone();
                                crate::closing::close_tab(view.clone(), move |w, cx| close.close(id, w, cx), window, cx);
                            }),
                    )
                })
                .on_click(move |_, window, cx| select.select_tab(ix, window, cx))
                .when(closable, |d| {
                    d.on_mouse_down(MouseButton::Middle, move |_, window, cx| {
                        cx.stop_propagation();
                        let middle = middle.clone();
                        crate::closing::close_tab(middle_view.clone(), move |w, cx| middle.close(id, w, cx), window, cx);
                    })
                })
                .when(group.is_droppable() && !collapsed, |d| {
                    d.drag_over::<DragPanel>(|d, _, _, cx| d.border_l_2().border_color(cx.theme().ring))
                        .on_drop(move |drag: &DragPanel, window, cx| {
                            cx.stop_propagation();
                            drop_on.drop_panel(drag.clone(), Some(ix), true, window, cx)
                        })
                });
            row = row.child(on_drag_panel(tab, if collapsed { None } else { drag }, panel).test_support());
        }
        // Room past the last tab to drop a panel at the end.
        let end = group.clone();
        let node = group.node();
        row = row.child(
            div()
                .id("sheet-end")
                .flex_1()
                .min_w(px(24.))
                .h_full()
                .when(group.is_droppable(), |d| {
                    d.drag_over::<DragPanel>(|d, _, _, cx| d.bg(cx.theme().drop_target)).on_drop(
                        move |drag: &DragPanel, window, cx| {
                            cx.stop_propagation();
                            let ix = (drag.source() == node).then(|| tabs_count.saturating_sub(1));
                            end.drop_panel(drag.clone(), ix, false, window, cx)
                        },
                    )
                }),
        );
        div()
            .relative()
            .h(px(height))
            .flex_none()
            .w_full()
            .flex()
            .items_center()
            .bg(ground)
            .child(backdrop)
            .child(row)
            .children(trailing)
            .into_any_element()
    }

    /// The shown document's actions and options, at the right of the top strip.
    fn doc_actions(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let handle = group.active_panel().and_then(PanelHandle::of).map(|h| h.panel())?;
        let buttons = handle.toolbar_buttons(window, cx).unwrap_or_default();
        let closable = group.is_closable();
        let close_group = group.clone();
        let zoom = group.clone();
        let zoomed = group.is_zoomed();
        Some(
            div()
                .flex_none()
                .h_full()
                .px_1()
                .flex()
                .items_center()
                .gap_1()
                .children(buttons.into_iter().map(|b| b.xsmall().ghost().tab_stop(false)))
                .child(
                    Button::new("doc-options")
                        .icon(IconName::Ellipsis)
                        .xsmall()
                        .ghost()
                        .tab_stop(false)
                        .dropdown_menu(move |menu, window, cx| {
                            let (close_group, zoom) = (close_group.clone(), zoom.clone());
                            menu.map(|m| handle.dropdown_menu(m, window, cx))
                                .separator()
                                .item(
                                    gpui_kit::component::menu::PopupMenuItem::new(if zoomed { "Zoom out" } else { "Zoom in" })
                                        .on_click(move |_, window, cx| zoom.toggle_zoom(window, cx)),
                                )
                                .when(closable, |menu| {
                                    menu.item(gpui_kit::component::menu::PopupMenuItem::new("Close").on_click(
                                        move |_, window, cx| {
                                            if let Some(p) = close_group.active_panel() {
                                                let id = p.panel_id(cx);
                                                let group = close_group.clone();
                                                crate::closing::close_tab(p.view(), move |w, cx| group.close(id, w, cx), window, cx);
                                            }
                                        },
                                    ))
                                })
                        }),
                )
                .into_any_element(),
        )
    }
}

impl TabGroupRenderer for SheetGroup {
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        let bg = if is_tool_group(group, cx) { crate::theme::tool_ground(cx) } else { cx.theme().background };
        self.inner.frame(group, window, cx).bg(bg)
    }

    fn content_frame(&self, _: &TabGroupContext, _: &mut Window, _: &mut App) -> Stateful<Div> {
        div().id("active-panel")
    }

    fn render_tab_bar(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> AnyElement {
        // The chat stands alone in the centre, fixed: no tab (its header says what it is).
        if visible(group, cx).is_empty() || is_chat_group(group, cx) {
            return Empty.into_any_element();
        }
        if is_tool_group(group, cx) {
            return self.header(group, window, cx);
        }
        let theme = cx.theme();
        // The strip's ground is the darkest surface, so the shown tab (the content's colour)
        // and the recessed grey ones both read against it.
        let (ground, content) = (crate::theme::strip_ground(cx), theme.background);
        let trailing = self.doc_actions(group, window, cx);
        // Room above the tabs, in the strip's own colour (no line between the shown tab and
        // its content below).
        div()
            .flex_none()
            .w_full()
            .flex()
            .flex_col()
            .bg(ground)
            .pt(px(DOC_STRIP_GAP_ABOVE))
            .child(self.strip(group, Joined::Bottom, TOP_STRIP, ground, content, trailing, window, cx))
            .into_any_element()
    }

    fn render_active_panel(&self, panel: AnyView, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> AnyElement {
        if group.is_collapsed() {
            return Empty.into_any_element();
        }
        let view = div()
            .id("tab-content")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(panel.cached(StyleRefinement::default().absolute().size_full()));
        if !is_tool_group(group, cx) || visible(group, cx).len() < 2 {
            return view.into_any_element();
        }
        let theme = cx.theme();
        // The tabs along a tool group's bottom: the shown one in the panel's grey, on the
        // session list's darker one.
        let (ground, content) = (theme.sidebar, crate::theme::tool_ground(cx));
        // Room in the panel's colour above the tabs, so the list does not run into them (the
        // shown tab still joins the panel through it), and a little below them.
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .bg(content)
            .child(view)
            .child(div().flex_none().h(px(STRIP_GAP_ABOVE)))
            .child(self.strip(group, Joined::Top, BOTTOM_STRIP, ground, content, None, window, cx))
            .child(div().flex_none().h(px(STRIP_GAP_BELOW)))
            .into_any_element()
    }

    fn render_drop_indicator(&self, indicator: DropIndicator, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        self.inner.render_drop_indicator(indicator, window, cx)
    }

    fn render_empty(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        self.inner.render_empty(group, window, cx)
    }
}

#[cfg(test)]
mod tests {
    //! Drag and drop through the skin, in a headless window: from a tool group's bottom tabs
    //! and its title bar grip, into a document group's top tabs.
    use std::time::Duration;

    use gpui_kit::component::dock::{BasePanel, DockLayout, Panel, PanelEvent, panel_handle};
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use gpui_kit::{
        App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, Render, TestAppContext, Window,
        WindowBounds, WindowOptions, div, prelude::*, px, size,
    };

    use super::{TestSupportExt, KeleuoSkin};

    struct Probe {
        kind: &'static str,
        id: &'static str,
        focus: FocusHandle,
    }
    impl BasePanel for Probe {
        fn panel_name(&self) -> &'static str {
            self.kind
        }
    }
    impl Panel for Probe {
        fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.id
        }
    }
    impl EventEmitter<PanelEvent> for Probe {}
    impl Focusable for Probe {
        fn focus_handle(&self, _: &App) -> FocusHandle {
            self.focus.clone()
        }
    }
    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().id(self.id).test_support().track_focus(&self.focus).size_full().child(self.id)
        }
    }
    struct Root(Entity<gpui_kit::component::dock::DockArea>);
    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    fn tab(e: &Entity<Probe>) -> (&'static str, usize) {
        ("sheet-tab", e.entity_id().as_u64() as usize)
    }

    #[gpui_kit::test]
    async fn panels_drag_from_bottom_tabs_and_the_grip(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut probes = Vec::new();
        let handle = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds { origin: Default::default(), size: size(px(1000.), px(600.)) })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let (area, _) = KeleuoSkin::dock_area("test", None, window, cx);
                let make = |kind, id, cx: &mut App| cx.new(|cx| Probe { kind, id, focus: cx.focus_handle() });
                let files = make("Files", "files", cx);
                let skills = make("Skills", "skills", cx);
                let doc_a = make("Doc", "doc-a", cx);
                let doc_b = make("Doc", "doc-b", cx);
                probes = vec![files.clone(), skills.clone(), doc_a.clone(), doc_b.clone()];
                let layout = DockLayout::h_split()
                    .child(DockLayout::tabs().panel_view(panel_handle(files), cx).panel_view(panel_handle(skills), cx), Some(px(300.)))
                    .child(DockLayout::tabs().panel_view(panel_handle(doc_a), cx).panel_view(panel_handle(doc_b), cx), None);
                area.update(cx, |a, cx| a.set_center(layout, window, cx));
                cx.new(|_| Root(area))
            })
            .unwrap()
            .0
        });
        let (files, skills, doc_a) = (probes[0].clone(), probes[1].clone(), probes[2].clone());
        let mut right = None;
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("files").visible());
            right = Some(window.find("doc-a").bounds());
            // A click on a bottom tab shows that panel.
            window.click(tab(&skills), cx);
        })
        .unwrap();
        let right = right.unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |w, _| w.try_find("skills").is_some()).await;
        // Drag Skills by its bottom tab onto the documents' top tab.
        cx.update_window(handle, |_, window, cx| window.drag_to(tab(&skills), tab(&doc_a), cx)).unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |w, _| {
            w.try_find("skills").is_some_and(|s| s.bounds().left() >= right.left())
        })
        .await;
        // Files is alone now: drag it by its title bar grip onto the documents' tabs too.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.drag_to(("panel-grip", files.entity_id().as_u64() as usize), tab(&doc_a), cx)
        })
        .unwrap();
        cx.wait_for(handle, Duration::from_secs(1), |w, cx| {
            w.render_frame(cx);
            // Its tab is now in the documents' strip along the top, not along a bottom edge.
            w.try_find(tab(&files)).is_some_and(|t| t.bounds().top() < px(40.))
        })
        .await;
    }

    #[gpui_kit::test]
    async fn the_chat_has_no_tab_and_takes_no_drops(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        gpui_kit::base::dock::pin_panels(&[super::CHAT]);
        let mut probes = Vec::new();
        let handle = cx.update(|cx| {
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(gpui_kit::Bounds { origin: Default::default(), size: size(px(1000.), px(600.)) })),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                let (area, _) = KeleuoSkin::dock_area("test", None, window, cx);
                let make = |kind, id, cx: &mut App| cx.new(|cx| Probe { kind, id, focus: cx.focus_handle() });
                let chat = make(super::CHAT, "chat", cx);
                let doc_a = make("Doc", "doc-a", cx);
                let doc_b = make("Doc", "doc-b", cx);
                probes = vec![chat.clone(), doc_a.clone(), doc_b.clone()];
                let layout = DockLayout::h_split()
                    .child(DockLayout::tabs().panel_view(panel_handle(chat), cx), Some(px(500.)))
                    .child(DockLayout::tabs().panel_view(panel_handle(doc_a), cx).panel_view(panel_handle(doc_b), cx), None);
                area.update(cx, |a, cx| a.set_center(layout, window, cx));
                cx.new(|_| Root(area))
            })
            .unwrap()
            .0
        });
        let (chat, doc_a) = (probes[0].clone(), probes[1].clone());
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            // No tab for the chat; its content starts at the top.
            assert!(window.try_find(tab(&chat)).is_none());
            assert!(window.find("chat").bounds().top() < px(4.));
            // A document dragged onto the chat stays where it was.
            window.drag_to(tab(&doc_a), "chat", cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find(tab(&doc_a)).is_some_and(|t| t.bounds().left() >= px(500.)), "the document stays right of the chat");
        })
        .unwrap();
        gpui_kit::base::dock::pin_panels(&[]);
    }
}
