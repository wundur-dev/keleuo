//! Tool calls in the chat: their cards, the diffs of edits, permission prompts, and the
//! notice and fix when a command fails.

use super::*;
use wundur_core::questions::Question;


/// How many lines of a diff the chat shows; and with Show all, the most it draws (the rest is
/// in the diff view).
const DIFF_SHOWN: usize = 40;
const DIFF_ALL: usize = 400;

impl ChatPanel {
    /// The line on a tool card that failed in a known way.
    pub(super) fn render_issue_notice(&self, tool_id: &str, issue: &Issue, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, warning, green) = (theme.foreground, theme.muted_foreground, theme.border, theme.warning, theme.green);
        let fixed = self.fixed.contains(&issue.kind);
        let open = issue.clone();
        // A write the sandbox refused is allowed, not fixed.
        let refused = issue.kind == crate::agent_session::SANDBOX_ISSUE;
        let (ask, done) = if refused { ("Allow…", "Allowed") } else { ("Fix so it doesn't happen again", "Fixed") };
        div()
            .px_2()
            .py_1()
            .border_t_1()
            .border_color(border)
            .bg(warning.opacity(0.08))
            .flex()
            .items_center()
            .gap_2()
            .text_xs()
            .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(warning))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .gap_1()
                    .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(issue.title.clone()))
                    .child(div().min_w_0().truncate().text_color(muted).child(issue.detail.replace('`', ""))),
            )
            .child(if fixed {
                div().flex_none().text_color(green).child(done).into_any_element()
            } else {
                Button::new(SharedString::from(format!("fix-{tool_id}")))
                    .xsmall()
                    .label(ask)
                    .on_click(cx.listener(move |p, _, _, cx| {
                        cx.stop_propagation();
                        p.fixing = Some(open.clone());
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .into_any_element()
    }

    /// The fixes for a known failure, each saying exactly what it writes where.
    pub(super) fn render_fix(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let issue = self.fixing.as_ref()?;
        let theme = cx.theme();
        let (fg, muted, border, bg) = (theme.foreground, theme.muted_foreground, theme.border, theme.background);
        let mut list = div().flex().flex_col().gap_2();
        for fix in &issue.fixes {
            let (kind, id) = (issue.kind.clone(), fix.id.clone());
            list = list.child(
                div()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(fix.label.clone()))
                            .child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(fix.preview.clone())),
                    )
                    .child(
                        div().flex_none().child(
                            Button::new(SharedString::from(format!("apply-{id}")))
                                .small()
                                .primary()
                                .label("Apply")
                                .on_click(cx.listener(move |p, _, window, cx| {
                                    let result = p.session.update(cx, |s, cx| s.apply_fix(&kind, &id, cx));
                                    match result {
                                        Ok(done) => {
                                            p.fixed.insert(kind.clone());
                                            p.fixing = None;
                                            gpui_kit::component::WindowExt::push_notification(window, done, cx);
                                        }
                                        Err(e) => gpui_kit::component::WindowExt::push_notification(window, format!("Not fixed: {e}"), cx),
                                    }
                                    p.list.remeasure();
                                    cx.notify();
                                })),
                        ),
                    ),
            );
        }
        Some(
            div()
                .id("fix-dialog")
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_center()
                .pt(px(60.))
                .bg(gpui_kit::black().opacity(0.25))
                .occlude()
                .capture_key_down(cx.listener(|p, ev: &KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        p.fixing = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .w(px(520.))
                        .max_w(relative(0.92))
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .rounded_lg()
                        .border_1()
                        .border_color(border)
                        .bg(bg)
                        .shadow_lg()
                        .child(div().text_base().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(issue.title.clone()))
                        .child(div().text_sm().line_height(rems(1.45)).text_color(muted).child(issue.detail.replace('`', "")))
                        .child(div().text_xs().text_color(muted).child("Choose a fix. It goes into the agent's own settings, so it works outside Wundur Studio too."))
                        .child(list)
                        .child(
                            div().flex().justify_end().child(
                                Button::new("fix-cancel").small().ghost().label("Not now").on_click(cx.listener(|p, _, _, cx| {
                                    p.fixing = None;
                                    cx.notify();
                                })),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// A diff in the chat: its first lines, with Show all for the rest. Each line is drawn
    /// (and its click handled) on every frame the card is on screen, so a big new file (one
    /// long green block) shown whole made the chat slow while it streamed.
    pub(super) fn render_diff(&self, key: &str, diff: &FileDiff, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (green, red, muted, border) =
            (theme.green, theme.red, theme.muted_foreground, theme.border);
        let mut col = div()
            .flex()
            .flex_col()
            .font_family(crate::fonts::code(cx))
            .text_size(px(11.5))
            .line_height(px(16.));
        let total: usize = diff.hunks.iter().map(|h| h.lines.len()).sum();
        let all_key = format!("{key}:all");
        let all = self.toggled.contains(&all_key);
        let max_lines = if all { DIFF_ALL } else { DIFF_SHOWN };
        let mut shown = 0;
        for hunk in &diff.hunks {
            if shown >= max_lines {
                break;
            }
            if diff.hunks.len() > 1 || hunk.old_start > 0 {
                col = col.child(
                    div()
                        .px_1()
                        .text_color(muted)
                        .bg(border.opacity(0.4))
                        .child(format!("@@ -{} +{} @@", hunk.old_start, hunk.new_start)),
                );
            }
            let mut new_line = hunk.new_start.max(1);
            for line in &hunk.lines {
                shown += 1;
                if shown > max_lines {
                    break;
                }
                let (sign, bg, fg) = match line.kind {
                    LineKind::Added => ("+", green.opacity(0.16), green),
                    LineKind::Removed => ("-", red.opacity(0.16), red),
                    LineKind::Context => (" ", transparent_black(), muted),
                };
                let number = if line.kind == LineKind::Removed {
                    String::new()
                } else {
                    new_line.to_string()
                };
                if line.kind != LineKind::Removed {
                    new_line += 1;
                }
                let path = diff.path.clone();
                let target_line = number.parse::<u32>().ok();
                col = col.child(
                    div()
                        .id(SharedString::from(format!("{key}-{shown}")))
                        .flex()
                        .bg(bg)
                        .cursor_pointer()
                        .on_click(
                            cx.listener(move |p, _, _, cx| p.open(path.clone(), target_line, cx)),
                        )
                        .child(
                            div()
                                .w(px(32.))
                                .flex_none()
                                .text_right()
                                .pr_1()
                                .text_color(muted.opacity(0.7))
                                .child(number),
                        )
                        .child(div().w(px(12.)).flex_none().text_color(fg).child(sign))
                        .child(
                            div()
                                .flex_1()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .child(line.text.clone()),
                        ),
                );
            }
        }
        // The rest: Show all (up to DIFF_ALL), and past that the diff view.
        let hidden = total.saturating_sub(max_lines);
        if hidden > 0 || all {
            let (fg, hover) = (cx.theme().foreground, cx.theme().secondary_hover);
            let label = match (all, hidden) {
                (false, _) => format!("Show all {total} lines"),
                (true, 0) => "Show fewer".to_string(),
                (true, n) => format!("Show fewer · {n} more in the diff view"),
            };
            col = col.child(
                div()
                    .id(SharedString::from(format!("{key}-more")))
                    .px_1()
                    .py(px(3.))
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(move |d| d.bg(hover).text_color(fg))
                    .child(label)
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if !p.toggled.remove(&all_key) {
                            p.toggled.insert(all_key.clone());
                        }
                        p.list.remeasure();
                        cx.notify();
                    })),
            );
        }
        col.into_any_element()
    }

    /// The header of a run of tool calls (`run`, items of the chat shown): how many of each,
    /// the latest one, whether one runs, and how long they took. A click opens or closes
    /// the group (its calls are entries of their own below it).
    pub(super) fn render_tool_group(&self, run: std::ops::Range<usize>, key: String, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, card_bg, hover, green) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.secondary, theme.secondary_hover, theme.green);
        let tools: Vec<ToolCall> = {
            let chat = &self.session.read(cx).chat;
            let items = match &self.agent {
                Some(id) => chat.agent(id).map(|a| a.items.as_slice()).unwrap_or_default(),
                None => chat.items.as_slice(),
            };
            items
                .get(run)
                .unwrap_or_default()
                .iter()
                .filter_map(|i| if let Item::Tool(t) = i { Some(t.clone()) } else { None })
                .collect()
        };
        let Some(latest) = tools.last() else { return div().into_any_element() };
        let running = tools.iter().any(ToolCall::running);
        // Failed calls stay out of groups (see `foldable`), so a group either runs or is done.
        let status = if running {
            self.spinner(muted).xsmall().into_any_element()
        } else {
            Icon::new(IconName::Check).xsmall().text_color(green).into_any_element()
        };
        let total: std::time::Duration = tools.iter().filter_map(ToolCall::elapsed).sum();
        let elapsed = (running || total.as_secs() >= 1).then(|| format_elapsed(total));
        // The one running, else the latest.
        let shown = tools.iter().find(|t| t.running()).unwrap_or(latest);
        div()
            .id(SharedString::from(format!("tool-group-{key}")))
            .h(px(24.))
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(card_bg.opacity(0.5))
            .cursor_pointer()
            .hover(|d| d.bg(hover))
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(muted))
            .child(status)
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child(wundur_core::conversation::tool_run_label(tools.iter().map(|t| t.name.as_str()))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::fonts::code(cx))
                    .text_color(muted)
                    .child(shown.summary.lines().next().unwrap_or_default().to_string()),
            )
            .when_some(elapsed, |d, e| d.child(div().flex_none().text_color(muted).child(e)))
            .on_click(cx.listener(move |p, _, _, cx| {
                if !p.toggled.remove(&key) {
                    p.toggled.insert(key.clone());
                }
                p.list.remeasure();
                cx.notify();
            }))
            .into_any_element()
    }

    pub(super) fn render_tool(&self, ix: usize, tool: &ToolCall, cx: &mut Context<Self>) -> AnyElement {
        // A tool call that started a subagent shows as the subagent's card.
        if let Some(agent) = self.session.read(cx).chat.agent(&tool.id).cloned()
        {
            return self.render_agent(&agent, true, cx);
        }
        let theme = cx.theme();
        let (fg, muted, border, card_bg, hover) = (
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.secondary,
            theme.secondary_hover,
        );
        let (green, red, page_bg) = (theme.green, theme.red, theme.background);
        // Diffs start open; other cards start closed.
        let default_open = tool.diff.is_some();
        let open = default_open != self.toggled.contains(&tool.id);
        let id = tool.id.clone();
        let status = if tool.running() {
            self.spinner(muted).xsmall().into_any_element()
        } else if !tool.done() {
            // Interrupted, or replayed without its result.
            Icon::new(IconName::Minus).xsmall().text_color(muted).into_any_element()
        } else if tool.is_error {
            Icon::new(IconName::CircleX).xsmall().text_color(red).into_any_element()
        } else {
            Icon::new(IconName::Check).xsmall().text_color(green).into_any_element()
        };
        // Live while it runs; afterwards how long it took, when that is worth saying.
        let elapsed = tool
            .elapsed()
            .filter(|d| tool.running() || d.as_secs() >= 1)
            .map(format_elapsed);
        let stats = tool.diff.as_ref().map(|d| {
            let path = d.path.clone();
            div()
                .flex()
                .gap_1()
                .flex_none()
                .child(div().text_color(green).child(format!("+{}", d.added())))
                .child(div().text_color(red).child(format!("-{}", d.removed())))
                // The file as a diff of everything the session changed in it.
                .when(tool.done() && !tool.is_error, |d| {
                    d.child(
                        div()
                            .id(SharedString::from(format!("open-diff-{}", tool.id)))
                            .px_1()
                            .rounded_sm()
                            .text_color(muted)
                            .hover(|d| d.bg(hover.opacity(0.6)).text_color(fg))
                            .child("Open diff")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(ChatPanelEvent::OpenDiff(path.clone()));
                            })),
                    )
                })
        });
        let summary_el = self.linked_text(
            SharedString::from(format!("sum-{}", tool.id)),
            &tool.summary,
            muted,
            cx,
        );
        let header = div()
            .id(SharedString::from(format!("tool-h-{}", tool.id)))
            .h(px(22.))
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .cursor_pointer()
            .hover(|d| d.bg(hover))
            .child(
                Icon::new(if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(muted),
            )
            .child(status)
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child(tool.name.clone()),
            )
            .when(tool.parent.is_some(), |d| {
                d.child(div().text_color(muted).child("(subagent)"))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(crate::fonts::code(cx))
                    .child(summary_el),
            )
            .children(stats)
            .when_some(elapsed, |d, e| d.child(div().flex_none().text_color(muted).child(e)))
            .on_click(cx.listener(move |p, _, _, cx| {
                if !p.toggled.remove(&id) {
                    p.toggled.insert(id.clone());
                }
                p.list.remeasure();
                cx.notify();
            }));
        let mut card = div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(card_bg.opacity(0.5))
            .overflow_hidden()
            .child(header);
        // Images it returned (a screenshot it looked at): thumbnails, open or not; a click shows
        // one full size.
        if !tool.images.is_empty() {
            let mut row = div().px_2().py(px(6.)).flex().flex_wrap().gap_2();
            for (n, path) in tool.images.iter().enumerate() {
                let p = path.clone();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("tool-img-{}-{n}", tool.id)))
                        .w(px(160.))
                        .h(px(110.))
                        .rounded(px(8.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(border)
                        .cursor_pointer()
                        .child(img(path.clone()).size_full().object_fit(ObjectFit::Contain))
                        .on_click(move |_, _, cx| crate::image_view::show(p.clone(), cx)),
                );
            }
            card = card.child(row);
        }
        // A known needless failure: what happened, and a way to stop it happening again.
        if let Some(issue) = &tool.issue {
            card = card.child(self.render_issue_notice(&tool.id, issue, cx));
        }
        // While it runs, its latest output, newest line at the bottom, whether the card is
        // open or not.
        if tool.running()
            && let Some(live) = tool.live.as_deref().filter(|l| !l.trim().is_empty())
        {
            let lines: Vec<&str> = live.trim_end_matches('\n').lines().collect();
            let tail = lines[lines.len().saturating_sub(LIVE_LINES)..].join("\n");
            card = card.child(
                div()
                    .px_2()
                    .py_1()
                    .border_t_1()
                    .border_color(border)
                    .bg(page_bg.opacity(0.6))
                    .font_family(crate::fonts::code(cx))
                    .text_size(px(11.))
                    .line_height(px(15.))
                    .text_color(muted)
                    .max_h(px(15. * LIVE_LINES as f32 + 8.))
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .child(tail),
            );
        }
        if open {
            let mut body = div().flex().flex_col().border_t_1().border_color(border);
            if let Some(diff) = &tool.diff {
                body = body.child(self.render_diff(&format!("d{ix}"), diff, cx));
            } else {
                let input = serde_json::to_string_pretty(&tool.input).unwrap_or_default();
                body = body.child(
                    div()
                        .px_2()
                        .py_1()
                        .font_family(crate::fonts::code(cx))
                        .text_size(px(11.))
                        .text_color(muted)
                        .max_h(px(160.))
                        .overflow_hidden()
                        .child(input),
                );
            }
            if let Some(result) = &tool.result
                && tool.diff.is_none()
            {
                let text: String = result.lines().take(60).collect::<Vec<_>>().join("\n");
                let more = result.lines().count().saturating_sub(60);
                body = body.child(
                    div()
                        .px_2()
                        .py_1()
                        .border_t_1()
                        .border_color(border)
                        .font_family(crate::fonts::code(cx))
                        .text_size(px(11.))
                        .text_color(if tool.is_error { red } else { fg })
                        .child(self.linked_text(
                            SharedString::from(format!("res-{}", tool.id)),
                            &text,
                            fg,
                            cx,
                        ))
                        .when(more > 0, |d| {
                            d.child(
                                div()
                                    .text_color(muted)
                                    .child(format!("... {more} more lines")),
                            )
                        }),
                );
            } else if let (Some(result), true) = (&tool.result, tool.is_error) {
                body = body.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(red)
                        .child(one_line(result, 300)),
                );
            }
            card = card.child(body);
        }
        card.into_any_element()
    }

    pub(super) fn render_permission(&self, p: &PermissionPrompt, cx: &mut Context<Self>) -> AnyElement {
        // A question for the person comes as a request to run the asking tool: shown as the
        // question, answered by choosing.
        if let Some(questions) = wundur_core::questions::parse(&p.tool_name, &p.input) {
            return self.render_question(p, std::rc::Rc::new(questions), cx);
        }
        let theme = cx.theme();
        let (warning, muted, fg, border) = (
            theme.warning,
            theme.muted_foreground,
            theme.foreground,
            theme.border,
        );
        let mut card = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(if p.answer.is_none() { warning } else { border })
            .text_xs()
            .child(
                div()
                    .flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .xsmall()
                            .text_color(warning),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(fg)
                            .child(format!("Allow {}?", p.tool_name)),
                    ),
            )
            .child(
                div()
                    .font_family(crate::fonts::code(cx))
                    .text_color(fg)
                    .child(p.summary.clone()),
            );
        if let Some(d) = &p.description
            && d != &p.summary
        {
            card = card.child(div().text_color(muted).child(d.clone()));
        }
        match p.answer {
            None => {
                let (rid1, rid2) = (p.request_id.clone(), p.request_id.clone());
                card =
                    card.child(
                        div()
                            .flex()
                            .gap_2()
                            .pt_1()
                            .child(
                                gpui_kit::component::button::Button::new(SharedString::from(
                                    format!("allow-{}", p.request_id),
                                ))
                                .xsmall()
                                .label("Allow")
                                .on_click(cx.listener(
                                    move |panel, _, _, cx| {
                                        panel.session.update(cx, |s, cx| s.answer(&rid1, true, cx))
                                    },
                                )),
                            )
                            .child(
                                gpui_kit::component::button::Button::new(SharedString::from(
                                    format!("deny-{}", p.request_id),
                                ))
                                .xsmall()
                                .label("Deny")
                                .on_click(cx.listener(
                                    move |panel, _, _, cx| {
                                        panel.session.update(cx, |s, cx| s.answer(&rid2, false, cx))
                                    },
                                )),
                            ),
                    );
            }
            Some(allowed) => {
                card = card.child(div().text_color(muted).child(if allowed {
                    "Allowed"
                } else {
                    "Denied"
                }));
            }
        }
        card.into_any_element()
    }
}

impl ChatPanel {
    /// A question the agent asks: each with its choices as buttons. One question with one
    /// answer goes at the click; otherwise Submit once each has a choice. Skip declines.
    /// A question card: the agent's questions one at a time ("Question 2 of 4", a bar of how
    /// far along), each with its choices as large rows, their descriptions shown. Choosing
    /// moves on to the next question; Back and Next go between them, and the last one is
    /// submitted with all the answers. One question with one answer goes with a click.
    fn render_question(&self, p: &PermissionPrompt, qs: std::rc::Rc<Vec<Question>>, cx: &mut Context<Self>) -> AnyElement {
        use gpui_kit::component::button::{Button, ButtonVariants as _};
        use gpui_kit::component::Disableable as _;
        let theme = cx.theme();
        let (ring, muted, fg, border, hover) = (theme.ring, theme.muted_foreground, theme.foreground, theme.border, theme.secondary_hover);
        let open = p.answer.is_none();
        let picks = self.picks.get(&p.request_id).cloned().unwrap_or_else(|| vec![Vec::new(); qs.len()]);
        let name = self.session.read(cx).backend.name();
        let count = qs.len();
        let step = self.q_step.get(&p.request_id).copied().unwrap_or(0).min(count.saturating_sub(1));
        let mut card = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(if open { ring } else { border })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(gpui_kit::assets::IconName::MessageCircleQuestionMark).small().text_color(ring))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(format!("{name} asks")))
                    .when(count > 1 && open, |d| d.child(div().text_sm().text_color(muted).child(format!("· Question {} of {count}", step + 1)))),
            );
        // Answered or skipped: each question with what was chosen.
        if !open {
            let said = match (&p.reply, p.answer) {
                (Some(r), _) => {
                    let answers: Vec<&str> = r.split("; ").collect();
                    let mut list = div().flex().flex_col().gap_1();
                    for (i, q) in qs.iter().enumerate() {
                        list = list.child(
                            div()
                                .text_sm()
                                .child(div().text_color(muted).child(if q.header.is_empty() { q.question.clone() } else { q.header.clone() }))
                                .child(div().text_color(fg).child(answers.get(i).copied().unwrap_or("").to_string())),
                        );
                    }
                    return card.child(list).into_any_element();
                }
                (None, Some(true)) => "Answered in the terminal".to_string(),
                _ => "Skipped".to_string(),
            };
            return card.child(div().text_sm().text_color(muted).child(said)).into_any_element();
        }
        // How far along: a segment per question, filled once answered, the shown one marked.
        if count > 1 {
            let mut bar = div().flex().gap_1();
            for i in 0..count {
                let done = picks.get(i).is_some_and(|p| !p.is_empty());
                let rid = p.request_id.clone();
                bar = bar.child(
                    div()
                        .id(SharedString::from(format!("q-seg-{}-{i}", p.request_id)))
                        .flex_1()
                        .h(px(4.))
                        .rounded_full()
                        .cursor_pointer()
                        // Answered: the accent, a little lighter than the one shown; not yet: the border's grey.
                        .bg(if i == step { ring } else if done { ring.opacity(0.7) } else { border.opacity(0.6) })
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel.q_step.insert(rid.clone(), i);
                            cx.notify();
                        })),
                );
            }
            card = card.child(bar);
        }
        let q = &qs[step];
        let mut options = div().flex().flex_col().gap(px(6.));
        for (label, description) in &q.options {
            let on = picks.get(step).is_some_and(|p| p.contains(label));
            let (rid, qs2, label2) = (p.request_id.clone(), qs.clone(), label.clone());
            // A radio for one answer, a box for several.
            let mark = div()
                .flex_none()
                .size(px(16.))
                .mt(px(2.))
                .flex()
                .items_center()
                .justify_center()
                .border_1()
                .border_color(if on { ring } else { muted.opacity(0.6) })
                .when(!q.multi, |d| d.rounded_full())
                .when(q.multi, |d| d.rounded(px(4.)))
                .when(on, |d| {
                    d.bg(ring).child(if q.multi {
                        Icon::new(IconName::Check).xsmall().text_color(gpui_kit::white()).into_any_element()
                    } else {
                        div().size(px(6.)).rounded_full().bg(gpui_kit::white()).into_any_element()
                    })
                });
            options = options.child(
                div()
                    .id(SharedString::from(format!("q-{}-{step}-{label}", p.request_id)))
                    .px_3()
                    .py(px(10.))
                    .flex()
                    .gap_3()
                    .rounded_md()
                    .border_1()
                    .border_color(if on { ring } else { border })
                    .when(on, |d| d.bg(ring.opacity(0.10)))
                    .when(!on, |d| d.hover(|d| d.bg(hover)))
                    .cursor_pointer()
                    .child(mark)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_sm().font_weight(FontWeight::MEDIUM).text_color(fg).child(label.clone()))
                            .when(!description.is_empty(), |d| d.child(div().text_xs().line_height(rems(1.4)).text_color(muted).child(description.clone()))),
                    )
                    .on_click(cx.listener(move |panel, _, _, cx| panel.pick(&rid, &qs2, step, &label2, cx))),
            );
        }
        card = card.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .when(!q.header.is_empty(), |d| d.child(div().text_xs().font_weight(FontWeight::SEMIBOLD).text_color(muted).child(q.header.to_uppercase())))
                .child(div().text_base().line_height(rems(1.45)).text_color(fg).child(q.question.clone()))
                .when(q.multi, |d| d.child(div().text_xs().text_color(muted).child("Choose any that apply.")))
                .child(options),
        );
        // Back, Next or Submit, and Skip.
        let one_click = count == 1 && !q.multi;
        let ready = picks.iter().all(|p| !p.is_empty());
        let last = step + 1 == count;
        let this_done = picks.get(step).is_some_and(|p| !p.is_empty());
        let (rid_back, rid_next, rid_submit, rid_skip, qs2) =
            (p.request_id.clone(), p.request_id.clone(), p.request_id.clone(), p.request_id.clone(), qs.clone());
        card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Button::new(SharedString::from(format!("q-skip-{}", p.request_id)))
                        .small()
                        .ghost()
                        .label("Skip")
                        .on_click(cx.listener(move |panel, _, _, cx| {
                            panel.picks.remove(&rid_skip);
                            panel.q_step.remove(&rid_skip);
                            panel.session.update(cx, |s, cx| s.answer(&rid_skip, false, cx));
                        })),
                )
                .child(div().flex_1())
                .when(step > 0, |d| {
                    d.child(Button::new(SharedString::from(format!("q-back-{}", p.request_id))).small().label("Back").on_click(cx.listener(move |panel, _, _, cx| {
                        let at = panel.q_step.entry(rid_back.clone()).or_insert(0);
                        *at = at.saturating_sub(1);
                        cx.notify();
                    })))
                })
                .when(!last, |d| {
                    d.child(
                        Button::new(SharedString::from(format!("q-next-{}", p.request_id)))
                            .small()
                            .label("Next")
                            .disabled(!this_done)
                            .on_click(cx.listener(move |panel, _, _, cx| {
                                let at = panel.q_step.entry(rid_next.clone()).or_insert(0);
                                *at += 1;
                                cx.notify();
                            })),
                    )
                })
                .when(last && !one_click, |d| {
                    d.child(
                        Button::new(SharedString::from(format!("q-submit-{}", p.request_id)))
                            .small()
                            .primary()
                            .label(if count > 1 { "Submit answers" } else { "Submit" })
                            .disabled(!ready)
                            .on_click(cx.listener(move |panel, _, _, cx| panel.submit_question(&rid_submit, &qs2, cx))),
                    )
                }),
        )
        .into_any_element()
    }

    /// A choice on a question card; one question with one answer goes at once.
    fn pick(&mut self, rid: &str, qs: &[Question], i: usize, label: &str, cx: &mut Context<Self>) {
        let picks = self.picks.entry(rid.to_string()).or_insert_with(|| vec![Vec::new(); qs.len()]);
        let Some(chosen) = picks.get_mut(i) else { return };
        if qs[i].multi {
            match chosen.iter().position(|l| l == label) {
                Some(at) => {
                    chosen.remove(at);
                }
                None => chosen.push(label.to_string()),
            }
        } else {
            *chosen = vec![label.to_string()];
        }
        if qs.len() == 1 && !qs[0].multi {
            self.submit_question(rid, qs, cx);
        } else if !qs[i].multi && i + 1 < qs.len() {
            // One answer chosen: on to the next question.
            self.q_step.insert(rid.to_string(), i + 1);
        }
        cx.notify();
    }

    fn submit_question(&mut self, rid: &str, qs: &[Question], cx: &mut Context<Self>) {
        let Some(picks) = self.picks.remove(rid) else { return };
        self.q_step.remove(rid);
        let answers = qs.iter().zip(picks).map(|(q, p)| (q.question.clone(), p)).collect();
        self.session.update(cx, |s, cx| s.answer_question(rid, answers, cx));
    }
}

/// How many of a running command's latest output lines its card shows.
pub(super) const LIVE_LINES: usize = 10;
