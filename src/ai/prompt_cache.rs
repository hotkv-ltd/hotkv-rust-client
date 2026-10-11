// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! [`PromptCache`]: a get-or-call cache in front of `PCACHE.*`, with a
//! canonical cache key that is byte-identical across every official
//! HotKV SDK for the same logical request.

use std::future::Future;

use sha2::{Digest, Sha256};

use super::{LlmStatsRecordOptions, Usage};
use crate::client::Client;
use crate::error::{Error, Result};

/// One chat style message, used only to build a [`PromptCache`] key. No
/// dependency on any LLM provider's SDK or wire format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMessage {
    pub role: String,
    pub content: String,
}

impl PromptMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        PromptMessage { role: role.into(), content: content.into() }
    }
}

/// The normalized shape a [`PromptCache`] key is derived from. `params`
/// values should be pre-formatted to strings by the caller (`"0.7"`, not
/// `0.7`) so the same logical request hashes to the same key regardless
/// of language or float formatting; keys are sorted by byte value before
/// hashing, so callers may pass them in any order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptCacheKeyInput {
    pub provider: String,
    pub model: String,
    /// The system prompt; `""` if there is none (never omitted from the
    /// canonical form).
    pub system: String,
    pub messages: Vec<PromptMessage>,
    pub params: Vec<(String, String)>,
}

/// Builds the canonical, compact JSON encoding `input` hashes to. See the
/// SDK specification section 4 for the exact rules; every official HotKV
/// SDK implements this identically so cache keys are shareable across
/// languages.
pub fn canonical_json(input: &PromptCacheKeyInput) -> String {
    let mut out = String::with_capacity(128 + input.messages.len() * 32 + input.params.len() * 16);
    out.push('{');

    out.push_str("\"provider\":");
    push_json_string(&mut out, &input.provider);
    out.push_str(",\"model\":");
    push_json_string(&mut out, &input.model);
    out.push_str(",\"system\":");
    push_json_string(&mut out, &input.system);

    out.push_str(",\"messages\":[");
    for (i, m) in input.messages.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"role\":");
        push_json_string(&mut out, &m.role);
        out.push_str(",\"content\":");
        push_json_string(&mut out, &m.content);
        out.push('}');
    }
    out.push(']');

    out.push_str(",\"params\":{");
    let mut sorted: Vec<&(String, String)> = input.params.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for (i, (k, v)) in sorted.into_iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_json_string(&mut out, k);
        out.push(':');
        push_json_string(&mut out, v);
    }
    out.push('}');

    out.push('}');
    out
}

/// Compact, standard escaped JSON string literal (quotes included).
/// Non-ASCII characters are written as literal UTF-8, matching Python's
/// `json.dumps(..., ensure_ascii=False)`.
fn push_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The `PromptCache` key: the lowercase hex SHA-256 digest of
/// [`canonical_json`]'s UTF-8 bytes.
pub fn prompt_cache_key(input: &PromptCacheKeyInput) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_json(input).as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for b in digest.iter() {
        hex.push_str(&format!("{:02x}", b));
    }
    hex
}

/// Token/cost/latency accounting for one LLM call, returned by the
/// caller's model function on a `PromptCache`/`SemanticCache` miss.
/// `model`, when non-empty, does two things: it enables
/// `LLMSTATS.RECORD` based accounting ([`PromptCache::with_stats_bucket`])
/// to price the call from the server's price table, and it is attached
/// to the stored cache entry (the `PCACHE.SET`/`SCACHE.SET` usage group,
/// see [`Usage`]) so a later hit attributes the tokens and cost that hit
/// did not have to spend.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmUsage {
    /// Empty when this call's usage should not be recorded anywhere
    /// (see [`LlmUsage::as_usage`]).
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Provider prompt-cache hit tokens.
    pub cache_read_tokens: u64,
    /// Anthropic cache-write tokens, 5-minute TTL tier.
    pub cache_write_5m_tokens: u64,
    /// Anthropic cache-write tokens, 1-hour TTL tier.
    pub cache_write_1h_tokens: u64,
    pub cost_usd: f64,
    pub latency_ms: i64,
}

impl LlmUsage {
    /// This usage as a cache-entry [`Usage`] group, or `None` when
    /// [`LlmUsage::model`] is empty (`Usage` requires one).
    pub fn as_usage(&self) -> Option<Usage> {
        if self.model.is_empty() {
            return None;
        }
        Some(
            Usage::new(&self.model)
                .input(self.input_tokens)
                .output(self.output_tokens)
                .cache_read(self.cache_read_tokens)
                .cache_write_5m(self.cache_write_5m_tokens)
                .cache_write_1h(self.cache_write_1h_tokens),
        )
    }
}

/// What the caller supplied function passed to [`PromptCache::get_or_call`]
/// must return on a cache miss.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmCallResult {
    pub response: String,
    pub usage: LlmUsage,
}

/// An idiomatic get-or-call cache in front of `PCACHE.*`: on a hit it
/// returns the stored response without invoking the LLM; on a miss it
/// calls the caller's function, stores the result, and returns it. Needs
/// the `prompt-cache` license feature; a `NOLICENSE` error is treated as
/// "the cache is unavailable", not as a call failure, so
/// [`PromptCache::get_or_call`] falls through to calling the LLM.
pub struct PromptCache {
    client: Client,
    ns: String,
    ttl_seconds: Option<i64>,
    stats_bucket: Option<String>,
}

impl PromptCache {
    /// A `PromptCache` storing entries in namespace `ns`, with the given
    /// default TTL (`None` for no expiry). Call
    /// [`PromptCache::create_namespace`] once before first use.
    pub fn new(client: Client, ns: impl Into<String>, ttl_seconds: Option<i64>) -> Self {
        PromptCache { client, ns: ns.into(), ttl_seconds, stats_bucket: None }
    }

    /// Enables `LLMSTATS.RECORD` accounting on every
    /// [`PromptCache::get_or_call`] (hit or miss) against `bucket`. The
    /// bucket is created automatically on first use.
    pub fn with_stats_bucket(mut self, bucket: impl Into<String>) -> Self {
        self.stats_bucket = Some(bucket.into());
        self
    }

    /// Ensures the `PCACHE` namespace backing this cache exists.
    /// `max_entries: None` uses the engine default; `cost_per_hit_usd`
    /// feeds `PCACHE.STATS`' `cost_saved_usd`.
    pub async fn create_namespace(&self, max_entries: Option<i64>, cost_per_hit_usd: Option<f64>) -> Result<()> {
        match self.client.pcache().create(&self.ns, max_entries, self.ttl_seconds, cost_per_hit_usd).await {
            Ok(()) => Ok(()),
            Err(Error::Server(e)) if e.message.contains("already exists") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Returns the cached response for `input` if present; otherwise
    /// calls `f`, caches its response, and returns that. The returned
    /// `bool` is whether the response came from cache.
    pub async fn get_or_call<F, Fut>(&self, input: &PromptCacheKeyInput, f: F) -> Result<(String, bool)>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<LlmCallResult>>,
    {
        let key = prompt_cache_key(input);

        match self.client.pcache().get(&self.ns, &key).await {
            Ok(Some(response)) => {
                self.record(&LlmUsage::default(), true, 0).await;
                return Ok((response, true));
            }
            Ok(None) => {}
            Err(e) if e.is_not_licensed() => {} // cache unavailable, not the LLM: fall through
            Err(e) => return Err(e),
        }

        let result = f().await?;
        // Best effort: a caching failure should not fail the caller's
        // request when the LLM call itself already succeeded.
        let _ = self
            .client
            .pcache()
            .set(&self.ns, &key, &result.response, self.ttl_seconds, None, result.usage.as_usage().as_ref())
            .await;
        self.record(&result.usage, false, result.usage.latency_ms).await;
        Ok((result.response, false))
    }

    /// The current `PCACHE.STATS` for this cache's namespace, including
    /// how many tokens and how much cost recorded usage has saved on
    /// hits (see [`crate::ai::UsageSavings`]).
    pub async fn stats(&self) -> Result<crate::ai::PcacheStats> {
        self.client.pcache().stats(&self.ns).await
    }

    async fn record(&self, usage: &LlmUsage, hit: bool, latency_ms: i64) {
        let Some(bucket) = self.stats_bucket.clone() else { return };
        let _ = self.client.llmstats().bucket(&bucket).await;
        let (input_tokens, output_tokens, cached_tokens, cost_usd) = if hit {
            (0, 0, usage.input_tokens as i64 + usage.output_tokens as i64, 0.0)
        } else {
            (usage.input_tokens as i64, usage.output_tokens as i64, 0, usage.cost_usd)
        };
        let mut opts = LlmStatsRecordOptions::default();
        if !hit && !usage.model.is_empty() {
            opts.model = Some(usage.model.clone());
            opts.cache_write_5m = usage.cache_write_5m_tokens;
            opts.cache_write_1h = usage.cache_write_1h_tokens;
        }
        let _ = self.client.llmstats().record(&bucket, input_tokens, output_tokens, cached_tokens, cost_usd, latency_ms, Some(&opts)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PromptCacheKeyInput {
        PromptCacheKeyInput {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-5".to_string(),
            system: "You are terse.".to_string(),
            messages: vec![PromptMessage::new("user", "hello")],
            params: vec![("temperature".to_string(), "0.7".to_string()), ("top_p".to_string(), "1".to_string())],
        }
    }

    #[test]
    fn canonical_form_is_compact_and_sorted() {
        let json = canonical_json(&sample());
        assert_eq!(
            json,
            "{\"provider\":\"anthropic\",\"model\":\"claude-sonnet-5\",\"system\":\"You are terse.\",\
             \"messages\":[{\"role\":\"user\",\"content\":\"hello\"}],\
             \"params\":{\"temperature\":\"0.7\",\"top_p\":\"1\"}}"
        );
    }

    #[test]
    fn missing_optionals_stay_present() {
        let input = PromptCacheKeyInput {
            provider: "openai".to_string(),
            model: "gpt-6-sol".to_string(),
            system: String::new(),
            messages: vec![],
            params: vec![],
        };
        let json = canonical_json(&input);
        assert_eq!(json, "{\"provider\":\"openai\",\"model\":\"gpt-6-sol\",\"system\":\"\",\"messages\":[],\"params\":{}}");
    }

    #[test]
    fn params_sort_by_byte_value_regardless_of_input_order() {
        let mut a = sample();
        a.params = vec![("z".to_string(), "1".to_string()), ("a".to_string(), "2".to_string())];
        let mut b = sample();
        b.params = vec![("a".to_string(), "2".to_string()), ("z".to_string(), "1".to_string())];
        assert_eq!(prompt_cache_key(&a), prompt_cache_key(&b));
    }

    #[test]
    fn escapes_control_characters_and_keeps_non_ascii_literal() {
        let mut input = sample();
        input.messages = vec![PromptMessage::new("user", "line1\nline2\t\"quoted\"\u{0007}caf\u{e9}")];
        let json = canonical_json(&input);
        assert!(json.contains("line1\\nline2\\t\\\"quoted\\\"\\u0007caf\u{e9}"));
    }

    #[test]
    fn key_is_64_char_lowercase_hex() {
        let key = prompt_cache_key(&sample());
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn key_is_deterministic() {
        assert_eq!(prompt_cache_key(&sample()), prompt_cache_key(&sample()));
    }

    #[test]
    fn different_messages_yield_different_keys() {
        let mut other = sample();
        other.messages[0].content = "goodbye".to_string();
        assert_ne!(prompt_cache_key(&sample()), prompt_cache_key(&other));
    }
}
