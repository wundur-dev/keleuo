//! Subagents in the chat: their cards, the strip of running ones, and the view watching one.

use super::*;

impl ChatPanel {
    /// Show a subagent's messages (from the sidebar tree or the strip).
    pub fn expand_agent(&mut self, tool_use_id: &str, cx: &mut Context<Self>) {
        self.toggled.insert(format!("agent:{tool_use_id}"));
        self.list.remeasure();
        cx.notify();
    }

    pub(super) fn agent_open(&self, tool_use_id: &str) -> bool {
        self.toggled.contains(&format!("agent:{tool_use_id}"))
    }

    pub(super) fn toggle_agent(&mut self, tool_use_id: &str, cx: &mut Context<Self>) {
        let key = format!("agent:{tool_use_id}");
        if !self.toggled.remove(&key) {
            self.toggled.insert(key);
        }
        // A card changed height: measure again (the scroll position stays on its item).
        self.list.remeasure();
        cx.notify();
    }

    /// One subagent: a compact row (status, description, type, elapsed, current tool, tokens);
    /// its own messages below when opened. `card` draws it as a card in the chat.
    pub(super) fn render_agent(&self, agent: &SubAgent, card: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, border, hover, green, red, violet) = (
            theme.foreground,
            theme.muted_foreground,
            theme.border,
            theme.secondary_hover,
            theme.green,
            theme.red,
            theme.magenta,
        );
        let open = self.agent_open(&agent.tool_use_id);
        let id = agent.tool_use_id.clone();
        let status = match agent.status {
            AgentStatus::Running => self.spinner(violet).xsmall().into_any_element(),
            AgentStatus::Done => Icon::new(IconName::Check).xsmall().text_color(green).into_any_element(),
            AgentStatus::Failed => Icon::new(IconName::CircleX).xsmall().text_color(red).into_any_element(),
        };
        let mut facts = Vec::new();
        if let Some(e) = agent.elapsed() {
            facts.push(format_elapsed(e));
        }
        if let Some(t) = &agent.current_tool {
            facts.push(t.clone());
        } else if let Some(n) = agent.tool_uses {
            facts.push(format!("{n} tools"));
        }
        if let Some(t) = agent.total_tokens {
            facts.push(format!("{:.1}k tok", t as f64 / 1000.0));
        }
        let title = agent.title();
        let header = div()
            .id(SharedString::from(format!(
                "agent-h-{}-{card}",
                agent.tool_use_id
            )))
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
                    .text_color(violet)
                    .child("agent"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(fg)
                    .child(title),
            )
            .when(!agent.kind.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_color(muted)
                        .child(agent.kind.clone()),
                )
            })
            .child(div().flex_none().text_color(muted).child(facts.join(" · ")))
            .on_click(cx.listener(move |p, _, _, cx| p.toggle_agent(&id, cx)));
        let mut el = div()
            .flex()
            .flex_col()
            .border_l_2()
            .border_color(violet.opacity(0.7))
            .when(card, |d| {
                d.border_1()
                    .border_color(border)
                    .rounded_md()
                    .bg(violet.opacity(0.05))
            })
            .overflow_hidden()
            .child(header);
        if open {
            let mut body = div()
                .flex()
                .flex_col()
                .gap_1()
                .px_2()
                .py_1()
                .border_t_1()
                .border_color(border);
            if agent.items.is_empty() {
                body = body.child(div().text_xs().text_color(muted).child("No messages yet."));
            }
            for child in self.render_items(&agent.items.clone(), &agent.tool_use_id.clone(), cx) {
                body = body.child(child);
            }
            if let Some(summary) = &agent.summary {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("Result: {}", one_line(summary, 400))),
                );
            }
            el = el.child(if card {
                body.into_any_element()
            } else {
                div()
                    .max_h(px(260.))
                    .id(SharedString::from(format!("agent-b-{}", agent.tool_use_id)))
                    .overflow_y_scroll()
                    .child(body)
                    .into_any_element()
            });
        }
        el.into_any_element()
    }

    /// A watched subagent: its description and state, then its items as they arrive. No
    /// message box: it is the subagent's work, not a conversation to write into.
    pub(super) fn render_agent_view(&mut self, id: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let (muted, border, bg) = (theme.muted_foreground, theme.border, theme.background);
        let (title, state) = match self.session.read(cx).chat.agent(id) {
            Some(a) => {
                let status = match a.status {
                    AgentStatus::Running => "running",
                    AgentStatus::Done => "done",
                    AgentStatus::Failed => "failed",
                };
                let mut parts = vec![a.kind.clone(), status.to_string()];
                if let Some(tool) = &a.current_tool {
                    parts.push(format!("using {tool}"));
                }
                if let Some(tokens) = a.total_tokens {
                    parts.push(format!("{tokens} tokens"));
                }
                (a.title(), parts.join(" · "))
            }
            None => ("Subagent".to_string(), "no longer in this conversation".to_string()),
        };
        if self.item_count(cx) != self.synced_items {
            self.sync_rows(cx);
        }
        let empty = self.list_len == 0;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(bg)
            .track_focus(&self.focus)
            .on_action(cx.listener(|p, _: &CopyConversation, window, cx| p.copy_conversation(window, cx)))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(border)
                    .child(div().text_sm().truncate().child(title))
                    .child(div().text_xs().text_color(muted).truncate().child(state)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .when(empty, |d| d.p_3().text_xs().text_color(muted).child("Nothing from it yet."))
                    .when(!empty, |d| {
                        d.child(
                            list(self.list.clone(), cx.processor(|this, ix, _window, cx| this.render_entry(ix, cx)))
                                .size_full(),
                        )
                    }),
            )
    }
}
