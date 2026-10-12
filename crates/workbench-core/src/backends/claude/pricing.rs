//! An estimate of what a Claude Code session costs, from the token counts in its session file
//! and Anthropic's published per-token prices. Claude Code's own interface does not write the
//! cost to the file, so the window works it out; it is an estimate (the prices below are the
//! first-party API rates; a subscription is billed differently).

use std::collections::HashMap;

use serde_json::Value;

/// US dollars per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    /// Writing the prompt cache (five-minute cache: 1.25 times input).
    pub cache_write: f64,
}

const fn price(input: f64, output: f64, cache_read: f64) -> Price {
    Price { input, output, cache_read, cache_write: input * 1.25 }
}

/// The price of a model id (`claude-opus-5-5`, `claude-haiku-4-5-20251001`, ...), by family and
/// version. `None` for a model not in the table.
pub fn price_of(model: &str) -> Option<Price> {
    let m = model.to_ascii_lowercase();
    let has = |s: &str| m.contains(s);
    Some(if has("fable") || has("mythos") {
        if has("5-1") { price(10.0, 50.0, 0.25) } else { price(10.0, 50.0, 1.0) }
    } else if has("opus") {
        if has("opus-5-5") { price(4.0, 20.0, 0.20) } else if has("opus-4-1") || has("opus-4-0") || has("opus-4-2025") || has("3-opus") { price(15.0, 75.0, 1.5) } else { price(5.0, 25.0, 0.50) }
    } else if has("sonnet") {
        if has("sonnet-5") { price(2.0, 10.0, 0.20) } else { price(3.0, 15.0, 0.30) }
    } else if has("haiku") {
        if has("3-haiku") { price(0.25, 1.25, 0.03) } else if has("3-5-haiku") { price(0.80, 4.0, 0.08) } else { price(1.0, 5.0, 0.10) }
    } else {
        return None;
    })
}

/// The cost of one message's `usage` with `model`'s prices.
pub fn cost(model: &str, usage: &Value) -> Option<f64> {
    let p = price_of(model)?;
    let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0) as f64;
    Some(
        (n("input_tokens") * p.input
            + n("output_tokens") * p.output
            + n("cache_read_input_tokens") * p.cache_read
            + n("cache_creation_input_tokens") * p.cache_write)
            / 1_000_000.0,
    )
}

/// Adds up a session file's assistant messages. Claude Code writes one line per content block,
/// each repeating its message's usage, so each message counts once (at its largest).
#[derive(Default)]
pub struct Tally {
    seen: HashMap<String, f64>,
}

impl Tally {
    /// What this line adds to the total (zero for lines without usage, or seen already).
    pub fn add(&mut self, line: &Value) -> f64 {
        if line.get("type").and_then(Value::as_str) != Some("assistant") {
            return 0.0;
        }
        let Some(message) = line.get("message") else { return 0.0 };
        let (Some(model), Some(usage)) = (message.get("model").and_then(Value::as_str), message.get("usage")) else {
            return 0.0;
        };
        let Some(c) = cost(model, usage) else { return 0.0 };
        let id = message.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let before = self.seen.get(&id).copied().unwrap_or(0.0);
        if c <= before {
            return 0.0;
        }
        self.seen.insert(id, c);
        c - before
    }
}

/// Whether Claude Code is billed per use, told from what it would use to sign in, without
/// reading any credential: an API key or token in the environment (`local`: this process's
/// environment is the agent's), a cloud provider switched on, or an `apiKeyHelper` or such a
/// variable in a settings file (`settings`: their text; only the names are looked at).
/// Otherwise it signs in with a subscription plan.
pub fn billed_per_use(local: bool, env: &dyn Fn(&str) -> bool, settings: &[String]) -> bool {
    const VARS: [&str; 5] =
        ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_USE_BEDROCK", "CLAUDE_CODE_USE_VERTEX", "CLAUDE_CODE_USE_FOUNDRY"];
    if local && VARS.iter().any(|v| env(v)) {
        return true;
    }
    settings.iter().any(|text| {
        let Ok(v) = serde_json::from_str::<Value>(text) else { return false };
        v.get("apiKeyHelper").is_some_and(|h| !h.is_null())
            || v.get("env").and_then(Value::as_object).is_some_and(|e| VARS.iter().any(|k| e.contains_key(*k)))
    })
}

/// The context window of a model in Claude Code, in tokens: a million for models run with
/// the `[1m]` suffix and for the current Opus, Sonnet and Fable families, 200,000 otherwise.
pub fn context_window(model: &str) -> u64 {
    let m = model.to_ascii_lowercase();
    let million = m.contains("[1m]")
        || ["fable", "mythos", "opus-5", "sonnet-5", "opus-4-6", "opus-4-7", "opus-4-8", "sonnet-4-6"]
            .iter()
            .any(|f| m.contains(f));
    if million { 1_000_000 } else { 200_000 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn billing_is_told_without_credentials() {
        let none = |_: &str| false;
        assert!(!billed_per_use(true, &none, &["{\"model\": \"opus\"}".into()]));
        assert!(billed_per_use(true, &|k: &str| k == "ANTHROPIC_API_KEY", &[]));
        // The environment of this process says nothing about a remote host.
        assert!(!billed_per_use(false, &|k: &str| k == "ANTHROPIC_API_KEY", &[]));
        assert!(billed_per_use(false, &none, &["{\"apiKeyHelper\": \"~/bin/key\"}".into()]));
        assert!(billed_per_use(true, &none, &["{\"env\": {\"ANTHROPIC_API_KEY\": \"x\"}}".into()]));
    }

    #[test]
    fn context_windows() {
        assert_eq!(context_window("claude-haiku-4-5"), 200_000);
        assert_eq!(context_window("claude-opus-5-5"), 1_000_000);
        assert_eq!(context_window("claude-sonnet-4-5[1m]"), 1_000_000);
    }

    #[test]
    fn prices_by_family() {
        assert_eq!(price_of("claude-opus-5-5").unwrap().input, 4.0);
        assert_eq!(price_of("claude-sonnet-4-6").unwrap().output, 15.0);
        assert_eq!(price_of("claude-haiku-4-5-20251001").unwrap().input, 1.0);
        assert_eq!(price_of("claude-fable-5-1").unwrap().cache_read, 0.25);
        assert!(price_of("<synthetic>").is_none());
    }

    #[test]
    fn a_message_counts_once() {
        let line = |out: u64| {
            json!({"type": "assistant", "message": {"id": "m1", "model": "claude-haiku-4-5",
                "usage": {"input_tokens": 1000, "output_tokens": out, "cache_read_input_tokens": 10000,
                          "cache_creation_input_tokens": 0}}})
        };
        let mut t = Tally::default();
        let first = t.add(&line(100));
        // 1000 * 1 + 100 * 5 + 10000 * 0.1 = 2500 per million.
        assert!((first - 0.0025).abs() < 1e-12);
        assert_eq!(t.add(&line(100)), 0.0);
        assert!((t.add(&line(200)) - 0.0005).abs() < 1e-12);
    }
}
