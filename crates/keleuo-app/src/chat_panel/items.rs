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
            let (path, by_command) = (c.path.clone(), c.by_command);
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
                    .on_click(cx.listener(move |_, _, _, cx| {
                        let event = if by_command { ChatPanelEvent::OpenFile(path.clone(), None) } else { ChatPanelEvent::OpenDiff(path.clone()) };
                        cx.emit(event)
                    })),
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
                move |block, _, cx| {
                let code = block.code();
                let lang = block.lang().unwrap_or_else(|| "code".into());
                let muted = cx.theme().muted_foreground;
                let theme = cx.theme();
                let (go, go_hover, go_active) = (theme.success, theme.success_hover, theme.success_active);
                // The dark theme's green is light: dark text on it reads, its own green does not.
                let go_fg = if theme.is_dark() { theme.background } else { theme.success_foreground };
                let command = shell_command(&code);
                let panel = panel.clone();
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .text_color(muted)
                    .child(lang)
                    .when_some(command, |d, command| {
                        // A green pill, so it reads as a button at a glance: brighter under the
                        // pointer, darker while pressed.
                        d.child(
                            div()
                                .id("run-code")
                                .h(px(20.))
                                .px(px(8.))
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .rounded(px(10.))
                                .bg(go)
                                .text_color(go_fg)
                                .font_weight(FontWeight::SEMIBOLD)
                                .shadow_xs()
                                .cursor_pointer()
                                .hover(|d| d.bg(go_hover).shadow_sm())
                                .active(|d| d.bg(go_active))
                                .child(Icon::new(gpui_kit::assets::IconName::Play).xsmall().text_color(go_fg))
                                .child("Run")
                                .tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Run it in a terminal in this folder").build(w, cx))
                                .on_click(move |_, _, cx| {
                                    let text = command.clone();
                                    let _ = panel.update(cx, |_, cx| cx.emit(ChatPanelEvent::RunCommand(text)));
                                }),
                        )
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
                // The person's message: a rounded bubble on the right.
                Item::User(text) => {
                    let copy = paste::without_markers(text);
                    let group = SharedString::from(format!("user-{key}-{ix}"));
                    div()
                        .group(group.clone())
                        .w_full()
                        .flex()
                        .flex_col()
                        .items_end()
                        .gap_1()
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
                        .child(hover_actions(group, format!("{key}-{ix}"), copy, muted))
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
                                .on_click(cx.listener(move |panel, _, _, cx| {
                                    panel.preview = Some(p.clone());
                                    cx.notify();
                                })),
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
            };
            out.push(el);
        }
        out
    }

    /// The interface waits on a question before it is up: Trust / Don't trust for the folder
    /// question, else a pointer to the terminal.
    /// While the agent thinks, before its first text or tool call: a shimmering line above the
    /// composer, so a quiet stretch reads as work in progress rather than a stall.
    pub(super) fn render_thinking(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = self.session.read(cx);
        // A streaming text item means the answer has started (its first delta sets the status
        // to "writing"; the item appears a moment earlier).
        let answering = matches!(session.chat.items.last(), Some(Item::Assistant { streaming: true, .. }));
        // Shown for the whole turn while no answer is streaming: a session run through its
        // terminal reports "working" or the tool it runs, never "thinking".
        if !session.chat.busy || answering || session.chat.needs_input() || session.view == View::Terminal {
            return None;
        }
        let name = session.backend.name();
        let label = match session.chat.status.as_deref() {
            None | Some("thinking" | "working" | "sending" | "requesting") => format!("{name} is thinking…"),
            Some(_) => format!("{name} is working…"),
        };
        let theme = cx.theme();
        let (ring, border, accent, muted, theme_fg) = (theme.ring, theme.border, theme.accent, theme.muted_foreground, theme.foreground);
        let elapsed = self.busy_since.map(|t| format_elapsed(t.elapsed()));
        Some(
            div()
                .flex_none()
                .px_2()
                .py_1()
                .flex()
                .items_center()
                .gap_2()
                .border_t_1()
                .border_color(border)
                .bg(accent)
                .text_sm()
                .child(self.spinner(ring).small())
                .child(div().text_color(theme_fg).child(label))
                // How long this turn has been going, at the right.
                .child(div().flex_1())
                .when_some(elapsed, |d, e| d.child(div().flex_none().text_xs().text_color(muted).child(e)))
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

/// Copy, under a message, shown while the pointer is over it.
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

/// A code block that is one shell command written as for the agent's shell (`! git push`): the
/// command, without the `!`.
pub(super) fn shell_command(code: &str) -> Option<String> {
    let mut lines = code.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?;
    let command = first.strip_prefix('!')?.trim();
    (lines.next().is_none() && !command.is_empty()).then(|| command.to_string())
}

#[cfg(test)]
mod shell_command_tests {
    use super::shell_command;

    #[test]
    fn one_bang_line_is_a_command() {
        assert_eq!(shell_command("! git push -u origin main\n").as_deref(), Some("git push -u origin main"));
        assert_eq!(shell_command("!ls").as_deref(), Some("ls"));
        assert_eq!(shell_command("ls -la"), None);
        assert_eq!(shell_command("! a\n! b"), None, "several commands: copy them instead");
        assert_eq!(shell_command("!"), None);
    }
}
