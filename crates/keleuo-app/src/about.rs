//! The About window: what Keleuo is, what it uses right now (its own memory apart from the
//! agents' interfaces it runs, which are separate programs), and the machine it runs on.
//! One at a time; asking again brings it to the front.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::procstats::{self, Machine, Usage};

/// Readings kept for the memory chart (one a second).
const HISTORY: usize = 60;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    About,
    Memory,
    System,
}

/// The open About window, if any.
#[derive(Default)]
struct Open(Option<AnyWindowHandle>);
impl Global for Open {}

pub struct About {
    tab: Tab,
    icon: Arc<Image>,
    machine: Option<Machine>,
    /// Readings, oldest first, with when each was taken.
    samples: VecDeque<(Instant, Usage)>,
    _poll: Task<()>,
}

/// Show the About window (or bring the open one forward).
pub fn open(cx: &mut App) {
    if let Some(handle) = cx.try_global::<Open>().and_then(|o| o.0)
        && handle.update(cx, |_, window, _| window.activate_window()).is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(560.), px(500.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some("About Keleuo".into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(12.), px(12.))),
        }),
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    };
    match gpui_kit::open_window(options, cx, |_, cx| cx.new(About::new)) {
        Ok((handle, _)) => cx.set_global(Open(Some(handle))),
        Err(e) => eprintln!("keleuo: could not open the About window: {e}"),
    }
}

impl About {
    fn new(cx: &mut Context<Self>) -> Self {
        // Read off the UI thread, once a second, while the window is open.
        let poll = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let machine = cx.background_spawn(async { procstats::machine() }).await;
            if this.update(cx, |a, cx| {
                a.machine = Some(machine);
                cx.notify();
            })
            .is_err()
            {
                return;
            }
            loop {
                let usage = cx.background_spawn(async { procstats::usage() }).await;
                let alive = this.update(cx, |a, cx| {
                    a.samples.push_back((Instant::now(), usage));
                    while a.samples.len() > HISTORY {
                        a.samples.pop_front();
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(1)).await;
            }
        });
        Self {
            tab: Tab::About,
            icon: Arc::new(Image::from_bytes(ImageFormat::Png, crate::app_icon::PNG.to_vec())),
            machine: None,
            samples: VecDeque::new(),
            _poll: poll,
        }
    }

    fn latest(&self) -> Option<Usage> {
        self.samples.back().map(|(_, u)| *u)
    }

    /// CPU use over the last few readings, as a percentage of one core.
    fn cpu_percent(&self) -> Option<f64> {
        let (t0, a) = self.samples.iter().rev().nth(3).or_else(|| self.samples.front())?;
        let (t1, b) = self.samples.back()?;
        let wall = t1.duration_since(*t0).as_nanos() as f64;
        (wall > 0.0).then(|| b.cpu_ns.saturating_sub(a.cpu_ns) as f64 / wall * 100.0)
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, muted, hover, selected, border) =
            (theme.foreground, theme.muted_foreground, theme.secondary_hover, theme.secondary, theme.border);
        let mut row = div().flex().items_center().gap(px(2.)).p(px(2.)).rounded_md().border_1().border_color(border);
        for (tab, label, icon) in [
            (Tab::About, "About", IconName::Info),
            (Tab::Memory, "Memory", IconName::ChartPie),
            (Tab::System, "System", IconName::Settings),
        ] {
            let on = self.tab == tab;
            row = row.child(
                div()
                    .id(SharedString::from(format!("about-tab-{label}")))
                    .h(px(24.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded_sm()
                    .text_sm()
                    .text_color(if on { fg } else { muted })
                    .when(on, |d| d.bg(selected))
                    .when(!on, |d| d.hover(|d| d.bg(hover)))
                    .child(Icon::new(icon).xsmall().text_color(if on { fg } else { muted }))
                    .child(label)
                    .on_click(cx.listener(move |a, _, _, cx| {
                        a.tab = tab;
                        cx.notify();
                    })),
            );
        }
        row
    }

    fn render_about(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .pt_2()
            .child(img(self.icon.clone()).size(px(128.)))
            .child(div().text_size(px(28.)).font_weight(FontWeight::SEMIBOLD).text_color(fg).child("Keleuo"))
            .child(div().text_sm().text_color(muted).child(format!("Version {}", env!("CARGO_PKG_VERSION"))))
            .child(
                div()
                    .pt_2()
                    .text_sm()
                    .text_color(fg)
                    .child("From the Greek κελεύω (keleuō): to command, to urge."),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_center()
                    .text_sm()
                    .line_height(rems(1.45))
                    .text_color(muted)
                    .child("A native window for your coding agent: its chat and its own terminal side by side, its subagents live, and what it changed as diffs."),
            )
            .child(
                div()
                    .pt_3()
                    .text_xs()
                    .text_color(muted)
                    .child("Built with GPUI and gpui-component. Drives Claude Code."),
            )
            .into_any_element()
    }

    fn render_memory(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, accent, agents_color) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.ring, theme.magenta);
        let Some(now) = self.latest() else {
            return div().text_sm().text_color(muted).child("Measuring…").into_any_element();
        };
        let peak = self.samples.iter().map(|(_, u)| u.footprint).max().unwrap_or(now.footprint);
        // The chart: Keleuo's own memory over the last minute.
        let top = (peak as f64 * 1.15).max(1.0);
        let mut bars = div().h(px(90.)).w_full().flex().items_end().gap(px(2.));
        for _ in self.samples.len()..HISTORY {
            bars = bars.child(div().flex_1().h(px(1.)));
        }
        for (_, u) in &self.samples {
            let h = (u.footprint as f64 / top * 90.0).max(2.0);
            bars = bars.child(div().flex_1().h(px(h as f32)).rounded_t(px(2.)).bg(accent.opacity(0.75)));
        }
        let big = |label: &'static str, value: String, note: String, color: Hsla| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap(px(2.))
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .child(div().flex().items_center().gap_2().child(div().size(px(8.)).rounded_full().bg(color)).child(div().text_xs().text_color(muted).child(label)))
                .child(div().text_size(px(26.)).font_weight(FontWeight::SEMIBOLD).text_color(fg).child(value))
                .child(div().text_xs().text_color(muted).child(note))
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(big("Keleuo", procstats::bytes(now.footprint), format!("Peak this minute {}", procstats::bytes(peak)), accent))
                    .child(big(
                        "Agents and their tools",
                        procstats::bytes(now.children_footprint),
                        format!("{} process{}, separate programs", now.children, if now.children == 1 { "" } else { "es" }),
                        agents_color,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .child(div().text_xs().text_color(muted).child("Keleuo's memory, the last minute"))
                    .child(bars),
            )
            .child(
                div()
                    .text_xs()
                    .line_height(rems(1.4))
                    .text_color(muted)
                    .child("Memory is the physical footprint, the figure Activity Monitor shows. The agents' interfaces (Claude Code is a Node.js program) and the commands they run are counted apart: they would use the same in a terminal."),
            )
            .into_any_element()
    }

    fn render_system(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border) = (theme.foreground, theme.muted_foreground, theme.border);
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        if let Some(m) = &self.machine {
            rows.push(("System", m.os.clone()));
            if !m.chip.is_empty() {
                rows.push(("Chip", m.chip.clone()));
            }
            rows.push(("Cores", m.cores.to_string()));
            rows.push(("Memory", procstats::bytes(m.memory)));
        }
        rows.push(("Display scale", format!("{}×", window.scale_factor())));
        rows.push(("Rendering", "GPU (Metal), native".into()));
        if let Some(start) = crate::START.get() {
            rows.push(("Keleuo open for", format_uptime(start.elapsed())));
        }
        if let Some(u) = self.latest() {
            rows.push(("Threads", u.threads.to_string()));
        }
        if let Some(cpu) = self.cpu_percent() {
            rows.push(("CPU (Keleuo)", format!("{cpu:.1}% of one core")));
        }
        let mut list = div().flex().flex_col().rounded_lg().border_1().border_color(border).overflow_hidden();
        let n = rows.len();
        for (i, (label, value)) in rows.into_iter().enumerate() {
            list = list.child(
                div()
                    .h(px(30.))
                    .px_3()
                    .flex()
                    .items_center()
                    .text_sm()
                    .when(i + 1 < n, |d| d.border_b_1().border_color(border))
                    .child(div().w(px(150.)).flex_none().text_color(muted).child(label))
                    .child(div().flex_1().min_w_0().truncate().text_color(fg).child(value)),
            );
        }
        list.into_any_element()
    }
}

fn format_uptime(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

impl Render for About {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = cx.theme().background;
        let fg = cx.theme().foreground;
        let body = match self.tab {
            Tab::About => self.render_about(cx),
            Tab::Memory => self.render_memory(cx),
            Tab::System => self.render_system(window, cx),
        };
        div()
            .size_full()
            .bg(bg)
            .text_color(fg)
            .flex()
            .flex_col()
            // Room for the window buttons, then the tabs in the middle.
            .pt(px(40.))
            .px_6()
            .pb_6()
            .gap_4()
            .child(div().flex().justify_center().child(self.render_tabs(cx)))
            .child(div().flex_1().min_h_0().child(body))
    }
}
