//! The conversation's items in the chat: the person's messages, the agent's markdown, the
//! end-of-turn card, compaction marks, agents' reports, the thinking and asking lines.

use super::*;

impl ChatPanel {
    /// The divider where the context was compacted, its summary on request.
    /// The person's message, its long pastes folded to a line that opens them.
    pub(super) fn render_user_text(&self, key: &str, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let parts = paste::parts(text);
        if !parts.iter().any(|p| matches!(p, paste::Part::Pasted(_))) {
            return text.to_string().into_any_element();
        }
        let theme = cx.theme();
        let (muted, border, hover, code_bg) = (theme.muted_foreground, theme.border, theme.secondary_hover, theme.background);
        let mut col = div().flex().flex_col().gap_1();
        for (k, part) in parts.into_iter().enumerate() {
            match part {
                paste::Part::Text(t) => {
                    if !t.trim().is_empty() {
                        col = col.child(t.trim().to_string());
                    }
                }
                paste::Part::Pasted(p) => {
                    let id = format!("paste:{key}:{k}");
                    let open = self.toggled.contains(&id);
                    let toggle = id.clone();
                    col = col.child(
                        div()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(border)
                            .overflow_hidden()
                            .child(
                                div()
                                    .id(SharedString::from(id))
                                    .px_2()
                                    .py_1()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_xs()
                                    .text_color(muted)
                                    .cursor_pointer()
                                    .hover(|d| d.bg(hover))
                                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                                    .child(format!("Pasted text · {}", paste::size(p)))
                                    .on_click(cx.listener(move |panel, _, _, cx| {
                                        if !panel.toggled.remove(&toggle) {
                                            panel.toggled.insert(toggle.clone());
                                        }
                                        panel.list.remeasure();
                                        cx.notify();
                                    })),
                            )
                            .when(open, |d| {
                                d.child(
                                    div()
                                        .px_2()
                                        .py_1()
                                        .bg(code_bg)
                                        .font_family("monospace")
                                        .text_xs()
                                        .flex()
                                        .flex_col()
                                        .children(p.lines().map(|l| if l.is_empty() { " ".to_string() } else { l.to_string() })),
                                )
                            }),
                    );
                }
            }
        }
        col.into_any_element()
    }

    pub(super) fn render_compacted(&self, summary: &str, trigger: Option<&str>, tokens: Option<u64>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, hover) = (theme.muted_foreground, theme.border, theme.secondary_hover);
        let key = format!("compact:{}:{}", summary.len(), summary.chars().take(40).collect::<String>());
        let open = self.toggled.contains(&key);
        let detail = match (trigger, tokens) {
            (Some(t), Some(n)) => format!(" ({t}, at {}k tokens)", n / 1000),
            (Some(t), None) => format!(" ({t})"),
            (None, Some(n)) => format!(" (at {}k tokens)", n / 1000),
            _ => String::new(),
        };
        let k = key.clone();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .id(SharedString::from(key))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(|d| d.bg(hover))
                    .child(div().flex_1().h(px(1.)).bg(border))
                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                    .child(format!("Context compacted here{detail}"))
                    .child(div().flex_1().h(px(1.)).bg(border))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        if !p.toggled.remove(&k) {
                            p.toggled.insert(k.clone());
                        }
                        p.list.remeasure();
                        cx.notify();
                    })),
            )
            .when(open && !summary.is_empty(), |d| {
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .text_color(muted)
                        .child(self.markdown(SharedString::from(format!("summary-{}", summary.len())), summary, cx)),
                )
            })
            .into_any_element()
    }

    /// A turn's summary (see [`keleuo_core::summary`]): how it ended and its headline, then
    /// what changed, how it was checked and what waits on the person, each section only when
    /// it has something. While it is being made, the same card with a spinner, so the summary
    /// fills in where it will stay; when it could not be made, why, quietly.
    pub(super) fn render_summary(&self, state: &summary::State, cx: &mut Context<Self>) -> AnyElement {
        // Turned off: kept summaries stay out of sight.
        if !crate::summaries_on(cx) {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let (fg, muted, border, card_bg) = (theme.foreground, theme.muted_foreground, theme.border, theme.secondary);
        let card = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py(px(10.))
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(card_bg.opacity(0.35))
            .text_sm()
            .line_height(rems(1.45));
        let s = match state {
            summary::State::Pending => {
                return card
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_color(muted)
                            .child(self.spinner(muted).small())
                            .child("Summarizing this turn…"),
                    )
                    .into_any_element();
            }
            summary::State::Failed(why) => {
                return div().text_size(px(12.)).text_color(muted).child(format!("No summary: {why}")).into_any_element();
            }
            summary::State::Ready(s) => s,
        };
        let amber = theme.yellow;
        // The turn's summary reads the turn; agents it left in the background are not done yet.
        let chat = &self.session.read(cx).chat;
        let background = chat.running_agents().filter(|a| a.background).count();
        let agents_on = background > 0 && chat.last_summary().is_some_and(|last| std::ptr::eq(last, s));
        let tone = match s.status {
            _ if agents_on => theme.magenta,
            summary::Status::Done => theme.green,
            summary::Status::Partial => theme.blue,
            summary::Status::Failed => theme.red,
            summary::Status::Waiting => amber,
        };
        let status_label = if agents_on { "Agents working" } else { s.status.label() };
        let (green, red, hover) = (theme.green, theme.red, theme.secondary_hover);
        let panel = cx.entity().downgrade();
        let mut card = card.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .px(px(7.))
                        .py(px(1.))
                        .rounded(px(5.))
                        .bg(tone.opacity(0.15))
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tone)
                        .child(status_label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(15.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(s.headline.clone()),
                )
                // Who wrote it: a summary is a model's reading of the turn, not the turn.
                .when(!s.model.is_empty(), |d| {
                    d.child(
                        div()
                            .flex_none()
                            .max_w(px(200.))
                            .truncate()
                            .text_size(px(11.))
                            .text_color(muted)
                            .child(format!("by {}", s.model)),
                    )
                }),
        );
        // A section: its title in the left column, its rows beside it.
        let section = |title: &'static str, color: Hsla, rows: Vec<AnyElement>| {
            div()
                .flex()
                .gap_3()
                .child(div().flex_none().w(px(76.)).text_size(px(13.)).text_color(color).child(title))
                .child(div().flex_1().min_w_0().flex().flex_col().gap(px(3.)).children(rows))
        };
        // A row that came from a piece of the reply: a click shows that piece in the chat.
        let row = |id: String, mark: AnyElement, text: String, source: Option<&summary::Source>| {
            let target = source.map(|s| s.text.clone());
            div()
                .id(SharedString::from(id))
                .flex()
                .gap_2()
                .px_1()
                .mx(px(-4.))
                .rounded(px(4.))
                .text_color(fg)
                // One line tall, so the mark sits on the item's first line.
                .child(div().flex_none().w(px(14.)).h(rems(1.45)).flex().items_center().justify_center().child(mark))
                .child(div().flex_1().min_w_0().child(text))
                .when_some(target, |d, target| {
                    let panel = panel.clone();
                    d.cursor_pointer().hover(|d| d.bg(hover)).on_click(move |_, _, cx| {
                        let _ = panel.update(cx, |p, cx| p.reveal_text(&target, cx));
                    })
                })
        };
        let bullet = || div().text_color(muted).child("•").into_any_element();
        let key = s.headline.len();
        if !s.changed.is_empty() {
            let rows = s.changed.iter().enumerate().map(|(n, c)| row(format!("sum-ch-{key}-{n}"), bullet(), c.clone(), None).into_any_element()).collect();
            card = card.child(section("Changed", muted, rows));
        }
        if !s.checked.is_empty() {
            let rows = s
                .checked
                .iter()
                .enumerate()
                .map(|(n, c)| {
                    let mark = match c.outcome {
                        summary::Outcome::Passed => Icon::new(IconName::Check).xsmall().text_color(green).into_any_element(),
                        summary::Outcome::Failed => Icon::new(IconName::CircleX).xsmall().text_color(red).into_any_element(),
                        summary::Outcome::NotRun => Icon::new(IconName::Minus).xsmall().text_color(muted).into_any_element(),
                        summary::Outcome::Unknown => div().text_color(muted).child("?").into_any_element(),
                    };
                    let what = match c.outcome {
                        summary::Outcome::NotRun => format!("{} · not run", c.what),
                        summary::Outcome::Unknown => format!("{} · result not given", c.what),
                        _ => c.what.clone(),
                    };
                    row(format!("sum-chk-{key}-{n}"), mark, what, c.source.as_ref()).into_any_element()
                })
                .collect();
            card = card.child(section("Checked", muted, rows));
        }
        if !s.needs_you.is_empty() {
            let rows = s
                .needs_you
                .iter()
                .enumerate()
                .map(|(n, t)| {
                    let dot = div().size(px(6.)).rounded_full().bg(amber).into_any_element();
                    let command = t.source.as_ref().and_then(|s| s.command.clone());
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(row(format!("sum-todo-{key}-{n}"), dot, t.text.clone(), t.source.as_ref()))
                        // The agent's own command for it, to run from here: shown as it runs.
                        .when_some(command, |d, command| {
                            d.child(
                                div()
                                    .ml(px(22.))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(MONO)
                                            .text_size(px(12.))
                                            .text_color(muted)
                                            .child(command.clone()),
                                    )
                                    .child(run_pill(SharedString::from(format!("sum-run-{key}-{n}")), command, background, panel.clone(), cx)),
                            )
                        })
                        .into_any_element()
                })
                .collect();
            card = card.child(section("Needs you", amber, rows));
        }
        card.into_any_element()
    }

    /// The end-of-turn card: the files the turn changed, each opening as its diff.
    pub(super) fn render_turn_changes(&self, ix: usize, changes: &[keleuo_core::changes::FileChange], cx: &mut Context<Self>) -> AnyElement {
        const SHOWN: usize = 8;
        let root = self.session.read(cx).cwd.clone();
        let theme = cx.theme();
        let (fg, muted, border, hover, green, red) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.secondary_hover, theme.green, theme.red);
        let (added, removed) = changes.iter().fold((0, 0), |(a, r), c| (a + c.added, r + c.removed));
        let count = |n: usize| if n == 1 { "1 file".to_string() } else { format!("{n} files") };
        let mut card = div()
            .flex()
            .flex_col()
            .rounded_md()
            .border_1()
            .border_color(border)
            .text_size(px(12.))
            .child(
                div()
                    .px_2()
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_color(fg)
                    .child(Icon::new(gpui_kit::assets::IconName::FileDiff).xsmall().text_color(muted))
                    .child(format!("Changed {}", count(changes.len())))
                    .child(div().text_size(px(11.)).text_color(green).child(format!("+{added}")))
                    .child(div().text_size(px(11.)).text_color(red).child(format!("−{removed}")))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(SharedString::from(format!("turn-changes-{ix}")))
                            .px_1()
                            .rounded(px(4.))
                            .text_size(px(11.))
                            .text_color(muted)
                            .cursor_pointer()
                            .hover(|d| d.bg(hover).text_color(fg))
                            .child("Changes")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatPanelEvent::ShowChanges))),
                    ),
            );
        for c in changes.iter().take(SHOWN) {
            let name = c.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let dir = c.path.parent().and_then(|p| p.strip_prefix(&root).ok()).map(|p| p.display().to_string()).unwrap_or_default();
            let path = c.path.clone();
            card = card.child(
                div()
                    .id(SharedString::from(format!("turn-change-{ix}-{}", c.path.display())))
                    .px_2()
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .border_t_1()
                    .border_color(border)
                    .cursor_pointer()
                    .hover(|d| d.bg(hover))
                    .child(crate::icons::file(&name, cx))
                    .child(div().flex_none().text_color(fg).child(name))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(11.)).text_color(muted).child(dir))
                    .when(c.by_command, |d| d.child(div().flex_none().text_size(px(10.)).text_color(muted).child("by a command")))
                    .when(c.created, |d| d.child(div().flex_none().text_size(px(10.)).text_color(green).child("new")))
                    .child(div().flex_none().text_size(px(11.)).text_color(green).child(format!("+{}", c.added)))
                    .child(div().flex_none().text_size(px(11.)).text_color(red).child(format!("−{}", c.removed)))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatPanelEvent::OpenDiff(path.clone())))),
            );
        }
        if changes.len() > SHOWN {
            card = card.child(
                div()
                    .id(SharedString::from(format!("turn-changes-more-{ix}")))
                    .px_2()
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(border)
                    .text_size(px(11.))
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(|d| d.bg(hover))
                    .child(format!("and {} more", count(changes.len() - SHOWN)))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ChatPanelEvent::ShowChanges))),
            );
        }
        card.into_any_element()
    }

    /// Another agent's message (a subagent's report): one row with its first line, the whole
    /// report on request.
    pub(super) fn render_agent_message(&self, from: &str, title: &str, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, hover, violet) =
            (theme.foreground, theme.muted_foreground, theme.border, theme.secondary_hover, theme.magenta);
        let key = format!("report:{from}:{}", text.len());
        let open = self.toggled.contains(&key);
        let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
        let k = key.clone();
        let header = div()
            .id(SharedString::from(key.clone()))
            .h(px(22.))
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .cursor_pointer()
            .hover(|d| d.bg(hover))
            .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall().text_color(muted))
            .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(violet).child("report"))
            .child(div().flex_none().max_w(relative(0.4)).truncate().text_color(fg).child(title.to_string()))
            .when(!open, |d| {
                d.child(div().flex_1().min_w_0().truncate().text_color(muted).child(one_line(&first.replace("**", ""), 200)))
            })
            .on_click(cx.listener(move |p, _, _, cx| {
                if !p.toggled.remove(&k) {
                    p.toggled.insert(k.clone());
                }
                p.list.remeasure();
                cx.notify();
            }));
        div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(border)
            .rounded_md()
            .bg(violet.opacity(0.05))
            .overflow_hidden()
            .child(header)
            .when(open, |d| {
                d.child(
                    div()
                        .px_2()
                        .py_1()
                        .border_t_1()
                        .border_color(border)
                        .text_color(fg)
                        .child(self.markdown(SharedString::from(format!("{key}-body")), text, cx)),
                )
            })
            .into_any_element()
    }

    /// The text of item `ix` when it is a preview of the reply being written (read off the
    /// agent's screen), not text from its record.
    pub(super) fn preview_text(&self, ix: usize, item: &Item, cx: &App) -> Option<String> {
        if self.agent.is_some() || self.session.read(cx).chat.preview_item() != Some(ix) {
            return None;
        }
        match item {
            Item::Assistant { text, .. } => Some(text.clone()),
            _ => None,
        }
    }

    /// The reply being written, as plain text with its lines kept (the screen has already
    /// turned its Markdown into terminal text); the finished reply replaces it, formatted.
    pub(super) fn render_preview(&self, text: String, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let mut col = div().w_full().flex().flex_col().text_color(fg).line_height(rems(1.5));
        for line in text.lines() {
            col = col.child(if line.trim().is_empty() { div().h(rems(0.75)) } else { div().child(line.to_string()) });
        }
        col.child(
            div()
                .pt_1()
                .flex()
                .items_center()
                .gap_1()
                .mr_2()
                .text_xs()
                .text_color(muted)
                .child(self.spinner(muted).xsmall())
                .child("Writing…"),
        )
        .into_any_element()
    }

    pub(super) fn markdown(&self, id: SharedString, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let md = self.index.linkify_markdown(text);
        let entity = cx.entity().downgrade();
        TextView::markdown(id, md)
            .selectable(true)
            .text_sm()
            // Code blocks: the language and a copy button; a one-line `!` command also gets Run,
            // which runs it (without the `!`) in a terminal tab of the session's folder.
            .code_block_actions({
                let panel = entity.clone();
                let session = self.session.clone();
                move |block, _, cx| {
                let code = block.code();
                let lang = block.lang().unwrap_or_else(|| "code".into());
                let muted = cx.theme().muted_foreground;
                let command = keleuo_core::conversation::shell_command(&code);
                let panel = panel.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(lang)
                    .when_some(command, |d, command| {
                        let background = session.read(cx).chat.running_agents().filter(|a| a.background).count();
                        d.child(run_pill("run-code", command, background, panel.clone(), cx))
                    })
                    .child(
                        Button::new("copy-code")
                            .xsmall()
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Copy)
                            .tooltip("Copy")
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                                gpui_kit::component::WindowExt::push_notification(window, "Copied", cx);
                            }),
                    )
                }
            })
            .on_link_click(move |url, ev, _, cx| {
                if let Some((path, line)) = links::parse_url(url) {
                    let _ = entity.update(cx, |p, cx| p.open(path, line, cx));
                } else if keleuo_core::web::is_web_link(url) && !ev.modifiers().platform {
                    // A web page opens in the Web panel; Cmd+click hands it to the browser.
                    let url = url.to_string();
                    let _ = entity.update(cx, |_, cx| cx.emit(ChatPanelEvent::OpenWeb(url)));
                } else {
                    cx.open_url(url);
                }
            })
            .into_any_element()
    }

    /// Plain text where file references are clickable spans.
    pub(super) fn linked_text(
        &self,
        id: SharedString,
        text: &str,
        color: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let found = self.index.find_links(text);
        if found.is_empty() {
            return div().child(text.to_string()).into_any_element();
        }
        let link_color = cx.theme().link;
        let ranges: Vec<std::ops::Range<usize>> = found.iter().map(|l| l.start..l.end).collect();
        let targets: Vec<(PathBuf, Option<u32>)> =
            found.into_iter().map(|l| (l.path, l.line)).collect();
        let mut highlights = Vec::new();
        for r in &ranges {
            highlights.push((
                r.clone(),
                HighlightStyle {
                    color: Some(link_color),
                    underline: Some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(link_color),
                        wavy: false,
                    }),
                    ..Default::default()
                },
            ));
        }
        let style = TextStyle {
            color,
            font_family: MONO.into(),
            font_size: px(11.).into(),
            ..Default::default()
        };
        let styled = StyledText::new(text.to_string()).with_default_highlights(&style, highlights);
        let entity = cx.entity().downgrade();
        InteractiveText::new(id, styled)
            .on_click(ranges, move |ix, _, cx| {
                if let Some((path, line)) = targets.get(ix).cloned() {
                    let _ = entity.update(cx, |p, cx| p.open(path, line, cx));
                }
            })
            .into_any_element()
    }

    /// Elements for a list of items; `key` keeps element ids unique per list.
    pub(super) fn render_items(&self, items: &[Item], key: &str, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (fg, muted, red, user_bg, _accent) = (
            theme.foreground,
            theme.muted_foreground,
            theme.red,
            theme.secondary,
            theme.ring,
        );
        let mut out = Vec::new();
        for (ix, item) in items.iter().enumerate() {
            let el = match item {
                // The person's message: a rounded bubble on the right, its Copy button beside
                // it on the left (beside rather than under, so the reply follows close below
                // and the message reads as the head of its turn).
                Item::User(text) => {
                    let copy = paste::without_markers(text);
                    let group = SharedString::from(format!("user-{key}-{ix}"));
                    div()
                        .group(group.clone())
                        .w_full()
                        .flex()
                        .justify_end()
                        .items_end()
                        .gap_1()
                        .child(hover_actions(group, format!("{key}-{ix}"), copy, muted))
                        .child(
                            div()
                                .max_w(relative(0.8))
                                .px_3()
                                .py_2()
                                .rounded(px(18.))
                                .bg(user_bg)
                                .text_sm()
                                .line_height(rems(1.45))
                                .text_color(fg)
                                .child(self.render_user_text(&format!("{key}-{ix}"), text, cx)),
                        )
                        .into_any_element()
                }
                // Claude's reply: plain text across the column, no bubble.
                Item::Assistant { text, streaming } => {
                    let copy = text.clone();
                    let group = SharedString::from(format!("reply-{key}-{ix}"));
                    div()
                        .group(group.clone())
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .text_color(fg)
                        .line_height(rems(1.5))
                        .child(self.markdown(SharedString::from(format!("msg-{key}-{ix}")), text, cx))
                        .when(!streaming, |d| d.child(hover_actions(group, format!("{key}-{ix}"), copy, muted)))
                        .into_any_element()
                }
                Item::Images(paths) => {
                    let mut row = div().w_full().flex().justify_end().gap_2().flex_wrap();
                    for (n, path) in paths.iter().enumerate() {
                        let p = path.clone();
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("img-{key}-{ix}-{n}")))
                                .size(px(96.))
                                .rounded(px(12.))
                                .overflow_hidden()
                                .border_1()
                                .border_color(cx.theme().border)
                                .cursor_pointer()
                                .child(img(path.clone()).size_full().object_fit(ObjectFit::Cover))
                                .on_click(move |_, _, cx| crate::image_view::show(p.clone(), cx)),
                        );
                    }
                    row.into_any_element()
                }
                Item::Tool(tool) => self.render_tool(ix, tool, cx),
                Item::Permission(p) => self.render_permission(p, cx),
                Item::Notice(t) => div()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(t.clone())
                    .into_any_element(),
                Item::Earlier { messages, .. } => div()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(format!("Earlier conversation, {messages} messages: reading..."))
                    .into_any_element(),
                Item::Compacted { summary, trigger, tokens } => {
                    self.render_compacted(summary, trigger.as_deref(), *tokens, cx)
                }
                Item::Error(t) => div()
                    .text_xs()
                    .text_color(red)
                    .child(t.clone())
                    .into_any_element(),
                Item::AgentMessage { from, title, text, .. } => self.render_agent_message(from, title, text, cx),
                Item::TurnChanges(changes) => self.render_turn_changes(ix, changes, cx),
                Item::Summary { state, .. } => self.render_summary(state, cx),
            };
            out.push(el);
        }
        out
    }

    /// The interface waits on a question before it is up: Trust / Don't trust for the folder
    /// question, else a pointer to the terminal.
    /// While the agent thinks, before its first text or tool call: a shimmering line above the
    /// composer, so a quiet stretch reads as work in progress rather than a stall.
    /// While the agent works: a quiet spinner, what it is doing and for how long ("Thinking ·
    /// 3m 02s"), in the message box beside the stop button. Not while it waits on the person
    /// (a prompt shows that), nor in Terminal (its interface shows its own).
    pub(super) fn render_working(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        if !session.chat.busy || session.chat.needs_input() || session.view == View::Terminal {
            return None;
        }
        // A streaming text item means the answer is being written.
        let answering = matches!(session.chat.items.last(), Some(Item::Assistant { streaming: true, .. }));
        let doing = match session.chat.status.as_deref() {
            _ if answering => "Writing",
            None | Some("thinking" | "working" | "sending" | "requesting") => "Thinking",
            Some(_) => "Working",
        };
        let muted = cx.theme().muted_foreground;
        let label = match self.busy_since.map(|t| format_elapsed(t.elapsed())) {
            Some(e) => format!("{doing} · {e}"),
            None => doing.to_string(),
        };
        Some(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(muted)
                .child(self.spinner(muted).xsmall())
                .child(label)
                .into_any_element(),
        )
    }

    /// The same at the foot of the conversation, as a chat app shows it: a spinner and grey
    /// words under the last message ("Thinking · 12s", or the tool running), while the agent
    /// works.
    pub(super) fn render_working_line(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        if !session.chat.busy || session.chat.needs_input() || session.view == View::Terminal {
            return None;
        }
        let answering = matches!(session.chat.items.last(), Some(Item::Assistant { streaming: true, .. }));
        let tool = session.chat.running_tools().last().map(|t| {
            let what = t.summary.lines().next().unwrap_or_default().trim().to_string();
            match t.name.as_str() {
                "Bash" | "Shell" => format!("Running {what}"),
                "Read" => format!("Reading {what}"),
                "Edit" | "MultiEdit" | "Write" | "StrReplace" => format!("Editing {what}"),
                "Grep" | "Glob" => format!("Searching {what}"),
                name => format!("{name} {what}"),
            }
        });
        let doing = match (answering, tool, session.chat.status.as_deref()) {
            (true, _, _) => "Writing".to_string(),
            (_, Some(t), _) => t,
            (_, None, None | Some("thinking" | "working" | "sending" | "requesting")) => "Thinking".into(),
            (_, None, Some(_)) => "Working".into(),
        };
        let label = match self.busy_since.map(|t| format_elapsed(t.elapsed())) {
            Some(e) => format!("{doing} · {e}"),
            None => doing,
        };
        let muted = cx.theme().muted_foreground;
        Some(
            div()
                .flex_none()
                .w_full()
                .flex()
                .justify_center()
                .px_4()
                .pb_2()
                .child(
                    div()
                        .w_full()
                        .max_w(px(COLUMN))
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(13.))
                        .text_color(muted)
                        .child(self.spinner(muted).small())
                        .child(div().min_w_0().truncate().child(label)),
                )
                .into_any_element(),
        )
    }

    /// Agents sent to the background go on after the turn ends, and the turn looks finished:
    /// a tinted bar above the message box says they are still working, with each one (a click
    /// opens it), until the last reports back.
    pub(super) fn render_background_agents(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        if session.view == View::Terminal {
            return None;
        }
        let running: Vec<(String, String, Option<std::time::Duration>)> = session
            .chat
            .running_agents()
            .filter(|a| a.background)
            .map(|a| (a.tool_use_id.clone(), a.title(), a.elapsed()))
            .collect();
        if running.is_empty() {
            return None;
        }
        let name = session.backend.name();
        let theme = cx.theme();
        let (fg, muted, violet, hover) = (theme.foreground, theme.muted_foreground, theme.magenta, theme.secondary_hover);
        let head = match running.len() {
            1 => "1 agent is still working".to_string(),
            n => format!("{n} agents are still working"),
        };
        let longest = running.iter().filter_map(|r| r.2).max().map(|d| format!(" · {}", format_elapsed(d))).unwrap_or_default();
        let after = if session.chat.busy {
            "in the background".to_string()
        } else {
            format!("in the background; {name} picks up when they report back")
        };
        let mut chips = div().flex().flex_wrap().gap_1();
        for (ix, (id, title, elapsed)) in running.into_iter().enumerate() {
            let elapsed = elapsed.map(|d| format!(" · {}", format_elapsed(d))).unwrap_or_default();
            chips = chips.child(
                div()
                    .id(("background-agent", ix))
                    .max_w(px(320.))
                    .px_2()
                    .py(px(1.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(violet.opacity(0.35))
                    .bg(theme.background)
                    .text_size(px(12.))
                    .text_color(fg)
                    .truncate()
                    .cursor_pointer()
                    .hover(|d| d.bg(hover))
                    .child(format!("{title}{elapsed}"))
                    .on_click(cx.listener(move |_, _, _, cx| cx.emit(ChatPanelEvent::OpenAgent(id.clone())))),
            );
        }
        Some(
            div()
                .flex_none()
                .w_full()
                .flex()
                .justify_center()
                .px_4()
                .pb_2()
                .child(
                    div()
                        .w_full()
                        .max_w(px(COLUMN))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(violet.opacity(0.5))
                        .bg(violet.opacity(0.07))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_size(px(13.))
                                .child(self.spinner(violet).small())
                                .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(fg).child(format!("{head}{longest}")))
                                .child(div().min_w_0().truncate().text_color(muted).child(after)),
                        )
                        .child(chips),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_asking(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        let asking = session.asking?;
        if session.view == View::Terminal {
            return None;
        }
        let name = session.backend.name();
        let theme = cx.theme();
        let (warning, border, muted) = (theme.warning, theme.border, theme.muted_foreground);
        let weak = self.session.downgrade();
        let show = {
            let weak = weak.clone();
            Button::new("asking-show").xsmall().label("Show terminal").on_click(move |_, window, cx| {
                let _ = weak.update(cx, |s, cx| s.set_view(View::Terminal, cx));
                if let Some(t) = weak.upgrade().and_then(|s| s.read(cx).terminal_view()) {
                    let f = t.read(cx).focus.clone();
                    window.focus(&f, cx);
                }
            })
        };
        let mut row = div()
            .flex_none()
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(border)
            .text_size(px(11.5))
            .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(warning));
        row = match asking {
            Asking::Trust => {
                let (w1, w2) = (weak.clone(), weak.clone());
                row.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(format!("{name} asks whether you trust this folder: it will be able to read, edit and run files here."))
                        .child(div().text_color(muted).child("Your answer goes to its own prompt, as if typed.")),
                )
                .child(Button::new("trust").xsmall().primary().label("Trust").on_click(move |_, _, cx| {
                    let _ = w1.update(cx, |s, cx| s.answer_trust(true, cx));
                }))
                .child(Button::new("distrust").xsmall().label("Don't trust").on_click(move |_, _, cx| {
                    let _ = w2.update(cx, |s, cx| s.answer_trust(false, cx));
                }))
                .child(show)
            }
            Asking::Other => row.child(div().flex_1().min_w_0().child(format!("{name} is asking something in the terminal."))).child(show),
        };
        Some(row.into_any_element())
    }
}

/// The room a finished reply keeps under its text for [`hover_actions`]: the column's gap and
/// an extra-small button.
pub(super) const HOVER_ROW: f32 = 24.;

/// Copy, by a message, shown while the pointer is over it.
pub(super) fn hover_actions(group: SharedString, key: String, text: String, muted: Hsla) -> impl IntoElement {
    div()
        .flex()
        .gap_1()
        .invisible()
        .group_hover(group, |d| d.visible())
        .child(
            Button::new(SharedString::from(format!("copy-{key}")))
                .xsmall()
                .ghost()
                .icon(gpui_kit::assets::IconName::Copy)
                .text_color(muted)
                .tooltip("Copy")
                .on_click(move |_, window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                    gpui_kit::component::WindowExt::push_notification(window, "Copied", cx);
                }),
        )
}

/// A green Run pill, so it reads as a button at a glance (brighter under the pointer, darker
/// while pressed): runs `command` in a terminal tab of the session's folder.
pub(super) fn run_pill(id: impl Into<ElementId>, command: String, background: usize, panel: WeakEntity<ChatPanel>, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    // Keleuo's own restart quits the app, and with it the agents working in the background:
    // amber, and it says so, while there are any. The caller counts them (`background`): this
    // runs while the panel is being drawn, and reading the panel from here panics.
    let stops = if restarts_keleuo(&command) { background } else { 0 };
    let (go, go_hover, go_active) = if stops > 0 {
        (theme.warning, theme.warning_hover, theme.warning_active)
    } else {
        (theme.success, theme.success_hover, theme.success_active)
    };
    // The dark theme's green is light: dark text on it reads, its own green does not.
    let go_fg = match (theme.is_dark(), stops > 0) {
        (true, _) => theme.background,
        (false, true) => theme.warning_foreground,
        (false, false) => theme.success_foreground,
    };
    let label = match stops {
        0 => "Run".to_string(),
        1 => "Run · stops 1 agent".to_string(),
        n => format!("Run · stops {n} agents"),
    };
    let tip: SharedString = match stops {
        0 => "Run it in a terminal in this folder".into(),
        n => format!("{n} background agent(s) are still working: restarting quits Keleuo and stops them. Wait for them to report back, or run it anyway.").into(),
    };
    div()
        .id(id)
        .flex_none()
        .h(px(20.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(10.))
        .bg(go)
        .text_size(px(11.))
        .text_color(go_fg)
        .font_weight(FontWeight::SEMIBOLD)
        .shadow_xs()
        .cursor_pointer()
        .hover(|d| d.bg(go_hover).shadow_sm())
        .active(|d| d.bg(go_active))
        .child(Icon::new(gpui_kit::assets::IconName::Play).xsmall().text_color(go_fg))
        .child(label)
        .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
        .on_click(move |_, _, cx| {
            let text = command.clone();
            let _ = panel.update(cx, |_, cx| cx.emit(ChatPanelEvent::RunCommand(text)));
        })
}

/// Whether `command` is Keleuo's own restart script (`./scripts/restart`), which quits the app.
fn restarts_keleuo(command: &str) -> bool {
    command.trim().trim_start_matches('!').split_whitespace().next().is_some_and(|w| w.ends_with("scripts/restart"))
}

#[cfg(test)]
mod run_pill_tests {
    use crate::agent_session::AgentSession;
    use crate::chat_panel::ChatPanel;
    use gpui_kit::{AppContext, TestAppContext};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[::core::prelude::v1::test]
    fn keleuos_restart_is_told_from_other_commands() {
        assert!(super::restarts_keleuo("./scripts/restart"));
        assert!(super::restarts_keleuo("! ./scripts/restart"));
        assert!(super::restarts_keleuo("scripts/restart --release"));
        assert!(!super::restarts_keleuo("cargo test"));
        assert!(!super::restarts_keleuo("echo ./scripts/restart"));
    }

    /// The Run pill is drawn while its panel is being drawn, so the panel is leased then:
    /// reading the panel from inside it panics (gpui's double lease). Drawn inside the panel's
    /// own update, as its render draws the message.
    #[gpui_kit::test]
    fn restart_pill_is_drawn_while_its_panel_is_leased(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            cx.set_global(crate::AppSettings(std::rc::Rc::new(std::cell::RefCell::new(keleuo_core::settings::Settings::load(None)))));
            cx.set_global(crate::backends::Backends(crate::backends::registry()));
        });
        let mut panel = None;
        cx.update(|cx| {
            let backend = crate::backends::backend(None, cx);
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                let session = cx.new(|_| AgentSession::new(Arc::new(keleuo_core::host::LocalHost), backend, PathBuf::from("/")));
                let p = cx.new(|cx| ChatPanel::new(session, window, cx));
                panel = Some(p.clone());
                p
            })
            .unwrap();
        });
        let panel = panel.unwrap();
        cx.update(|cx| {
            panel.update(cx, |_, cx| {
                let _ = super::run_pill("restart", "./scripts/restart".into(), 0, panel.downgrade(), cx);
            });
        });
    }
}
