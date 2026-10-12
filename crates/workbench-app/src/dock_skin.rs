//! The dock's look, after an earlier app's panel headers and sheet tabs, on top of
//! gpui-component's `DockSkin`.
//!
//! gpui-component splits its dock in two: `gpui_base::dock` owns the behaviour (the layout tree,
//! dragging, dropping, saving) and calls a renderer for everything visible. [`WorkbenchSkin`] is
//! that renderer. It hands the parts it does not change (splitters, dock edges, drop
//! placeholders, the zoom and close actions) to the stock `DockSkin`, and draws tab groups
//! itself:
//!
//! - a group of tool panels (Files, Skills, Terminals, Agent) gets a title bar: a grip to drag
//!   it by, the shown panel's title and actions, and its options on hover. With several panels
//!   in one group their tabs run along the bottom as sheet tabs joined to the content above;
//! - any other group (files, terminals, the chat) keeps its tabs on top, as sheet tabs standing
//!   on the content below.
//!
//! Tabs and the grip drag with the dock's own `DragPanel`, and drop through
//! `TabGroupContext::drop_panel`, so docking, splitting and saving work as before.

use std::cell::{Cell, RefCell};
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

use crate::sheet_tabs::{self, GEOMETRY, Joined, Palette};

/// The panels that are tools (lists beside the work) rather than documents.
const TOOLS: [&str; 4] = ["Files", "Skills", "Agent", "Terminals"];

/// A tool group's title bar.
pub const HEADER_HEIGHT: f32 = 26.;
/// A tool group's tabs along the bottom.
pub const BOTTOM_STRIP: f32 = 24.;
/// A document group's tabs along the top.
pub const TOP_STRIP: f32 = 28.;

const PREVIEW: Size<Pixels> = size(px(120.), px(26.));

pub struct WorkbenchSkin {
    inner: Rc<DockSkin>,
}

impl WorkbenchSkin {
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
            DockArea::new(id, version, window, cx).with_renderer(Rc::new(WorkbenchSkin { inner }))
        });
        (area, handle.expect("built inside the constructor"))
    }
}

impl DockAreaRenderer for WorkbenchSkin {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.inner.frame(window, cx)
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
        })
    }
}

/// What one strip remembers between its layout and its paint.
#[derive(Default)]
struct StripState {
    /// Each drawn tab's slot this frame, by panel index.
    slots: RefCell<Vec<(usize, Bounds<Pixels>)>>,
    hot: Cell<Option<usize>>,
}

struct SheetGroup {
    inner: Rc<dyn TabGroupRenderer>,
    strip: Rc<StripState>,
    scroll: ScrollHandle,
}

fn is_tool_group(group: &TabGroupContext, cx: &App) -> bool {
    !group.panels().is_empty() && group.panels().iter().all(|p| TOOLS.contains(&p.panel_name(cx)))
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
    /// A tool group's title bar: grip, title, the shown panel's actions, its options on hover.
    fn header(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> AnyElement {
        let theme = cx.theme();
        let (bar, border, fg, muted) = (theme.tab_bar, theme.border, theme.foreground, theme.muted_foreground);
        let Some(ix) = shown_ix(group, cx) else {
            return div().into_any_element();
        };
        let panel = group.panels()[ix].clone();
        let handle = PanelHandle::of(&panel).map(|h| h.panel());
        let buttons = handle.as_ref().and_then(|h| h.toolbar_buttons(window, cx)).unwrap_or_default();
        let title = title_of(&panel, window, cx);
        let drag = drag_of(group, ix, cx);
        let closable = group.is_closable();
        let close_group = group.clone();
        let menu_panel = handle.clone();
        let handle_el = on_drag_panel(
            div()
                .id("panel-grip")
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
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
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
            .pl(px(6.))
            .pr(px(4.))
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
            .child(handle_el)
            .children(buttons.into_iter().map(|b| b.xsmall().ghost().tab_stop(false)))
            .child(
                div().invisible().group_hover("panel-header", |d| d.visible()).child(
                    Button::new("panel-options")
                        .icon(IconName::Ellipsis)
                        .xsmall()
                        .ghost()
                        .tab_stop(false)
                        .dropdown_menu(move |menu, window, cx| {
                            let close_group = close_group.clone();
                            menu.when_some(menu_panel.clone(), |menu, p| p.dropdown_menu(menu, window, cx))
                                .when(closable, |menu| {
                                    menu.separator().item(
                                        gpui_kit::component::menu::PopupMenuItem::new("Close").on_click(
                                            move |_, window, cx| {
                                                if let Some(p) = close_group.active_panel() {
                                                    let id = p.panel_id(cx);
                                                    close_group.close(id, window, cx);
                                                }
                                            },
                                        ),
                                    )
                                })
                        }),
                ),
            )
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
        let accent = if focused(group, window, cx) { theme.ring } else { sheet_tabs::mix(theme.ring, ground, 0.45) };
        let palette = Palette::new(ground, theme.foreground, content, theme.foreground, theme.border, accent);
        let documents = joined == Joined::Bottom;
        let shown = shown_ix(group, cx);
        let collapsed = group.is_collapsed();
        self.strip.slots.borrow_mut().clear();

        let state = self.strip.clone();
        let backdrop = canvas(
            |_, _, _| (),
            move |b, _, window, _| {
                // The line along the joined edge, then the tabs: the others from the right, the
                // shown one last, over them and the line.
                let y = match joined {
                    Joined::Top => b.origin.y,
                    Joined::Bottom => b.origin.y + b.size.height - px(1.),
                };
                window.paint_quad(fill(Bounds::new(point(b.origin.x, y), size(b.size.width, px(1.))), palette.line));
                let slots = state.slots.borrow().clone();
                let selected: Vec<bool> = slots.iter().map(|(ix, _)| Some(*ix) == shown && !collapsed).collect();
                for i in sheet_tabs::paint_order(&selected) {
                    let (ix, bounds) = slots[i];
                    // Clip to the strip, so tabs scrolled out of it do not paint over neighbours.
                    if bounds.right() < b.left() || bounds.left() > b.right() {
                        continue;
                    }
                    sheet_tabs::paint_tab(bounds, joined, selected[i], state.hot.get() == Some(ix), &palette, window);
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let mut row = div()
            .id("sheet-tabs")
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_x_scroll()
            .track_scroll(&self.scroll)
            .pl(px(4.));
        let tabs_count = group.panels().len();
        for (n, ix) in visible(group, cx).into_iter().enumerate() {
            let panel = group.panels()[ix].clone();
            let is_shown = Some(ix) == shown && !collapsed;
            let title = title_of(&panel, window, cx);
            let drag = drag_of(group, ix, cx);
            let closable = documents && group.is_panel_closable(panel.panel_id(cx), cx);
            let state = self.strip.clone();
            let record = self.strip.clone();
            let (select, close, drop_on, middle) = (group.clone(), group.clone(), group.clone(), group.clone());
            let id = panel.panel_id(cx);
            let pad = GEOMETRY.pad + GEOMETRY.slant;
            let tab = div()
                .id(("sheet-tab", ix))
                .group("sheet-tab")
                .relative()
                .flex_none()
                .h_full()
                .max_w(px(220.))
                .pl(px(pad))
                .pr(px(if closable { pad - 6. } else { pad }))
                .when(n > 0, |d| d.ml(px(-GEOMETRY.overlap)))
                .flex()
                .items_center()
                .gap_1()
                .map(|d| match joined {
                    // Labels sit in the tab's body, away from the recessed free edge.
                    Joined::Top => d.pb(px(2.)),
                    Joined::Bottom => d.pt(px(2.)),
                })
                .text_size(px(if documents { 12. } else { 11. }))
                .text_color(if is_shown { palette.content_text } else { palette.face_text })
                .child(
                    canvas(move |b, _, _| record.slots.borrow_mut().push((ix, b)), |_, _, _, _| {})
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                )
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
                                close.close(id, window, cx);
                            }),
                    )
                })
                .on_hover(move |hovered, window, _| {
                    let now = if *hovered { Some(ix) } else if state.hot.get() == Some(ix) { None } else { state.hot.get() };
                    if now != state.hot.get() {
                        state.hot.set(now);
                        window.refresh();
                    }
                })
                .on_click(move |_, window, cx| select.select_tab(ix, window, cx))
                .when(closable, |d| {
                    d.on_mouse_down(MouseButton::Middle, move |_, window, cx| {
                        cx.stop_propagation();
                        middle.close(id, window, cx);
                    })
                })
                .when(group.is_droppable() && !collapsed, |d| {
                    d.drag_over::<DragPanel>(|d, _, _, cx| d.border_l_2().border_color(cx.theme().ring))
                        .on_drop(move |drag: &DragPanel, window, cx| {
                            cx.stop_propagation();
                            drop_on.drop_panel(drag.clone(), Some(ix), true, window, cx)
                        })
                });
            row = row.child(on_drag_panel(tab, if collapsed { None } else { drag }, panel));
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
                                                close_group.close(id, window, cx);
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
        let frame = self.inner.frame(group, window, cx);
        if is_tool_group(group, cx) { frame.bg(cx.theme().sidebar) } else { frame }
    }

    fn content_frame(&self, _: &TabGroupContext, _: &mut Window, _: &mut App) -> Stateful<Div> {
        div().id("active-panel")
    }

    fn render_tab_bar(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> AnyElement {
        if visible(group, cx).is_empty() {
            return Empty.into_any_element();
        }
        if is_tool_group(group, cx) {
            return self.header(group, window, cx);
        }
        let theme = cx.theme();
        let (ground, content) = (theme.tab_bar, theme.background);
        let trailing = self.doc_actions(group, window, cx);
        self.strip(group, Joined::Bottom, TOP_STRIP, ground, content, trailing, window, cx)
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
        let (ground, content) = (theme.tab_bar, theme.sidebar);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(view)
            .child(self.strip(group, Joined::Top, BOTTOM_STRIP, ground, content, None, window, cx))
            .into_any_element()
    }

    fn render_drop_indicator(&self, indicator: DropIndicator, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        self.inner.render_drop_indicator(indicator, window, cx)
    }

    fn render_empty(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        self.inner.render_empty(group, window, cx)
    }
}
