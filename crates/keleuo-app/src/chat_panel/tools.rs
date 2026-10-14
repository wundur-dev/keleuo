//! Tool calls in the chat: their cards, the diffs of edits, permission prompts, and the
//! notice and fix when a command fails.

use super::*;
use keleuo_core::questions::Question;

impl ChatPanel {
    /// The line on a tool card that failed in a known way.
    pub(super) fn render_issue_notice(&self, tool_id: &str, issue: &Issue, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, warning, green) = (theme.foreground, theme.muted_foreground, theme.border, theme.warning, theme.green);
        let fixed = self.fixed.contains(&issue.kind);
        let open = issue.clone();
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
                div().flex_none().text_color(green).child("Fixed").into_any_element()
            } else {
                Button::new(SharedString::from(format!("fix-{tool_id}")))
                    .xsmall()
                    .label("Fix so it doesn't happen again")
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
                                    let result = p.session.read(cx).apply_fix(&kind, &id);
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
                        .child(div().text_xs().text_color(muted).child("Choose a fix. It goes into the agent's own settings, so it works outside Keleuo too."))
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

    pub(super) fn render_diff(&self, key: &str, diff: &FileDiff, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (green, red, muted, border) =
            (theme.green, theme.red, theme.muted_foreground, theme.border);
        let mut col = div()
            .flex()
            .flex_col()
            .font_family(MONO)
            .text_size(px(11.5))
            .line_height(px(16.));
        let max_lines = 400;
        let mut shown = 0;
        for hunk in &diff.hunks {
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
        if shown > max_lines {
            col = col.child(
                div()
                    .px_1()
                    .text_color(muted)
                    .child(format!("... {} more lines", shown - max_lines)),
            );
        }
        col.into_any_element()
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
                    .font_family(MONO)
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
                    .font_family(MONO)
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
                        .font_family(MONO)
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
                        .font_family(MONO)
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
        if let Some(questions) = keleuo_core::questions::parse(&p.tool_name, &p.input) {
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
                    .font_family(MONO)
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
    fn render_question(&self, p: &PermissionPrompt, qs: std::rc::Rc<Vec<Question>>, cx: &mut Context<Self>) -> AnyElement {
        use gpui_kit::component::button::{Button, ButtonVariants as _};
        use gpui_kit::component::{Disableable as _, Selectable as _};
        let theme = cx.theme();
        let (ring, muted, fg, border) = (theme.ring, theme.muted_foreground, theme.foreground, theme.border);
        let open = p.answer.is_none();
        let picks = self.picks.get(&p.request_id).cloned().unwrap_or_else(|| vec![Vec::new(); qs.len()]);
        let name = self.session.read(cx).backend.name();
        let mut card = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(if open { ring } else { border })
            .text_xs()
            .child(
                div()
                    .flex()
                    .gap_1()
                    .items_center()
                    .child(Icon::new(gpui_kit::assets::IconName::MessageCircleQuestionMark).xsmall().text_color(ring))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(format!("{name} asks"))),
            );
        for (i, q) in qs.iter().enumerate() {
            let mut options = div().flex().flex_wrap().gap_1();
            for (label, description) in &q.options {
                let on = picks.get(i).is_some_and(|p| p.contains(label));
                let (rid, qs2, label2) = (p.request_id.clone(), qs.clone(), label.clone());
                let description = description.clone();
                options = options.child(
                    Button::new(SharedString::from(format!("q-{}-{i}-{label}", p.request_id)))
                        .xsmall()
                        .label(label.clone())
                        .selected(on)
                        .when(on, |b| b.primary())
                        .disabled(!open)
                        .when(!description.is_empty(), |b| b.tooltip(description))
                        .on_click(cx.listener(move |panel, _, _, cx| panel.pick(&rid, &qs2, i, &label2, cx))),
                );
            }
            card = card.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(!q.header.is_empty(), |d| d.child(div().text_color(muted).child(q.header.clone())))
                    .child(div().text_sm().text_color(fg).child(q.question.clone()))
                    .when(q.multi, |d| d.child(div().text_color(muted).child("Choose any that apply.")))
                    .child(options),
            );
        }
        if open {
            let one_click = qs.len() == 1 && !qs[0].multi;
            let ready = picks.iter().all(|p| !p.is_empty());
            let (rid1, rid2, qs2) = (p.request_id.clone(), p.request_id.clone(), qs.clone());
            card = card.child(
                div()
                    .flex()
                    .gap_2()
                    .when(!one_click, |d| {
                        d.child(
                            Button::new(SharedString::from(format!("q-submit-{}", p.request_id)))
                                .xsmall()
                                .primary()
                                .label("Submit")
                                .disabled(!ready)
                                .on_click(cx.listener(move |panel, _, _, cx| panel.submit_question(&rid1, &qs2, cx))),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!("q-skip-{}", p.request_id)))
                            .xsmall()
                            .ghost()
                            .label("Skip")
                            .on_click(cx.listener(move |panel, _, _, cx| {
                                panel.picks.remove(&rid2);
                                panel.session.update(cx, |s, cx| s.answer(&rid2, false, cx));
                            })),
                    ),
            );
        } else {
            let said = match (&p.reply, p.answer) {
                (Some(r), _) => format!("Answered: {r}"),
                (None, Some(true)) => "Answered in the terminal".to_string(),
                _ => "Skipped".to_string(),
            };
            card = card.child(div().text_color(muted).child(said));
        }
        card.into_any_element()
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
        }
        cx.notify();
    }

    fn submit_question(&mut self, rid: &str, qs: &[Question], cx: &mut Context<Self>) {
        let Some(picks) = self.picks.remove(rid) else { return };
        let answers = qs.iter().zip(picks).map(|(q, p)| (q.question.clone(), p)).collect();
        self.session.update(cx, |s, cx| s.answer_question(rid, answers, cx));
    }
}

/// How many of a running command's latest output lines its card shows.
pub(super) const LIVE_LINES: usize = 10;
