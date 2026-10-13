//! The message box: Markdown marks, `/` commands, long pastes as pills, attachments, the
//! model picker, and lists that continue on Shift+Enter.

use super::*;

/// Shift+Enter on a list line in the message box: the next item's marker on the new line, or
/// the marker taken away on an item left empty (ending the list). False when it is a plain new
/// line. Edits only around the cursor, so paste tokens stay tokens and Undo takes it back.
pub(super) fn continue_list(input: &Entity<TextareaState>, window: &mut Window, cx: &mut App) -> bool {
    let (text, selected) = {
        let input = input.read(cx);
        (input.value(), input.selected_range())
    };
    if !selected.is_empty() {
        return false;
    }
    let cursor = selected.start;
    let start = text[..cursor].rfind('\n').map_or(0, |i| i + 1);
    let end = text[cursor..].find('\n').map_or(text.len(), |i| cursor + i);
    let Some(step) = compose::list_break(&text[start..end], cursor - start) else { return false };
    // Splitting an item: the spaces after the cursor go, so the rest follows the marker as typed.
    let spaces = text[cursor..end].len() - text[cursor..end].trim_start_matches([' ', '\t']).len();
    input.update(cx, |s, cx| match step {
        ListBreak::Continue(marker) => {
            if spaces > 0 {
                s.set_selected_range(cursor..cursor + spaces, cx);
            }
            s.replace(format!("\n{marker}"), window, cx)
        }
        ListBreak::End => {
            s.set_selected_range(start..end, cx);
            s.replace("", window, cx);
        }
    });
    true
}

/// A paste's token in the message box: a small raised pill (rounded, lit from above, a soft
/// shadow) with its size.
pub(super) fn paste_pill(token: &InlineTokenContext, _: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let base = theme.secondary;
    let (top, bottom, edge) = if theme.is_dark() {
        (Hsla { l: (base.l + 0.13).min(1.), ..base }, Hsla { l: (base.l + 0.04).min(1.), ..base }, theme.foreground.opacity(0.18))
    } else {
        (Hsla { l: (base.l + 0.03).min(1.), ..base }, Hsla { l: (base.l - 0.05).max(0.), ..base }, theme.border)
    };
    let edge = if token.is_selected() { theme.ring } else { edge };
    div()
        .h(token.line_height() - px(2.))
        .mx(px(1.))
        .px(px(7.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(7.))
        .border_1()
        .border_color(edge)
        .bg(linear_gradient(180., linear_color_stop(top, 0.), linear_color_stop(bottom, 1.)))
        .shadow_xs()
        .text_xs()
        .text_color(theme.foreground)
        .child(Icon::new(gpui_kit::assets::IconName::FileText).xsmall().text_color(theme.muted_foreground))
        .child(token.token().label().clone())
        .into_any_element()
}

impl ChatPanel {
    /// Tests: send `text` with what is attached and pasted.
    pub fn test_send(&mut self, text: &str, cx: &mut Context<Self>) {
        let images = std::mem::take(&mut self.attachments);
        let text = paste::expand(text, &std::mem::take(&mut self.pastes), true);
        self.session.update(cx, |s, cx| s.send_with(&text, images, cx));
    }

    /// Tests: paste `text` into the message box (short text goes in as typed).
    pub fn test_paste(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if paste::is_long(&text) {
            self.paste_long(text, window, cx);
        } else {
            self.input.update(cx, |s, cx| s.insert(text, window, cx));
        }
    }

    /// Light Markdown in the message box: code on a faint ground, headings and bold in bold,
    /// italic in italic, quotes and Markdown's own characters quieter. The text is unchanged.
    /// Marked again on every change (the styles follow edits, but what is Markdown can change).
    pub(super) fn mark_markdown(&mut self, cx: &mut Context<Self>) {
        use keleuo_core::markdown_marks::{Mark, marks};
        let theme = cx.theme();
        let (muted, code_bg, link) = (theme.muted_foreground, theme.foreground.opacity(0.07), theme.blue);
        self.marked_dark = Some(theme.is_dark());
        let text = self.input.read(cx).value().to_string();
        let mut decorations: Vec<TextDecoration> = marks(&text)
            .into_iter()
            .map(|(range, mark)| {
                let style = match mark {
                    Mark::Code => HighlightStyle { background_color: Some(code_bg), ..Default::default() },
                    Mark::Heading | Mark::Bold => HighlightStyle { font_weight: Some(FontWeight::BOLD), ..Default::default() },
                    Mark::Italic => HighlightStyle { font_style: Some(FontStyle::Italic), ..Default::default() },
                    Mark::Quote | Mark::Syntax => HighlightStyle { color: Some(muted), ..Default::default() },
                    Mark::Link => HighlightStyle {
                        color: Some(link),
                        underline: Some(UnderlineStyle { thickness: px(1.), color: Some(link.opacity(0.5)), wavy: false }),
                        ..Default::default()
                    },
                };
                TextDecoration::new(range, style)
            })
            .collect();
        // A fence naming a language (```ts): its code in the code viewer's colours.
        let highlight_theme = cx.theme().highlight_theme.clone();
        for (lang, range) in keleuo_core::markdown_marks::fences(&text) {
            let highlighter = self
                .md_highlighters
                .entry(lang.clone())
                .or_insert_with(|| gpui_kit::component::highlighter::SyntaxHighlighter::new(&lang));
            let code = gpui_kit::component::input::Rope::from(&text[range.clone()]);
            highlighter.update(None, &code, None);
            for (r, style) in highlighter.styles(&(0..code.len()), &*highlight_theme) {
                if style != HighlightStyle::default() {
                    decorations.push(TextDecoration::new(range.start + r.start..range.start + r.end, style));
                }
            }
        }
        match &self.md_marks {
            Some(marks) => marks.set(decorations, cx),
            None => self.md_marks = Some(self.input.update(cx, |s, cx| s.create_decorations_collection(decorations, cx))),
        }
    }

    /// The `/` list for what is typed: its matches, when the message is a lone `/word` and the
    /// list was not closed. Reads the skills the first time.
    pub(super) fn slash_matches(&mut self, cx: &mut Context<Self>) -> Vec<keleuo_core::slash::Choice> {
        let value = self.input.read(cx).value().to_string();
        let Some(query) = keleuo_core::slash::typed(&value).map(str::to_string) else {
            self.slash_closed = false;
            return Vec::new();
        };
        if query != self.slash_query {
            self.slash_query = query.clone();
            self.slash_ix = 0;
            self.slash_closed = false;
        }
        let Some(choices) = self.slash_choices.clone() else {
            self.load_slash_choices(cx);
            return Vec::new();
        };
        if self.slash_closed {
            return Vec::new();
        }
        keleuo_core::slash::matching(&choices, &query, 8).into_iter().cloned().collect()
    }

    pub(super) fn load_slash_choices(&mut self, cx: &mut Context<Self>) {
        self.slash_choices = Some(Rc::new(Vec::new()));
        let s = self.session.read(cx);
        let (backend, host, root) = (s.backend.clone(), s.host(), s.cwd.clone());
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let catalog = backend.skills(host.as_ref(), &root);
                    keleuo_core::slash::choices(&catalog, backend.builtin_commands(), backend.name())
                })
                .await;
            let _ = this.update(cx, |p, cx| {
                p.slash_choices = Some(Rc::new(found));
                cx.notify();
            });
        })
        .detach();
    }

    /// Put the highlighted `/` choice in the message box, replacing what was typed after the `/`.
    pub(super) fn accept_slash(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let typed = 1 + self.slash_query.len();
        let text = format!("/{name} ");
        self.input.update(cx, |s, cx| {
            s.set_selected_range(0..typed, cx);
            s.replace(text, window, cx);
        });
        self.slash_closed = true;
        cx.notify();
    }

    /// Keys while the `/` list is up: Up and Down move, Tab (or Enter on a name not yet typed in
    /// full) takes the highlighted one, Escape closes the list. True when the key was used.
    pub(super) fn slash_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let found = self.slash_matches(cx);
        if found.is_empty() {
            return false;
        }
        match key {
            "up" => self.slash_ix = self.slash_ix.checked_sub(1).unwrap_or(found.len() - 1),
            "down" => self.slash_ix = (self.slash_ix + 1) % found.len(),
            "escape" => self.slash_closed = true,
            "tab" | "enter" => {
                let pick = &found[self.slash_ix.min(found.len() - 1)];
                // Typed in full already: Enter sends it.
                if key == "enter" && pick.name == self.slash_query {
                    return false;
                }
                let name = pick.name.clone();
                self.accept_slash(&name, window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// The `/` list over the message box.
    pub(super) fn render_slash(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let found = self.slash_matches(cx);
        if found.is_empty() {
            return None;
        }
        let ix = self.slash_ix.min(found.len() - 1);
        let theme = cx.theme();
        let (fg, muted, sel, border, bg) = (theme.foreground, theme.muted_foreground, theme.secondary, theme.border, theme.popover);
        let mut list = div()
            .id("slash-list")
            .w_full()
            .mb_1()
            .p_1()
            .rounded(px(12.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .shadow_md()
            .flex()
            .flex_col();
        for (n, choice) in found.into_iter().enumerate() {
            let name = choice.name.clone();
            list = list.child(
                div()
                    .id(("slash", n))
                    .px_2()
                    .py(px(5.))
                    .rounded(px(8.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .when(n == ix, |d| d.bg(sel))
                    .hover(|d| d.bg(sel))
                    .child(div().flex_none().text_sm().font_family("monospace").text_color(fg).child(format!("/{}", choice.name)))
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(muted).child(choice.description))
                    .child(div().flex_none().text_xs().text_color(muted).child(choice.from))
                    .on_click(cx.listener(move |p, _, window, cx| {
                        p.accept_slash(&name, window, cx);
                        p.input.update(cx, |s, cx| s.focus(window, cx));
                    })),
            );
        }
        Some(list.into_any_element())
    }

    /// "Other model…": a field for any model name the backend takes; Enter switches to it and
    /// keeps it among the recent ones, Escape or clicking away drops it.
    pub(super) fn start_model_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Model name, e.g. claude-opus-4-1"));
        input.update(cx, |s, cx| s.focus(window, cx));
        let sub = cx.subscribe_in(&input, window, |p, input, ev: &InputEvent, _, cx| match ev {
            InputEvent::PressEnter { .. } => {
                let name = input.read(cx).value().trim().to_string();
                p.model_input = None;
                if !name.is_empty() {
                    remember_model(&name, cx);
                    p.session.update(cx, |s, cx| s.set_model(&name, cx));
                }
                cx.notify();
            }
            InputEvent::Blur => {
                p.model_input = None;
                cx.notify();
            }
            _ => {}
        });
        self.model_input = Some((input, sub));
        cx.notify();
    }

    /// Tests: "Other model…" with `name` typed and Enter.
    pub fn test_other_model(&mut self, name: &str, cx: &mut Context<Self>) {
        remember_model(name, cx);
        self.session.update(cx, |s, cx| s.set_model(name, cx));
    }

    /// Tests: a paste's "Show in text field" (`inline`) or "Remove".
    pub fn test_unfold(&mut self, n: usize, inline: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.unfold_paste(n, inline, window, cx);
    }

    /// Tests: press send.
    pub fn test_send_composed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.send_composed(window, cx);
    }

    /// Send what the message box holds: its text with the long pastes put back (marked, so
    /// the chat folds them), and the attached images. The box empties.
    pub(super) fn send_composed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.input.read(cx).value().to_string();
        if typed.trim().is_empty() && self.attachments.is_empty() {
            return;
        }
        let text = paste::expand(&typed, &std::mem::take(&mut self.pastes), true);
        self.input.update(cx, |s, cx| s.set_value("", window, cx));
        // Sending goes back to the end and follows it.
        self.list.set_follow_mode(FollowMode::Tail);
        let images = std::mem::take(&mut self.attachments);
        self.session.update(cx, |s, cx| s.send_with(&text, images, cx));
    }

    /// A long paste: kept aside, and a token for it put at the cursor. The token is one unit
    /// in the box (the cursor steps over it, Backspace takes it whole), drawn as a pill; its
    /// text is the `[Pasted text #n · …]` that sending expands.
    pub(super) fn paste_long(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let n = self.pastes.len() + 1;
        let typed = paste::token(n, &text);
        let token = InlineToken::new(format!("paste-{n}"), typed.clone()).with_label(format!("Pasted text · {}", paste::size(&text)));
        self.pastes.push(text);
        self.input.update(cx, |s, cx| {
            if s.replace_with_token(token, window, cx).is_err() {
                s.insert(typed, window, cx);
            }
        });
        cx.notify();
    }

    /// Paste `n` back into the message box as plain text (`inline`), or out of it. The other
    /// tokens stay tokens.
    pub(super) fn unfold_paste(&mut self, n: usize, inline: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pasted) = self.pastes.get(n - 1).cloned() else { return };
        let id = format!("paste-{n}");
        let content = self.input.read(cx).content();
        let text = content.text().to_string();
        let (mut out, mut kept, mut last) = (String::new(), Vec::new(), 0);
        for span in content.tokens() {
            let r = span.range();
            out.push_str(&text[last..r.start]);
            last = r.end;
            if span.token().id().as_ref() == id {
                if inline {
                    out.push_str(&pasted);
                }
            } else {
                let start = out.len();
                out.push_str(&text[r]);
                kept.push((start..out.len(), span.token().clone()));
            }
        }
        out.push_str(&text[last..]);
        let mut rebuilt = InputContent::new(out.clone());
        for (range, token) in kept {
            rebuilt = match rebuilt.clone().with_token(range, token) {
                Ok(c) => c,
                Err(_) => rebuilt,
            };
        }
        self.input.update(cx, |s, cx| s.set_value(rebuilt, window, cx));
        self.mark_markdown(cx);
        cx.notify();
    }

    /// Tests: attach an image file.
    pub fn test_attach(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.attach([path], cx);
    }

    /// Add image files to the next message.
    pub(super) fn attach(&mut self, paths: impl IntoIterator<Item = PathBuf>, cx: &mut Context<Self>) {
        for p in paths {
            let image = matches!(
                p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
                Some("png" | "jpg" | "jpeg" | "gif" | "webp")
            );
            if image && !self.attachments.contains(&p) {
                self.attachments.push(p);
            }
        }
        cx.notify();
    }

    /// A pasted image (bytes from the clipboard): kept in the session's image folder.
    pub(super) fn attach_bytes(&mut self, bytes: &[u8], ext: &str, cx: &mut Context<Self>) {
        let id = self.session.read(cx).chat.session_id.clone();
        if let Some(dir) = keleuo_core::images::session_dir(id.as_deref())
            && let Ok(path) = keleuo_core::images::store(&dir, bytes, ext)
        {
            self.attach([path], cx);
        }
    }

    /// The composer: a rounded box floating at the bottom of the column, with the images to
    /// send, the text, and the session's controls (attach, model, permission mode, send/stop).
    pub(super) fn render_composer(&self, slash: Option<AnyElement>, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (border, muted, fg, field, accent) = (theme.border, theme.muted_foreground, theme.foreground, theme.secondary, theme.ring);
        let session = self.session.read(cx);
        let busy = session.chat.busy;
        let caps = session.backend.capabilities();
        let model = session.chat.model.clone().or_else(|| session.model.clone()).unwrap_or_else(|| "default model".into());
        let mode = session.live_mode.clone().or_else(|| session.permission_mode.clone()).unwrap_or_else(|| "default".into());
        let has_text = !self.input.read(cx).value().trim().is_empty() || !self.attachments.is_empty();
        let mono = !cx.global::<crate::AppSettings>().0.borrow().always_proportional
            && keleuo_core::markdown_marks::has_fence(&self.input.read(cx).value());
        let restarting = session.restarting.clone();
        let queued = session.queued_count();
        let name = session.backend.name();
        let weak = self.session.downgrade();
        let entity = cx.entity().downgrade();

        let mut chips = div().flex().flex_wrap().gap_2();
        for (n, path) in self.attachments.iter().enumerate() {
            let (p1, p2) = (path.clone(), path.clone());
            chips = chips.child(
                div()
                    .id(("chip", n))
                    .relative()
                    .size(px(56.))
                    .rounded(px(10.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(border)
                    .cursor_pointer()
                    .child(img(path.clone()).size_full().object_fit(ObjectFit::Cover))
                    .on_click(cx.listener(move |p, _, _, cx| {
                        p.preview = Some(p1.clone());
                        cx.notify();
                    }))
                    .child(
                        div()
                            .id(("chip-x", n))
                            .absolute()
                            .top(px(2.))
                            .right(px(2.))
                            .size(px(16.))
                            .rounded_full()
                            .bg(gpui_kit::black().opacity(0.7))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::Close).xsmall().text_color(gpui_kit::white()))
                            .on_click(cx.listener(move |p, _, _, cx| {
                                cx.stop_propagation();
                                p.attachments.retain(|a| *a != p2);
                                cx.notify();
                            })),
                    ),
            );
        }

        // Long pastes still in the box: what each holds, to put back in the text or drop.
        let value = self.input.read(cx).value().to_string();
        let mut paste_chips = 0;
        for n in paste::tokens_in(&value) {
            let Some(text) = self.pastes.get(n.wrapping_sub(1)) else { continue };
            paste_chips += 1;
            let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            let action = |id: &'static str, label: &'static str, inline: bool| {
                div()
                    .id((id, n))
                    .px_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|d| d.text_color(fg))
                    .child(label)
                    .on_click(cx.listener(move |p, _, window, cx| p.unfold_paste(n, inline, window, cx)))
            };
            chips = chips.child(
                div()
                    .h(px(56.))
                    .max_w(px(280.))
                    .px_2()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(border)
                    .bg(field)
                    .flex()
                    .flex_col()
                    .justify_center()
                    .text_xs()
                    .text_color(muted)
                    .child(div().text_color(fg).truncate().child(format!("Pasted text · {}", paste::size(text))))
                    .child(div().truncate().font_family("monospace").child(first))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(action("paste-inline", "Show in text field", true))
                            .child(action("paste-drop", "Remove", false)),
                    ),
            );
        }

        // The backend's own names (aliases, each the latest of its family), the names typed in
        // before, and "Other model…" to type one. The running one is ticked; an alias shows
        // what it stands for.
        let models: Vec<String> = caps.models.clone();
        let recent: Vec<String> = cx.global::<crate::AppSettings>().0.borrow().recent_models.clone();
        let (w_model, w_mode) = (weak.clone(), weak.clone());
        let panel_for_menu = entity.clone();
        // What was picked (an alias or a typed name), and what it runs as.
        let (picked, running_as) = (session.model.clone(), session.chat.model.clone());
        let model_button = Button::new("model")
            .xsmall()
            .ghost()
            .label(short_model(&model))
            .tooltip("Model")
            .dropdown_menu(move |menu, _, _| {
                use gpui_kit::component::menu::PopupMenuItem;
                let mut menu = menu;
                let is = |m: &str| match (&picked, &running_as) {
                    (Some(p), _) => p == m,
                    (None, Some(r)) => r.contains(m),
                    (None, None) => false,
                };
                for m in &models {
                    let (w, m2) = (w_model.clone(), m.clone());
                    let running = is(m);
                    let label = match (&running_as, running) {
                        (Some(r), true) if r != m => format!("{} ({})", capitalize(m), r),
                        _ => capitalize(m),
                    };
                    menu = menu.item(PopupMenuItem::new(label).checked(running).on_click(move |_, _, cx| {
                        let _ = w.update(cx, |s, cx| s.set_model(&m2, cx));
                    }));
                }
                if !recent.is_empty() {
                    menu = menu.separator();
                    for m in &recent {
                        let (w, m2) = (w_model.clone(), m.clone());
                        menu = menu.item(PopupMenuItem::new(m.clone()).checked(picked.as_deref() == Some(m.as_str())).on_click(move |_, _, cx| {
                            remember_model(&m2, cx);
                            let _ = w.update(cx, |s, cx| s.set_model(&m2, cx));
                        }));
                    }
                }
                let panel = panel_for_menu.clone();
                menu.separator().item(PopupMenuItem::new("Other model…").on_click(move |_, window, cx| {
                    let _ = panel.update(cx, |p, cx| p.start_model_input(window, cx));
                }))
            });
        // Typing a model name: the field takes the button's place.
        let model_control = match &self.model_input {
            Some((input, _)) => div()
                .w(px(240.))
                .capture_key_down(cx.listener(|p, ev: &KeyDownEvent, _, cx| {
                    if ev.keystroke.key == "escape" {
                        cx.stop_propagation();
                        p.model_input = None;
                        cx.notify();
                    }
                }))
                .child(Input::new(input).xsmall())
                .into_any_element(),
            None => model_button.into_any_element(),
        };
        let modes: Vec<(Option<String>, &'static str)> = vec![
            (None, "Ask before acting"),
            (caps.accept_edits_mode.clone(), "Accept edits"),
            (Some("plan".to_string()), "Plan"),
        ];
        let mode_label = match mode.as_str() {
            "acceptEdits" => "Accept edits",
            "plan" => "Plan",
            "auto" => "Auto",
            "bypassPermissions" => "Bypass permissions",
            _ => "Ask before acting",
        };
        let mode_button = Button::new("mode")
            .xsmall()
            .ghost()
            .label(mode_label)
            .tooltip("Permission mode")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                for (m, label) in &modes {
                    let (w, m2) = (w_mode.clone(), m.clone());
                    menu = menu.item(gpui_kit::component::menu::PopupMenuItem::new(*label).on_click(move |_, _, cx| {
                        let _ = w.update(cx, |s, cx| s.set_mode(m2.clone(), cx));
                    }));
                }
                menu
            });
        let send = div()
            .id("send")
            .size(px(28.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .when(busy, |d| d.bg(fg))
            .when(!busy && has_text, |d| d.bg(fg))
            .when(!busy && !has_text, |d| d.bg(muted.opacity(0.35)))
            .child(
                Icon::new(if busy { gpui_kit::assets::IconName::Square } else { gpui_kit::assets::IconName::ArrowUp })
                    .small()
                    .text_color(theme.background),
            )
            .tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(if busy { "Stop (Esc in the terminal)" } else { "Send (Enter)" }).build(w, cx))
            .on_click(cx.listener(move |p, _, window, cx| {
                if busy {
                    p.session.update(cx, |s, cx| s.interrupt(cx));
                    return;
                }
                p.send_composed(window, cx);
            }));
        let attach = Button::new("attach")
            .xsmall()
            .ghost()
            .icon(gpui_kit::assets::IconName::Paperclip)
            .tooltip("Attach images")
            .on_click(cx.listener(|_, _, _, cx| {
                let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: None });
                cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    if let Ok(Ok(Some(paths))) = paths.await {
                        let _ = this.update(cx, |p, cx| p.attach(paths, cx));
                    }
                })
                .detach();
            }));
        let paste_target = entity.clone();
        div()
            .flex_none()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .px_4()
            .pb_3()
            .pt_1()
            // The `/` list, just above the box it completes.
            .children(slash.map(|s| div().w_full().max_w(px(COLUMN)).child(s)))
            .child(
                div()
                    .id("composer")
                    .w_full()
                    .max_w(px(COLUMN))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .rounded(px(20.))
                    .border_1()
                    .border_color(border)
                    .bg(field)
                    .shadow_md()
                    .drag_over::<ExternalPaths>(move |d, _, _, _| d.border_color(accent))
                    .on_drop(cx.listener(|p, paths: &ExternalPaths, _, cx| p.attach(paths.paths().to_vec(), cx)))
                    // Shift+Enter on a list item: before the box puts in its plain new line.
                    // Enter with the `/` list up takes the highlighted name.
                    .capture_action(cx.listener(|p, action: &Enter, window, cx| {
                        let used = !action.secondary
                            && if action.shift { continue_list(&p.input, window, cx) } else { p.slash_key("enter", window, cx) };
                        if used {
                            cx.stop_propagation();
                        }
                    }))
                    // Up, Down, Tab and Escape steer the `/` list while it is up (they are the
                    // box's own actions, so they are caught before it moves its cursor).
                    .capture_action(cx.listener(|p, _: &MoveUp, window, cx| {
                        if p.slash_key("up", window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(cx.listener(|p, _: &MoveDown, window, cx| {
                        if p.slash_key("down", window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(cx.listener(|p, _: &IndentInline, window, cx| {
                        if p.slash_key("tab", window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .capture_action(cx.listener(|p, _: &Escape, window, cx| {
                        if p.slash_key("escape", window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .when_some(restarting, |d, why| {
                        // Restarting (a new model or mode): what is typed now waits for it.
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_size(px(11.5))
                                .text_color(muted)
                                .child(Icon::new(IconName::LoaderCircle).xsmall())
                                .child(match queued {
                                    0 => format!("Restarting {name} {why}. A message sent now goes when it is up."),
                                    n => format!("Restarting {name} {why}. {n} message{} waiting to go.", if n == 1 { "" } else { "s" }),
                                }),
                        )
                    })
                    .when(!self.attachments.is_empty() || paste_chips > 0, |d| d.child(chips))
                    .child(
                        Textarea::new(&self.input)
                            .appearance(false)
                            .text_sm()
                            // A message holding a code block is written in the code font (View menu).
                            .when(mono, |t| t.font_family(cx.theme().mono_font_family.clone()))
                            .token(paste_pill)
                            .on_paste(move |item, window, cx| {
                                // An image (or image files) on the clipboard: attach it.
                                let mut took = false;
                                for entry in item.entries() {
                                    match entry {
                                        ClipboardEntry::Image(image) => {
                                            let ext = match image.format {
                                                ImageFormat::Jpeg => "jpg",
                                                ImageFormat::Gif => "gif",
                                                ImageFormat::Webp => "webp",
                                                _ => "png",
                                            };
                                            let bytes = image.bytes.clone();
                                            let _ = paste_target.update(cx, |p, cx| p.attach_bytes(&bytes, ext, cx));
                                            took = true;
                                        }
                                        ClipboardEntry::ExternalPaths(paths) => {
                                            let paths = paths.paths().to_vec();
                                            let _ = paste_target.update(cx, |p, cx| p.attach(paths, cx));
                                            took = true;
                                        }
                                        _ => {}
                                    }
                                }
                                // Long text: a token in the box, the text kept aside (the box is
                                // being changed now, so after this).
                                if !took
                                    && let Some(text) = item.text().filter(|t| paste::is_long(t))
                                {
                                    let target = paste_target.clone();
                                    window.defer(cx, move |window, cx| {
                                        let _ = target.update(cx, |p, cx| p.paste_long(text, window, cx));
                                    });
                                    took = true;
                                }
                                took
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_color(muted)
                            .child(attach)
                            .child(model_control)
                            .when(caps.permission_prompts, |d| d.child(mode_button))
                            .child(div().flex_1())
                            .child(send),
                    ),
            )
            .into_any_element()
    }
}

/// `claude-opus-5-5` as `opus-5-5`; a short name stays.
/// Keep a typed model name among the recent ones, saved.
pub(super) fn remember_model(name: &str, cx: &mut App) {
    let settings = cx.global::<crate::AppSettings>().0.clone();
    settings.borrow_mut().remember_model(name);
    let _ = settings.borrow().save();
}

pub(super) fn short_model(model: &str) -> String {
    model.strip_prefix("claude-").unwrap_or(model).to_string()
}

pub(super) fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

#[cfg(test)]
mod list_tests {
    use gpui_kit::component::input::{Enter, Textarea, TextareaState};
    use gpui_kit::{AppContext, Context, Entity, Focusable, IntoElement, Render, TestAppContext, Window, div, prelude::*};

    use super::continue_list;

    /// A message box wired as the composer is: Shift+Enter caught before the box's own new line.
    struct Composer(Entity<TextareaState>);
    impl Render for Composer {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .capture_action(cx.listener(|p, action: &Enter, window, cx| {
                    if action.shift && !action.secondary && continue_list(&p.0, window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .child(Textarea::new(&self.0))
        }
    }

    #[gpui_kit::test]
    fn shift_enter_continues_and_ends_a_list(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let mut input = None;
        let handle = cx
            .update(|cx| {
                gpui_kit::open_window(Default::default(), cx, |window, cx| {
                    let state = cx.new(|cx| TextareaState::new(window, cx).submit_on_enter(true));
                    window.focus(&state.focus_handle(cx), cx);
                    input = Some(state.clone());
                    cx.new(|_| Composer(state))
                })
                .unwrap()
                .0
            });
        let input = input.unwrap();
        let value = |cx: &mut TestAppContext| input.read_with(cx, |s, _| s.value().to_string());
        cx.update_window(handle, |_, window, cx| {
            input.update(cx, |s, cx| s.set_value("1. milk", window, cx));
            input.update(cx, |s, cx| s.set_selected_range(7..7, cx));
        })
        .unwrap();
        cx.simulate_keystrokes(handle, "shift-enter");
        assert_eq!(value(cx), "1. milk\n2. ");
        cx.simulate_keystrokes(handle, "shift-enter");
        assert_eq!(value(cx), "1. milk\n");
        cx.simulate_keystrokes(handle, "shift-enter");
        assert_eq!(value(cx), "1. milk\n\n", "after the list, a plain new line");
        // In the middle of an item: the rest goes to the next item.
        cx.update_window(handle, |_, window, cx| {
            input.update(cx, |s, cx| s.set_value("- eggs and ham", window, cx));
            input.update(cx, |s, cx| s.set_selected_range(6..6, cx));
        })
        .unwrap();
        cx.simulate_keystrokes(handle, "shift-enter");
        assert_eq!(value(cx), "- eggs\n- and ham");
        cx.simulate_keystrokes(handle, "cmd-z");
        assert_eq!(value(cx), "- eggs and ham", "Undo takes the new item back");
    }
}
