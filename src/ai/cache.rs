// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! AI cache command families: `SCACHE.*`, `PCACHE.*`, the four identical
//! keyed cache engines (`TOOL.CACHE.*`/`NCACHE.*`/`GUARD.*`/`RERANK.*`),
//! `LLMSTATS.*` and `LLMBUDGET.*`.

use std::collections::HashMap;

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::value::{field_bool_int, field_f64, field_i64, field_string, named_fields, FromValue, Value};

// ---------------------------------------------------------- Token usage --

/// The shared "token usage group" wire option:
/// `MODEL model [INPUT n] [OUTPUT n] [CACHEREAD n] [CACHEWRITE5M n]
/// [CACHEWRITE1H n]`, accepted by [`ScacheCommands::set`],
/// [`PcacheCommands::set`], every keyed cache
/// [`KeyedCacheCommands::set`], and [`LlmBudgetCommands::consume`].
///
/// `model` is required; the token counts default to `0` and are omitted
/// from the wire when zero, but `MODEL` is always sent once a `Usage` is
/// attached, so the server can price the call and, on a later cache hit,
/// attribute the tokens (and cost) that hit did not spend.
///
/// `input`/`output` are uncached prompt/output tokens, `cache_read` is
/// provider prompt-cache hit tokens, and `cache_write_5m`/`cache_write_1h`
/// are Anthropic cache-write tokens by TTL tier (leave both `0` for
/// providers without a separate cache-write cost, e.g. OpenAI).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

impl Usage {
    /// A new usage value for `model`, every token count starting at `0`.
    pub fn new(model: impl Into<String>) -> Self {
        Usage { model: model.into(), ..Default::default() }
    }

    pub fn input(mut self, tokens: u64) -> Self {
        self.input = tokens;
        self
    }

    pub fn output(mut self, tokens: u64) -> Self {
        self.output = tokens;
        self
    }

    pub fn cache_read(mut self, tokens: u64) -> Self {
        self.cache_read = tokens;
        self
    }

    pub fn cache_write_5m(mut self, tokens: u64) -> Self {
        self.cache_write_5m = tokens;
        self
    }

    pub fn cache_write_1h(mut self, tokens: u64) -> Self {
        self.cache_write_1h = tokens;
        self
    }
}

/// When an AI cache entry, conversation or working memory key expires:
/// a time to live in seconds from now (`TTL`), or the Unix time in
/// milliseconds it expires at (`PXAT`). The server accepts one or the other,
/// never both, on `SCACHE.SET`, `PCACHE.SET`, the keyed cache `SET`,
/// `AMEM.CONV.CREATE` and `AMEM.WORK.SET`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    /// `TTL seconds`.
    Ttl(i64),
    /// `PXAT unix-ms`.
    Pxat(i64),
}

impl Expiry {
    /// Appends the `TTL`/`PXAT` option to `cmd` when `expiry` is `Some`.
    pub(crate) fn append(expiry: Option<Expiry>, cmd: Cmd) -> Cmd {
        match expiry {
            None => cmd,
            Some(Expiry::Ttl(s)) => cmd.kw_opt("TTL", Some(s)),
            Some(Expiry::Pxat(ms)) => cmd.kw_opt("PXAT", Some(ms)),
        }
    }
}

fn non_zero(v: u64) -> Option<u64> {
    (v > 0).then_some(v)
}

/// Appends `usage`'s wire options to `cmd`; a no-op when `usage` is
/// `None` (the command then behaves exactly as if the usage group did
/// not exist).
fn append_usage(mut cmd: Cmd, usage: Option<&Usage>) -> Cmd {
    let Some(u) = usage else { return cmd };
    cmd = cmd.arg("MODEL").arg(&u.model);
    cmd = cmd.kw_opt("INPUT", non_zero(u.input));
    cmd = cmd.kw_opt("OUTPUT", non_zero(u.output));
    cmd = cmd.kw_opt("CACHEREAD", non_zero(u.cache_read));
    cmd = cmd.kw_opt("CACHEWRITE5M", non_zero(u.cache_write_5m));
    cmd = cmd.kw_opt("CACHEWRITE1H", non_zero(u.cache_write_1h));
    cmd
}

/// The tokens (and, once the model is priced, the cost) a cache's hits
/// did not have to spend, accumulated from usage recorded on a `SET`
/// call (see [`Usage`]). Every field is zero when no `SET` ever carried
/// a usage value. Reported by [`ScacheCommands::stats`],
/// [`PcacheCommands::stats`] and [`KeyedCacheCommands::stats`].
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageSavings {
    pub requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub cost_usd: f64,
}

fn parse_usage_savings(m: &HashMap<String, Value>) -> UsageSavings {
    UsageSavings {
        requests: field_i64(m, "usage_saved_requests"),
        input_tokens: field_i64(m, "usage_saved_input_tokens"),
        output_tokens: field_i64(m, "usage_saved_output_tokens"),
        cache_read_tokens: field_i64(m, "usage_saved_cache_read_tokens"),
        cache_write_tokens: field_i64(m, "usage_saved_cache_write_tokens"),
        cost_usd: field_f64(m, "usage_saved_cost_usd"),
    }
}

// -------------------------------------------------------------- SCACHE --

/// `SCACHE.STATS` fields. Parsed by field name (RESP3 map or flat RESP2
/// array): a missing field defaults to zero and an extra unknown one is
/// ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ScacheStats {
    pub entries: i64,
    pub hits: i64,
    pub misses: i64,
    pub hit_rate: f64,
    pub p50_us: i64,
    pub usage_saved: UsageSavings,
}

impl FromValue for ScacheStats {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(ScacheStats {
            entries: field_i64(&m, "entries"),
            hits: field_i64(&m, "hits"),
            misses: field_i64(&m, "misses"),
            hit_rate: field_f64(&m, "hit_rate"),
            p50_us: field_i64(&m, "p50_us"),
            usage_saved: parse_usage_savings(&m),
        })
    }
}

/// Builds the `SCACHE.SET` command; see [`ScacheCommands::set`].
fn build_scache_set_cmd(ns: impl ToArg, query_text: impl ToArg, response: impl ToArg, vector: &[f64], expiry: Option<Expiry>, usage: Option<&Usage>) -> Cmd {
    let cmd = Cmd::new("SCACHE.SET")
        .arg(ns)
        .arg(query_text)
        .arg(response)
        .args_f32(vector);
    append_usage(Expiry::append(expiry, cmd), usage)
}

/// Builds the `SCACHE.CREATE` command; the threshold is narrowed to the 32
/// bit float the server reads.
fn build_scache_create_cmd(ns: impl ToArg, dims: i64, metric: Option<&str>, threshold: Option<f64>, ttl_seconds: Option<i64>) -> Cmd {
    Cmd::new("SCACHE.CREATE")
        .arg(ns)
        .arg(dims)
        .kw_opt("METRIC", metric)
        .kw_f32("THRESHOLD", threshold)
        .kw_opt("TTL", ttl_seconds)
}

/// `SCACHE.*` (semantic cache) commands. Reach it with [`Client::scache`].
pub struct ScacheCommands(pub(crate) Client);

impl ScacheCommands {
    /// `SCACHE.CREATE ns dims [METRIC m] [THRESHOLD t] [TTL s]`.
    pub async fn create(&self, ns: impl ToArg, dims: i64, metric: Option<&str>, threshold: Option<f64>, ttl_seconds: Option<i64>) -> Result<()> {
        self.0.call(build_scache_create_cmd(ns, dims, metric, threshold, ttl_seconds)).await
    }

    /// `SCACHE.SET ns query_text response val1 ... valN [TTL s]
    /// [usage group]`. `usage` records the original (uncached) model
    /// call's token usage with this entry, so a later `SCACHE.GET` hit
    /// attributes the tokens (and, once the model is priced, the cost)
    /// to `stats().usage_saved`; see [`Usage`].
    pub async fn set(&self, ns: impl ToArg, query_text: impl ToArg, response: impl ToArg, vector: &[f64], ttl_seconds: Option<i64>, usage: Option<&Usage>) -> Result<()> {
        self.0.call(build_scache_set_cmd(ns, query_text, response, vector, ttl_seconds.map(Expiry::Ttl), usage)).await
    }

    /// Like [`ScacheCommands::set`], but the entry expires after a time to
    /// live or at a Unix time in milliseconds (`PXAT`), as `expiry` says.
    /// `None` leaves the namespace default in force.
    pub async fn set_with_expiry(&self, ns: impl ToArg, query_text: impl ToArg, response: impl ToArg, vector: &[f64], expiry: Option<Expiry>, usage: Option<&Usage>) -> Result<()> {
        self.0.call(build_scache_set_cmd(ns, query_text, response, vector, expiry, usage)).await
    }

    /// `SCACHE.GET ns val1 ... valN [THRESHOLD override]` ->
    /// `(response, similarity)` on a hit above the threshold.
    pub async fn get(&self, ns: impl ToArg, vector: &[f64], threshold_override: Option<f64>) -> Result<Option<(String, f64)>> {
        let cmd = Cmd::new("SCACHE.GET").arg(ns).args_f32(vector).kw_f32("THRESHOLD", threshold_override);
        let v = self.0.command_idempotent(cmd).await?;
        if v.is_nil() {
            return Ok(None);
        }
        <(String, f64)>::from_value(v).map(Some)
    }

    /// `SCACHE.STATS ns`.
    pub async fn stats(&self, ns: impl ToArg) -> Result<ScacheStats> {
        self.0.call_ro(Cmd::new("SCACHE.STATS").arg(ns)).await
    }

    /// `SCACHE.FLUSH ns`.
    pub async fn flush(&self, ns: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("SCACHE.FLUSH").arg(ns)).await
    }

    /// `SCACHE.DROP ns`.
    pub async fn drop(&self, ns: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("SCACHE.DROP").arg(ns)).await
    }

    /// `SCACHE.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("SCACHE.LIST")).await
    }

    /// `SCACHE.THRESHOLD ns value`.
    pub async fn set_threshold(&self, ns: impl ToArg, value: f64) -> Result<()> {
        self.0.call(Cmd::new("SCACHE.THRESHOLD").arg(ns).arg_f32(value)).await
    }
}

// -------------------------------------------------------------- PCACHE --

/// `PCACHE.STATS` fields. Parsed by field name (RESP3 map or flat RESP2
/// array): a missing field defaults to zero and an extra unknown one is
/// ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PcacheStats {
    pub entries: i64,
    pub hits: i64,
    pub misses: i64,
    pub evictions: i64,
    pub memory_bytes: i64,
    pub hit_rate: f64,
    pub p50_ns: i64,
    pub p95_ns: i64,
    pub p99_ns: i64,
    pub cost_saved_usd: f64,
    pub usage_saved: UsageSavings,
}

impl FromValue for PcacheStats {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(PcacheStats {
            entries: field_i64(&m, "entries"),
            hits: field_i64(&m, "hits"),
            misses: field_i64(&m, "misses"),
            evictions: field_i64(&m, "evictions"),
            memory_bytes: field_i64(&m, "memory_bytes"),
            hit_rate: field_f64(&m, "hit_rate"),
            p50_ns: field_i64(&m, "p50_ns"),
            p95_ns: field_i64(&m, "p95_ns"),
            p99_ns: field_i64(&m, "p99_ns"),
            cost_saved_usd: field_f64(&m, "cost_saved_usd"),
            usage_saved: parse_usage_savings(&m),
        })
    }
}

/// A `PCACHE.GETEX` hit.
#[derive(Debug, Clone, PartialEq)]
pub struct PcacheEntry {
    pub response: String,
    pub metadata: String,
    pub hit_count: i64,
}

impl FromValue for PcacheEntry {
    fn from_value(value: Value) -> Result<Self> {
        let mut items = value.into_items()?;
        if items.len() != 3 {
            return Err(Error::Protocol(format!("expected 3 elements in a PCACHE.GETEX reply, got {}", items.len())));
        }
        let hit_count = i64::from_value(items.pop().unwrap())?;
        let metadata = String::from_value(items.pop().unwrap())?;
        let response = String::from_value(items.pop().unwrap())?;
        Ok(PcacheEntry { response, metadata, hit_count })
    }
}

/// Builds the `PCACHE.SET` command; see [`PcacheCommands::set`].
fn build_pcache_set_cmd(ns: impl ToArg, prompt: impl ToArg, response: impl ToArg, expiry: Option<Expiry>, meta_json: Option<&str>, usage: Option<&Usage>) -> Cmd {
    let cmd = Cmd::new("PCACHE.SET")
        .arg(ns)
        .arg(prompt)
        .arg(response);
    let cmd = Expiry::append(expiry, cmd).kw_opt("META", meta_json);
    append_usage(cmd, usage)
}

/// Builds the `PCACHE.CREATE` command.
fn build_pcache_create_cmd(ns: impl ToArg, max_entries: Option<i64>, ttl_seconds: Option<i64>, cost_per_hit_usd: Option<f64>) -> Cmd {
    Cmd::new("PCACHE.CREATE")
        .arg(ns)
        .kw_opt("MAX_ENTRIES", max_entries)
        .kw_opt("TTL", ttl_seconds)
        .kw_non_negative("COST_PER_HIT", cost_per_hit_usd)
}

/// `PCACHE.*` (prompt/LLM response cache) commands. Reach it with
/// [`Client::pcache`].
pub struct PcacheCommands(pub(crate) Client);

impl PcacheCommands {
    /// `PCACHE.CREATE ns [MAX_ENTRIES n] [TTL s] [COST_PER_HIT usd]`.
    pub async fn create(&self, ns: impl ToArg, max_entries: Option<i64>, ttl_seconds: Option<i64>, cost_per_hit_usd: Option<f64>) -> Result<()> {
        self.0.call(build_pcache_create_cmd(ns, max_entries, ttl_seconds, cost_per_hit_usd)).await
    }

    /// `PCACHE.SET ns prompt response [TTL s] [META json] [usage group]`.
    /// `usage` records the original (uncached) model call's token usage
    /// with this entry, so a later `PCACHE.GET`/`GETEX` hit attributes
    /// the tokens (and, once the model is priced, the cost) to
    /// `stats().usage_saved`; see [`Usage`].
    pub async fn set(&self, ns: impl ToArg, prompt: impl ToArg, response: impl ToArg, ttl_seconds: Option<i64>, meta_json: Option<&str>, usage: Option<&Usage>) -> Result<()> {
        self.0.call(build_pcache_set_cmd(ns, prompt, response, ttl_seconds.map(Expiry::Ttl), meta_json, usage)).await
    }

    /// Like [`PcacheCommands::set`], but the entry expires after a time to
    /// live or at a Unix time in milliseconds (`PXAT`), as `expiry` says.
    pub async fn set_with_expiry(&self, ns: impl ToArg, prompt: impl ToArg, response: impl ToArg, expiry: Option<Expiry>, meta_json: Option<&str>, usage: Option<&Usage>) -> Result<()> {
        self.0.call(build_pcache_set_cmd(ns, prompt, response, expiry, meta_json, usage)).await
    }

    /// `PCACHE.GET ns prompt`.
    pub async fn get(&self, ns: impl ToArg, prompt: impl ToArg) -> Result<Option<String>> {
        self.0.call_ro(Cmd::new("PCACHE.GET").arg(ns).arg(prompt)).await
    }

    /// `PCACHE.GETEX ns prompt`.
    pub async fn getex(&self, ns: impl ToArg, prompt: impl ToArg) -> Result<Option<PcacheEntry>> {
        self.0.call_ro(Cmd::new("PCACHE.GETEX").arg(ns).arg(prompt)).await
    }

    /// `PCACHE.DEL ns prompt`.
    pub async fn del(&self, ns: impl ToArg, prompt: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("PCACHE.DEL").arg(ns).arg(prompt)).await
    }

    /// `PCACHE.STATS ns`.
    pub async fn stats(&self, ns: impl ToArg) -> Result<PcacheStats> {
        self.0.call_ro(Cmd::new("PCACHE.STATS").arg(ns)).await
    }

    /// `PCACHE.FLUSH ns` -> number of entries flushed.
    pub async fn flush(&self, ns: impl ToArg) -> Result<i64> {
        self.0.call(Cmd::new("PCACHE.FLUSH").arg(ns)).await
    }

    /// `PCACHE.DROP ns`.
    pub async fn drop(&self, ns: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("PCACHE.DROP").arg(ns)).await
    }

    /// `PCACHE.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("PCACHE.LIST")).await
    }

    /// `PCACHE.CONFIG ns SET MAX_ENTRIES|TTL|COST_PER_HIT value`.
    pub async fn config_set(&self, ns: impl ToArg, field: &str, value: impl ToArg) -> Result<()> {
        self.0.call(Cmd::new("PCACHE.CONFIG").arg(ns).arg("SET").arg(field).arg(value)).await
    }
}

// ------------------------------------------------------- Keyed caches --

/// `TOOL.CACHE.*`/`NCACHE.*`/`GUARD.*`/`RERANK.*` fields (identical
/// engine, different namespace/license). Parsed by field name (RESP3 map
/// or flat RESP2 array): a missing field defaults to zero and an extra
/// unknown one is ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KeyedCacheStats {
    pub entries: i64,
    pub hits: i64,
    pub misses: i64,
    pub evictions: i64,
    pub memory_bytes: i64,
    pub usage_saved: UsageSavings,
}

impl FromValue for KeyedCacheStats {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(KeyedCacheStats {
            entries: field_i64(&m, "entries"),
            hits: field_i64(&m, "hits"),
            misses: field_i64(&m, "misses"),
            evictions: field_i64(&m, "evictions"),
            memory_bytes: field_i64(&m, "memory_bytes"),
            usage_saved: parse_usage_savings(&m),
        })
    }
}

/// Builds the `<PREFIX>.SET` command; see [`KeyedCacheCommands::set`].
fn build_keyed_cache_set_cmd(
    prefix: &str,
    ns: impl ToArg,
    key: impl ToArg,
    value: impl ToArg,
    expiry: Option<Expiry>,
    meta_json: Option<&str>,
    usage: Option<&Usage>,
) -> Cmd {
    let cmd = Expiry::append(expiry, Cmd::new(&format!("{}.SET", prefix)).arg(ns).arg(key).arg(value)).kw_opt("META", meta_json);
    append_usage(cmd, usage)
}

/// One of the four identical keyed cache engines. Reach it with
/// [`Client::tool_cache`], [`Client::ncache`], [`Client::guard`] or
/// [`Client::rerank`].
pub struct KeyedCacheCommands {
    client: Client,
    prefix: &'static str,
}

impl KeyedCacheCommands {
    pub(crate) fn new(client: Client, prefix: &'static str) -> Self {
        KeyedCacheCommands { client, prefix }
    }

    fn cmd(&self, sub: &str) -> Cmd {
        Cmd::new(&format!("{}.{}", self.prefix, sub))
    }

    /// `<PREFIX>.CREATE ns [MAX_ENTRIES n] [TTL s]`.
    pub async fn create(&self, ns: impl ToArg, max_entries: Option<i64>, ttl_seconds: Option<i64>) -> Result<()> {
        let cmd = self.cmd("CREATE").arg(ns).kw_opt("MAX_ENTRIES", max_entries).kw_opt("TTL", ttl_seconds);
        self.client.call(cmd).await
    }

    /// `<PREFIX>.SET ns key value [TTL s] [META json] [usage group]`.
    /// `usage` records the original (uncached) model call's token usage
    /// with this entry, so a later `<PREFIX>.GET` hit attributes the
    /// tokens (and, once the model is priced, the cost) to
    /// `stats().usage_saved`; see [`Usage`].
    pub async fn set(&self, ns: impl ToArg, key: impl ToArg, value: impl ToArg, ttl_seconds: Option<i64>, meta_json: Option<&str>, usage: Option<&Usage>) -> Result<()> {
        let cmd = build_keyed_cache_set_cmd(self.prefix, ns, key, value, ttl_seconds.map(Expiry::Ttl), meta_json, usage);
        self.client.call(cmd).await
    }

    /// Like [`KeyedCacheCommands::set`], but the entry expires after a time
    /// to live or at a Unix time in milliseconds (`PXAT`), as `expiry` says.
    pub async fn set_with_expiry(&self, ns: impl ToArg, key: impl ToArg, value: impl ToArg, expiry: Option<Expiry>, meta_json: Option<&str>, usage: Option<&Usage>) -> Result<()> {
        let cmd = build_keyed_cache_set_cmd(self.prefix, ns, key, value, expiry, meta_json, usage);
        self.client.call(cmd).await
    }

    /// `<PREFIX>.GET ns key`.
    pub async fn get(&self, ns: impl ToArg, key: impl ToArg) -> Result<Option<String>> {
        self.client.call_ro(self.cmd("GET").arg(ns).arg(key)).await
    }

    /// `<PREFIX>.DEL ns key`.
    pub async fn del(&self, ns: impl ToArg, key: impl ToArg) -> Result<bool> {
        self.client.call(self.cmd("DEL").arg(ns).arg(key)).await
    }

    /// `<PREFIX>.STATS ns`.
    pub async fn stats(&self, ns: impl ToArg) -> Result<KeyedCacheStats> {
        self.client.call_ro(self.cmd("STATS").arg(ns)).await
    }

    /// `<PREFIX>.FLUSH ns` -> number of entries flushed.
    pub async fn flush(&self, ns: impl ToArg) -> Result<i64> {
        self.client.call(self.cmd("FLUSH").arg(ns)).await
    }

    /// `<PREFIX>.DROP ns`.
    pub async fn drop(&self, ns: impl ToArg) -> Result<bool> {
        self.client.call(self.cmd("DROP").arg(ns)).await
    }

    /// `<PREFIX>.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.client.call_ro(self.cmd("LIST")).await
    }
}

// ------------------------------------------------------------ LLMSTATS --

/// `LLMSTATS.GET` fields. Parsed by field name (RESP3 map or flat RESP2
/// array): a missing field defaults to zero and an extra unknown one is
/// ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LlmStats {
    pub requests: i64,
    pub errors: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_tokens: i64,
    pub cost_usd: f64,
    pub avg_latency_ms: f64,
    pub requests_1m: i64,
    pub tokens_1m: i64,
    pub cost_usd_1m: f64,
    pub cache_write_5m_tokens: i64,
    pub cache_write_1h_tokens: i64,
    /// Requests recorded with a `MODEL` the price table has no price
    /// for; their cost is `0`, not silently wrong.
    pub unpriced_requests: i64,
}

impl FromValue for LlmStats {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(LlmStats {
            requests: field_i64(&m, "requests"),
            errors: field_i64(&m, "errors"),
            input_tokens: field_i64(&m, "input_tokens"),
            output_tokens: field_i64(&m, "output_tokens"),
            cached_tokens: field_i64(&m, "cached_tokens"),
            cost_usd: field_f64(&m, "cost_usd"),
            avg_latency_ms: field_f64(&m, "avg_latency_ms"),
            requests_1m: field_i64(&m, "requests_1m"),
            tokens_1m: field_i64(&m, "tokens_1m"),
            cost_usd_1m: field_f64(&m, "cost_usd_1m"),
            cache_write_5m_tokens: field_i64(&m, "cache_write_5m_tokens"),
            cache_write_1h_tokens: field_i64(&m, "cache_write_1h_tokens"),
            unpriced_requests: field_i64(&m, "unpriced_requests"),
        })
    }
}

/// Optional trailing keywords for [`LlmStatsCommands::record`]. `error:
/// None` omits the `ERROR` option. When `model` is set, the server
/// computes cost from its price table (see
/// [`LlmStatsCommands::price_set`]) and ignores the positional
/// `cost_usd` (pass `0.0`); `cache_write_5m`/`cache_write_1h`
/// (Anthropic cache-write tokens by TTL tier) are only meaningful
/// together with `model`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmStatsRecordOptions {
    pub error: Option<bool>,
    pub model: Option<String>,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

/// One model's decoded price sheet, as returned by
/// [`LlmStatsCommands::price_get`]/[`LlmStatsCommands::price_list`].
/// `source` is `"default"`, `"custom"` or `"unpriced"`. Parsed by field
/// name: a missing field defaults to zero/empty and an extra unknown one
/// is ignored.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmPrice {
    pub model: String,
    pub source: String,
    pub priced: bool,
    pub retired: bool,
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    pub cache_read_usd_per_mtok: f64,
    pub cache_write_5m_usd_per_mtok: f64,
    pub cache_write_1h_usd_per_mtok: f64,
}

impl FromValue for LlmPrice {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(LlmPrice {
            model: field_string(&m, "model"),
            source: field_string(&m, "source"),
            priced: field_bool_int(&m, "priced"),
            retired: field_bool_int(&m, "retired"),
            input_usd_per_mtok: field_f64(&m, "input_usd_per_mtok"),
            output_usd_per_mtok: field_f64(&m, "output_usd_per_mtok"),
            cache_read_usd_per_mtok: field_f64(&m, "cache_read_usd_per_mtok"),
            cache_write_5m_usd_per_mtok: field_f64(&m, "cache_write_5m_usd_per_mtok"),
            cache_write_1h_usd_per_mtok: field_f64(&m, "cache_write_1h_usd_per_mtok"),
        })
    }
}

/// Optional cache price overrides for [`LlmStatsCommands::price_set`].
/// `None` fields default (server side) to 0.1x (cache read), 1.25x
/// (5-minute cache write) and 2x (1-hour cache write) the input price.
/// Prices are USD per 1,000,000 tokens.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmPriceOptions {
    pub cache_read_usd_per_mtok: Option<f64>,
    pub cache_write_5m_usd_per_mtok: Option<f64>,
    pub cache_write_1h_usd_per_mtok: Option<f64>,
}

/// Builds the `LLMSTATS.RECORD` command; see
/// [`LlmStatsCommands::record`].
fn build_record_cmd(
    bucket: impl ToArg,
    input_tokens: i64,
    output_tokens: i64,
    cached_tokens: i64,
    cost_usd: f64,
    latency_ms: i64,
    opts: Option<&LlmStatsRecordOptions>,
) -> Cmd {
    let mut cmd = Cmd::new("LLMSTATS.RECORD")
        .arg(bucket)
        .arg(input_tokens)
        .arg(output_tokens)
        .arg(cached_tokens)
        .arg_non_negative(cost_usd)
        .arg(latency_ms);
    if let Some(o) = opts {
        if let Some(e) = o.error {
            cmd = cmd.arg("ERROR").arg(if e { 1 } else { 0 });
        }
        if let Some(model) = &o.model {
            cmd = cmd.arg("MODEL").arg(model);
        }
        cmd = cmd.kw_opt("CACHEWRITE5M", non_zero(o.cache_write_5m));
        cmd = cmd.kw_opt("CACHEWRITE1H", non_zero(o.cache_write_1h));
    }
    cmd
}

/// Builds the `LLMSTATS.PRICE SET` command; see
/// [`LlmStatsCommands::price_set`].
fn build_price_set_cmd(model: impl ToArg, input_usd_per_mtok: f64, output_usd_per_mtok: f64, opts: Option<&LlmPriceOptions>) -> Cmd {
    let mut cmd = Cmd::new("LLMSTATS.PRICE")
        .arg("SET")
        .arg(model)
        .arg("INPUT")
        .arg_usd(input_usd_per_mtok)
        .arg("OUTPUT")
        .arg_usd(output_usd_per_mtok);
    if let Some(o) = opts {
        if let Some(v) = o.cache_read_usd_per_mtok {
            cmd = cmd.arg("CACHEREAD").arg_usd(v);
        }
        if let Some(v) = o.cache_write_5m_usd_per_mtok {
            cmd = cmd.arg("CACHEWRITE5M").arg_usd(v);
        }
        if let Some(v) = o.cache_write_1h_usd_per_mtok {
            cmd = cmd.arg("CACHEWRITE1H").arg_usd(v);
        }
    }
    cmd
}

/// `LLMSTATS.*` commands. Reach it with [`Client::llmstats`].
pub struct LlmStatsCommands(pub(crate) Client);

impl LlmStatsCommands {
    /// `LLMSTATS.BUCKET name` (idempotent create).
    pub async fn bucket(&self, name: impl ToArg) -> Result<()> {
        self.0.call(Cmd::new("LLMSTATS.BUCKET").arg(name)).await
    }

    /// `LLMSTATS.RECORD bucket input_tokens output_tokens cached_tokens
    /// cost_usd latency_ms [ERROR 0|1] [MODEL model] [CACHEWRITE5M n]
    /// [CACHEWRITE1H n]`. `cached_tokens` is cache-READ tokens. For
    /// OpenAI, pass `prompt_tokens - cached_tokens` as `input_tokens`.
    /// `opts` may be `None`.
    #[allow(clippy::too_many_arguments)]
    pub async fn record(
        &self,
        bucket: impl ToArg,
        input_tokens: i64,
        output_tokens: i64,
        cached_tokens: i64,
        cost_usd: f64,
        latency_ms: i64,
        opts: Option<&LlmStatsRecordOptions>,
    ) -> Result<()> {
        let cmd = build_record_cmd(bucket, input_tokens, output_tokens, cached_tokens, cost_usd, latency_ms, opts);
        self.0.call(cmd).await
    }

    /// `LLMSTATS.GET bucket`.
    pub async fn get(&self, bucket: impl ToArg) -> Result<LlmStats> {
        self.0.call_ro(Cmd::new("LLMSTATS.GET").arg(bucket)).await
    }

    /// `LLMSTATS.RESET bucket`.
    pub async fn reset(&self, bucket: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("LLMSTATS.RESET").arg(bucket)).await
    }

    /// `LLMSTATS.DROP bucket`.
    pub async fn drop(&self, bucket: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("LLMSTATS.DROP").arg(bucket)).await
    }

    /// `LLMSTATS.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("LLMSTATS.LIST")).await
    }

    /// `LLMSTATS.PRICE SET model INPUT usd OUTPUT usd [CACHEREAD usd]
    /// [CACHEWRITE5M usd] [CACHEWRITE1H usd]`. Sets (or replaces) a
    /// custom price override for `model`, shared with
    /// [`LlmStatsCommands::record`]'s and [`LlmBudgetCommands::consume`]'s
    /// `MODEL` option. Prices are USD per 1,000,000 tokens. `opts` may be
    /// `None`.
    pub async fn price_set(&self, model: impl ToArg, input_usd_per_mtok: f64, output_usd_per_mtok: f64, opts: Option<&LlmPriceOptions>) -> Result<()> {
        let cmd = build_price_set_cmd(model, input_usd_per_mtok, output_usd_per_mtok, opts);
        self.0.call(cmd).await
    }

    /// `LLMSTATS.PRICE GET model`. An unknown model replies with
    /// `source: "unpriced"`, `priced: false`, not an error.
    pub async fn price_get(&self, model: impl ToArg) -> Result<LlmPrice> {
        self.0.call_ro(Cmd::new("LLMSTATS.PRICE").arg("GET").arg(model)).await
    }

    /// `LLMSTATS.PRICE LIST` -> every known model's price sheet (built-in
    /// and custom).
    pub async fn price_list(&self) -> Result<Vec<LlmPrice>> {
        self.0.call_ro(Cmd::new("LLMSTATS.PRICE").arg("LIST")).await
    }

    /// `LLMSTATS.PRICE DEL model` -> whether a custom override existed
    /// and was removed (the model then falls back to its built-in price,
    /// if any).
    pub async fn price_del(&self, model: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("LLMSTATS.PRICE").arg("DEL").arg(model)).await
    }
}

// ----------------------------------------------------------- LLMBUDGET --

/// `LLMBUDGET.STATUS` fields. Parsed by field name (RESP3 map or flat
/// RESP2 array): a missing field defaults to zero and an extra unknown
/// one is ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LlmBudgetStatus {
    pub tokens_used: i64,
    pub token_limit: i64,
    pub cost_used_usd: f64,
    pub cost_limit_usd: f64,
    pub window_seconds: i64,
    pub window_remaining: i64,
    pub requests_allowed: i64,
    pub requests_rejected: i64,
    /// Requests recorded with a `MODEL` the price table has no price
    /// for; they still consumed the token budget but cost `$0`.
    pub unpriced_requests: i64,
}

impl FromValue for LlmBudgetStatus {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(LlmBudgetStatus {
            tokens_used: field_i64(&m, "tokens_used"),
            token_limit: field_i64(&m, "token_limit"),
            cost_used_usd: field_f64(&m, "cost_used_usd"),
            cost_limit_usd: field_f64(&m, "cost_limit_usd"),
            window_seconds: field_i64(&m, "window_seconds"),
            window_remaining: field_i64(&m, "window_remaining"),
            requests_allowed: field_i64(&m, "requests_allowed"),
            requests_rejected: field_i64(&m, "requests_rejected"),
            unpriced_requests: field_i64(&m, "unpriced_requests"),
        })
    }
}

/// Builds the `LLMBUDGET.SET` command; the limit is sent as the fixed point
/// USD form the server requires (see [`crate::numfmt::usd`]).
fn build_budget_set_cmd(name: impl ToArg, token_limit: i64, cost_limit_usd: f64, window_seconds: i64) -> Cmd {
    Cmd::new("LLMBUDGET.SET").arg(name).arg(token_limit).arg_usd(cost_limit_usd).arg(window_seconds)
}

/// Builds the `LLMBUDGET.CONSUME` command.
fn build_consume_cmd(name: impl ToArg, tokens: i64, cost_usd: f64, usage: Option<&Usage>) -> Cmd {
    append_usage(Cmd::new("LLMBUDGET.CONSUME").arg(name).arg(tokens).arg_non_negative(cost_usd), usage)
}

/// `LLMBUDGET.*` commands. Reach it with [`Client::llmbudget`].
pub struct LlmBudgetCommands(pub(crate) Client);

impl LlmBudgetCommands {
    /// `LLMBUDGET.SET name token_limit cost_limit_usd window_seconds`.
    pub async fn set(&self, name: impl ToArg, token_limit: i64, cost_limit_usd: f64, window_seconds: i64) -> Result<()> {
        self.0.call(build_budget_set_cmd(name, token_limit, cost_limit_usd, window_seconds)).await
    }

    /// `LLMBUDGET.CONSUME name tokens cost_usd [usage group]` -> whether
    /// it was allowed. When `usage` is given, `tokens`/`cost_usd` are
    /// ignored server side (pass `0`/`0.0`): the token count is the sum
    /// of `usage`'s counts, and the cost is computed from the price
    /// table (see [`LlmStatsCommands::price_set`]); an unpriced model
    /// still consumes the token budget but costs `$0`, counted in
    /// [`LlmBudgetStatus::unpriced_requests`].
    pub async fn consume(&self, name: impl ToArg, tokens: i64, cost_usd: f64, usage: Option<&Usage>) -> Result<bool> {
        self.0.call(build_consume_cmd(name, tokens, cost_usd, usage)).await
    }

    /// `LLMBUDGET.STATUS name`.
    pub async fn status(&self, name: impl ToArg) -> Result<LlmBudgetStatus> {
        self.0.call_ro(Cmd::new("LLMBUDGET.STATUS").arg(name)).await
    }

    /// `LLMBUDGET.RESET name`.
    pub async fn reset(&self, name: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("LLMBUDGET.RESET").arg(name)).await
    }

    /// `LLMBUDGET.DROP name`.
    pub async fn drop(&self, name: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("LLMBUDGET.DROP").arg(name)).await
    }

    /// `LLMBUDGET.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("LLMBUDGET.LIST")).await
    }
}

impl Client {
    /// The `SCACHE.*` (semantic cache) command group.
    pub fn scache(&self) -> ScacheCommands {
        ScacheCommands(self.clone())
    }

    /// The `PCACHE.*` (prompt/LLM response cache) command group.
    pub fn pcache(&self) -> PcacheCommands {
        PcacheCommands(self.clone())
    }

    /// The `TOOL.CACHE.*` (tool call result cache) command group.
    pub fn tool_cache(&self) -> KeyedCacheCommands {
        KeyedCacheCommands::new(self.clone(), "TOOL.CACHE")
    }

    /// The `NCACHE.*` (negative/"don't retry" cache) command group.
    pub fn ncache(&self) -> KeyedCacheCommands {
        KeyedCacheCommands::new(self.clone(), "NCACHE")
    }

    /// The `GUARD.*` (guardrail/moderation decision cache) command group.
    pub fn guard(&self) -> KeyedCacheCommands {
        KeyedCacheCommands::new(self.clone(), "GUARD")
    }

    /// The `RERANK.*` (reranker result cache) command group.
    pub fn rerank(&self) -> KeyedCacheCommands {
        KeyedCacheCommands::new(self.clone(), "RERANK")
    }

    /// The `LLMSTATS.*` command group.
    pub fn llmstats(&self) -> LlmStatsCommands {
        LlmStatsCommands(self.clone())
    }

    /// The `LLMBUDGET.*` command group.
    pub fn llmbudget(&self) -> LlmBudgetCommands {
        LlmBudgetCommands(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(cmd: &Cmd) -> Vec<String> {
        cmd.parts.iter().map(|p| String::from_utf8(p.clone()).unwrap()).collect()
    }

    fn bulk(s: &str) -> Value {
        Value::Bulk(s.as_bytes().to_vec())
    }

    // ------------------------------------------------------- Usage args --

    #[test]
    fn usage_none_appends_nothing() {
        let cmd = append_usage(Cmd::new("X").arg("a"), None);
        assert_eq!(args(&cmd), vec!["X", "a"]);
    }

    #[test]
    fn usage_with_only_model_sends_just_model() {
        let usage = Usage::new("claude-sonnet-5");
        let cmd = append_usage(Cmd::new("X"), Some(&usage));
        assert_eq!(args(&cmd), vec!["X", "MODEL", "claude-sonnet-5"]);
    }

    #[test]
    fn usage_omits_zero_counts_but_always_sends_model() {
        let usage = Usage::new("gpt-6-sol").input(0).output(0).cache_read(0).cache_write_5m(0).cache_write_1h(0);
        let cmd = append_usage(Cmd::new("X"), Some(&usage));
        assert_eq!(args(&cmd), vec!["X", "MODEL", "gpt-6-sol"]);
    }

    #[test]
    fn usage_sends_every_non_zero_count_in_order() {
        let usage = Usage::new("claude-sonnet-5").input(120).output(40).cache_read(10).cache_write_5m(5).cache_write_1h(2);
        let cmd = append_usage(Cmd::new("X"), Some(&usage));
        assert_eq!(
            args(&cmd),
            vec![
                "X", "MODEL", "claude-sonnet-5", "INPUT", "120", "OUTPUT", "40", "CACHEREAD", "10", "CACHEWRITE5M", "5", "CACHEWRITE1H", "2",
            ]
        );
    }

    #[test]
    fn usage_sends_only_the_counts_that_are_non_zero() {
        let usage = Usage::new("gpt-6-sol").output(40).cache_write_1h(2);
        let cmd = append_usage(Cmd::new("X"), Some(&usage));
        assert_eq!(args(&cmd), vec!["X", "MODEL", "gpt-6-sol", "OUTPUT", "40", "CACHEWRITE1H", "2"]);
    }

    // ------------------------------------------------- SET command args --

    #[test]
    fn scache_set_without_usage() {
        let cmd = build_scache_set_cmd("faq", "q", "r", &[1.0, 0.5], Some(Expiry::Ttl(3600)), None);
        assert_eq!(args(&cmd), vec!["SCACHE.SET", "faq", "q", "r", "1", "0.5", "TTL", "3600"]);
    }

    #[test]
    fn scache_set_with_usage() {
        let usage = Usage::new("claude-sonnet-5").input(12).output(1);
        let cmd = build_scache_set_cmd("faq", "q", "r", &[1.0], None, Some(&usage));
        assert_eq!(args(&cmd), vec!["SCACHE.SET", "faq", "q", "r", "1", "MODEL", "claude-sonnet-5", "INPUT", "12", "OUTPUT", "1"]);
    }

    #[test]
    fn pcache_set_with_ttl_meta_and_usage() {
        let usage = Usage::new("gpt-6-sol").input(50);
        let cmd = build_pcache_set_cmd("chat", "prompt1", "resp1", Some(Expiry::Ttl(60)), Some("{}"), Some(&usage));
        assert_eq!(
            args(&cmd),
            vec!["PCACHE.SET", "chat", "prompt1", "resp1", "TTL", "60", "META", "{}", "MODEL", "gpt-6-sol", "INPUT", "50"]
        );
    }

    #[test]
    fn pcache_set_without_any_options() {
        let cmd = build_pcache_set_cmd("chat", "prompt1", "resp1", None, None, None);
        assert_eq!(args(&cmd), vec!["PCACHE.SET", "chat", "prompt1", "resp1"]);
    }

    #[test]
    fn keyed_cache_set_uses_the_engine_prefix_and_usage() {
        let usage = Usage::new("claude-sonnet-5").cache_read(8);
        let cmd = build_keyed_cache_set_cmd("TOOL.CACHE", "ns", "k1", "v1", None, None, Some(&usage));
        assert_eq!(args(&cmd), vec!["TOOL.CACHE.SET", "ns", "k1", "v1", "MODEL", "claude-sonnet-5", "CACHEREAD", "8"]);

        let cmd = build_keyed_cache_set_cmd("RERANK", "ns", "k1", "v1", Some(Expiry::Ttl(30)), None, None);
        assert_eq!(args(&cmd), vec!["RERANK.SET", "ns", "k1", "v1", "TTL", "30"]);
    }

    #[test]
    fn llmbudget_consume_without_and_with_usage() {
        let cmd = append_usage(Cmd::new("LLMBUDGET.CONSUME").arg("b").arg(100i64).arg(0.1f64), None);
        assert_eq!(args(&cmd), vec!["LLMBUDGET.CONSUME", "b", "100", "0.1"]);

        let usage = Usage::new("claude-sonnet-5").input(10).output(5);
        let cmd = append_usage(Cmd::new("LLMBUDGET.CONSUME").arg("b").arg(0i64).arg(0.0f64), Some(&usage));
        assert_eq!(args(&cmd), vec!["LLMBUDGET.CONSUME", "b", "0", "0", "MODEL", "claude-sonnet-5", "INPUT", "10", "OUTPUT", "5"]);
    }

    // ---------------------------------------------- LLMSTATS.RECORD args --

    #[test]
    fn record_with_no_options() {
        let cmd = build_record_cmd("b", 100, 50, 0, 0.002, 250, None);
        assert_eq!(args(&cmd), vec!["LLMSTATS.RECORD", "b", "100", "50", "0", "0.002", "250"]);
    }

    #[test]
    fn record_with_error_only() {
        let opts = LlmStatsRecordOptions { error: Some(true), ..Default::default() };
        let cmd = build_record_cmd("b", 1, 1, 0, 0.0, 1, Some(&opts));
        assert_eq!(args(&cmd), vec!["LLMSTATS.RECORD", "b", "1", "1", "0", "0", "1", "ERROR", "1"]);
    }

    #[test]
    fn record_with_model_only_ignores_cost_usd_by_convention_but_still_sends_it() {
        let opts = LlmStatsRecordOptions { model: Some("claude-sonnet-5".to_string()), ..Default::default() };
        let cmd = build_record_cmd("b", 100, 50, 10, 0.0, 250, Some(&opts));
        assert_eq!(args(&cmd), vec!["LLMSTATS.RECORD", "b", "100", "50", "10", "0", "250", "MODEL", "claude-sonnet-5"]);
    }

    #[test]
    fn record_with_model_and_cache_writes() {
        let opts = LlmStatsRecordOptions {
            error: Some(false),
            model: Some("claude-sonnet-5".to_string()),
            cache_write_5m: 100,
            cache_write_1h: 20,
        };
        let cmd = build_record_cmd("b", 1, 2, 3, 0.0, 4, Some(&opts));
        assert_eq!(
            args(&cmd),
            vec![
                "LLMSTATS.RECORD", "b", "1", "2", "3", "0", "4", "ERROR", "0", "MODEL", "claude-sonnet-5", "CACHEWRITE5M", "100",
                "CACHEWRITE1H", "20",
            ]
        );
    }

    #[test]
    fn record_cache_writes_without_model_are_still_sent() {
        // The SDK does not enforce the server's "needs MODEL" rule
        // client-side; it just builds the requested args and lets the
        // server validate.
        let opts = LlmStatsRecordOptions { cache_write_5m: 5, ..Default::default() };
        let cmd = build_record_cmd("b", 1, 1, 1, 0.0, 1, Some(&opts));
        assert_eq!(args(&cmd), vec!["LLMSTATS.RECORD", "b", "1", "1", "1", "0", "1", "CACHEWRITE5M", "5"]);
    }

    // ------------------------------------------------ LLMSTATS.PRICE args --

    #[test]
    fn usd_prices_trim_and_never_use_exponent() {
        use crate::numfmt::usd;
        assert_eq!(usd(3.0).unwrap(), "3");
        assert_eq!(usd(0.0).unwrap(), "0");
        assert_eq!(usd(0.30).unwrap(), "0.3");
        assert_eq!(usd(15.000001).unwrap(), "15.000001");
        assert_eq!(usd(0.000001).unwrap(), "0.000001");
        assert!(!usd(0.000000123).unwrap().contains('e'));
    }

    #[test]
    fn price_set_rounds_and_rejects_bad_prices() {
        let cmd = build_price_set_cmd("m", 0.1 + 0.2, 1e-7, None);
        assert_eq!(args(&cmd), vec!["LLMSTATS.PRICE", "SET", "m", "INPUT", "0.3", "OUTPUT", "0"]);
        assert!(build_price_set_cmd("m", -1.0, 1.0, None).check().is_err());
        assert!(build_price_set_cmd("m", f64::NAN, 1.0, None).check().is_err());
        let opts = LlmPriceOptions { cache_read_usd_per_mtok: Some(f64::INFINITY), ..Default::default() };
        assert!(build_price_set_cmd("m", 1.0, 1.0, Some(&opts)).check().is_err());
    }

    #[test]
    fn llmbudget_set_sends_a_fixed_point_limit() {
        for (limit, wire) in [(25.0, "25"), (0.5, "0.5"), (1e-5, "0.00001"), (0.1 + 0.2, "0.3"), (1e12, "1000000000000")] {
            let cmd = build_budget_set_cmd("b", 1_000, limit, 60);
            assert_eq!(args(&cmd), vec!["LLMBUDGET.SET", "b", "1000", wire, "60"]);
            assert!(cmd.check().is_ok());
        }
        for bad in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e300, 18446744073710.0] {
            assert!(build_budget_set_cmd("b", 1, bad, 60).check().is_err(), "{bad}");
        }
    }

    #[test]
    fn llmbudget_consume_and_record_costs_are_plain_decimals_and_non_negative() {
        for (cost, wire) in [(0.0, "0"), (1e-7, "0.0000001"), (2.5, "2.5"), (1e21, "1000000000000000000000")] {
            let cmd = build_record_cmd("b", 1, 1, 0, cost, 1, None);
            assert_eq!(args(&cmd)[5], wire);
        }
        for bad in [-0.5, f64::NAN, f64::INFINITY] {
            assert!(build_record_cmd("b", 1, 1, 0, bad, 1, None).check().is_err());
            assert!(build_consume_cmd("b", 1, bad, None).check().is_err());
        }
        assert_eq!(args(&build_consume_cmd("b", 10, 0.25, None)), vec!["LLMBUDGET.CONSUME", "b", "10", "0.25"]);
    }

    #[test]
    fn pxat_replaces_ttl_on_cache_sets() {
        let at = Some(Expiry::Pxat(1_900_000_000_000));
        let cmd = build_scache_set_cmd("ns", "q", "r", &[1.0], at, None);
        assert_eq!(args(&cmd), vec!["SCACHE.SET", "ns", "q", "r", "1", "PXAT", "1900000000000"]);
        let cmd = build_pcache_set_cmd("ns", "p", "r", at, Some("{}"), None);
        assert_eq!(args(&cmd), vec!["PCACHE.SET", "ns", "p", "r", "PXAT", "1900000000000", "META", "{}"]);
        let cmd = build_keyed_cache_set_cmd("GUARD", "ns", "k", "v", at, None, None);
        assert_eq!(args(&cmd), vec!["GUARD.SET", "ns", "k", "v", "PXAT", "1900000000000"]);
    }

    #[test]
    fn scache_pcache_float_options_are_plain_and_validated() {
        let cmd = build_scache_set_cmd("ns", "q", "r", &[0.1, -2.0, 1e-5, 0.0], None, None);
        assert_eq!(args(&cmd), vec!["SCACHE.SET", "ns", "q", "r", "0.1", "-2", "0.00001", "0"]);
        assert!(build_scache_set_cmd("ns", "q", "r", &[1.0, f64::NAN], None, None).check().is_err());
        assert!(build_scache_set_cmd("ns", "q", "r", &[1e39], None, None).check().is_err());
        assert_eq!(
            args(&build_scache_create_cmd("ns", 3, Some("cosine"), Some(0.85), Some(60))),
            vec!["SCACHE.CREATE", "ns", "3", "METRIC", "cosine", "THRESHOLD", "0.85", "TTL", "60"]
        );
        assert!(build_scache_create_cmd("ns", 3, None, Some(f64::NAN), None).check().is_err());
        assert_eq!(
            args(&build_pcache_create_cmd("ns", Some(10), None, Some(0.002))),
            vec!["PCACHE.CREATE", "ns", "MAX_ENTRIES", "10", "COST_PER_HIT", "0.002"]
        );
        assert!(build_pcache_create_cmd("ns", None, None, Some(-0.1)).check().is_err());
    }

    #[test]
    fn price_set_required_fields_only() {
        let cmd = build_price_set_cmd("gpt-6-sol", 3.0, 15.0, None);
        assert_eq!(args(&cmd), vec!["LLMSTATS.PRICE", "SET", "gpt-6-sol", "INPUT", "3", "OUTPUT", "15"]);
    }

    #[test]
    fn price_set_with_all_cache_overrides() {
        let opts = LlmPriceOptions {
            cache_read_usd_per_mtok: Some(0.3),
            cache_write_5m_usd_per_mtok: Some(3.75),
            cache_write_1h_usd_per_mtok: Some(6.0),
        };
        let cmd = build_price_set_cmd("claude-sonnet-5", 3.0, 15.0, Some(&opts));
        assert_eq!(
            args(&cmd),
            vec![
                "LLMSTATS.PRICE", "SET", "claude-sonnet-5", "INPUT", "3", "OUTPUT", "15", "CACHEREAD", "0.3", "CACHEWRITE5M", "3.75",
                "CACHEWRITE1H", "6",
            ]
        );
    }

    #[test]
    fn price_get_and_list_and_del_args() {
        assert_eq!(args(&Cmd::new("LLMSTATS.PRICE").arg("GET").arg("gpt-6-sol")), vec!["LLMSTATS.PRICE", "GET", "gpt-6-sol"]);
        assert_eq!(args(&Cmd::new("LLMSTATS.PRICE").arg("LIST")), vec!["LLMSTATS.PRICE", "LIST"]);
        assert_eq!(args(&Cmd::new("LLMSTATS.PRICE").arg("DEL").arg("gpt-6-sol")), vec!["LLMSTATS.PRICE", "DEL", "gpt-6-sol"]);
    }

    // ------------------------------------------------------ Reply parsing --

    #[test]
    fn scache_stats_parses_resp2_flat_array_with_usage_saved() {
        let v = Value::Array(vec![
            bulk("entries"), Value::Integer(1),
            bulk("hits"), Value::Integer(4),
            bulk("misses"), Value::Integer(1),
            bulk("hit_rate"), bulk("0.8"),
            bulk("p50_us"), Value::Integer(120),
            bulk("usage_saved_requests"), Value::Integer(4),
            bulk("usage_saved_input_tokens"), Value::Integer(48),
            bulk("usage_saved_output_tokens"), Value::Integer(4),
            bulk("usage_saved_cache_read_tokens"), Value::Integer(0),
            bulk("usage_saved_cache_write_tokens"), Value::Integer(0),
            bulk("usage_saved_cost_usd"), bulk("0.000160"),
        ]);
        let stats = ScacheStats::from_value(v).unwrap();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.hits, 4);
        assert_eq!(stats.hit_rate, 0.8);
        assert_eq!(stats.usage_saved.requests, 4);
        assert_eq!(stats.usage_saved.input_tokens, 48);
        assert_eq!(stats.usage_saved.cost_usd, 0.000160);
    }

    #[test]
    fn scache_stats_parses_resp3_map_and_tolerates_missing_and_unknown_fields() {
        let v = Value::Map(vec![
            (bulk("entries"), Value::Integer(2)),
            (bulk("hits"), Value::Integer(0)),
            (bulk("a_field_from_the_future"), bulk("ignored")),
        ]);
        let stats = ScacheStats::from_value(v).unwrap();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 0); // missing -> default
        assert_eq!(stats.usage_saved, UsageSavings::default());
    }

    #[test]
    fn pcache_stats_parses_all_16_fields() {
        let v = Value::Map(vec![
            (bulk("entries"), Value::Integer(10)),
            (bulk("hits"), Value::Integer(7)),
            (bulk("misses"), Value::Integer(3)),
            (bulk("evictions"), Value::Integer(1)),
            (bulk("memory_bytes"), Value::Integer(2048)),
            (bulk("hit_rate"), bulk("0.7")),
            (bulk("p50_ns"), Value::Integer(500)),
            (bulk("p95_ns"), Value::Integer(900)),
            (bulk("p99_ns"), Value::Integer(1200)),
            (bulk("cost_saved_usd"), bulk("0.014")),
            (bulk("usage_saved_requests"), Value::Integer(7)),
            (bulk("usage_saved_input_tokens"), Value::Integer(700)),
            (bulk("usage_saved_output_tokens"), Value::Integer(70)),
            (bulk("usage_saved_cache_read_tokens"), Value::Integer(0)),
            (bulk("usage_saved_cache_write_tokens"), Value::Integer(0)),
            (bulk("usage_saved_cost_usd"), bulk("0.002100")),
        ]);
        let stats = PcacheStats::from_value(v).unwrap();
        assert_eq!(stats.entries, 10);
        assert_eq!(stats.evictions, 1);
        assert_eq!(stats.p99_ns, 1200);
        assert_eq!(stats.cost_saved_usd, 0.014);
        assert_eq!(stats.usage_saved.input_tokens, 700);
        assert_eq!(stats.usage_saved.cost_usd, 0.0021);
    }

    #[test]
    fn keyed_cache_stats_parses_11_fields() {
        let v = Value::Array(vec![
            bulk("entries"), Value::Integer(3),
            bulk("hits"), Value::Integer(2),
            bulk("misses"), Value::Integer(1),
            bulk("evictions"), Value::Integer(0),
            bulk("memory_bytes"), Value::Integer(64),
            bulk("usage_saved_requests"), Value::Integer(2),
        ]);
        let stats = KeyedCacheStats::from_value(v).unwrap();
        assert_eq!(stats.entries, 3);
        assert_eq!(stats.memory_bytes, 64);
        assert_eq!(stats.usage_saved.requests, 2);
    }

    #[test]
    fn llm_stats_parses_all_13_fields_and_ignores_order() {
        let v = Value::Map(vec![
            (bulk("unpriced_requests"), Value::Integer(1)),
            (bulk("cache_write_1h_tokens"), Value::Integer(20)),
            (bulk("cache_write_5m_tokens"), Value::Integer(100)),
            (bulk("cost_usd_1m"), bulk("0.01")),
            (bulk("tokens_1m"), Value::Integer(150)),
            (bulk("requests_1m"), Value::Integer(2)),
            (bulk("avg_latency_ms"), bulk("125.5")),
            (bulk("cost_usd"), bulk("0.002")),
            (bulk("cached_tokens"), Value::Integer(15)),
            (bulk("output_tokens"), Value::Integer(55)),
            (bulk("input_tokens"), Value::Integer(110)),
            (bulk("errors"), Value::Integer(0)),
            (bulk("requests"), Value::Integer(2)),
        ]);
        let stats = LlmStats::from_value(v).unwrap();
        assert_eq!(stats.requests, 2);
        assert_eq!(stats.input_tokens, 110);
        assert_eq!(stats.cached_tokens, 15);
        assert_eq!(stats.avg_latency_ms, 125.5);
        assert_eq!(stats.cache_write_5m_tokens, 100);
        assert_eq!(stats.cache_write_1h_tokens, 20);
        assert_eq!(stats.unpriced_requests, 1);
    }

    #[test]
    fn llm_budget_status_parses_all_9_fields() {
        let v = Value::Array(vec![
            bulk("tokens_used"), Value::Integer(100),
            bulk("token_limit"), Value::Integer(1000),
            bulk("cost_used_usd"), bulk("0.010000"),
            bulk("cost_limit_usd"), bulk("1.000000"),
            bulk("window_seconds"), Value::Integer(3600),
            bulk("window_remaining"), Value::Integer(3599),
            bulk("requests_allowed"), Value::Integer(1),
            bulk("requests_rejected"), Value::Integer(0),
            bulk("unpriced_requests"), Value::Integer(0),
        ]);
        let status = LlmBudgetStatus::from_value(v).unwrap();
        assert_eq!(status.tokens_used, 100);
        assert_eq!(status.cost_used_usd, 0.01);
        assert_eq!(status.window_remaining, 3599);
        assert_eq!(status.unpriced_requests, 0);
    }

    #[test]
    fn llm_price_get_parses_all_9_fields() {
        let v = Value::Map(vec![
            (bulk("model"), bulk("claude-sonnet-5")),
            (bulk("source"), bulk("custom")),
            (bulk("priced"), Value::Integer(1)),
            (bulk("retired"), Value::Integer(0)),
            (bulk("input_usd_per_mtok"), bulk("3.000000")),
            (bulk("output_usd_per_mtok"), bulk("15.000000")),
            (bulk("cache_read_usd_per_mtok"), bulk("0.300000")),
            (bulk("cache_write_5m_usd_per_mtok"), bulk("3.750000")),
            (bulk("cache_write_1h_usd_per_mtok"), bulk("6.000000")),
        ]);
        let price = LlmPrice::from_value(v).unwrap();
        assert_eq!(price.model, "claude-sonnet-5");
        assert_eq!(price.source, "custom");
        assert!(price.priced);
        assert!(!price.retired);
        assert_eq!(price.input_usd_per_mtok, 3.0);
        assert_eq!(price.cache_write_1h_usd_per_mtok, 6.0);
    }

    #[test]
    fn llm_price_unknown_model_is_unpriced_not_an_error() {
        let v = Value::Array(vec![
            bulk("model"), bulk("no-such-model"),
            bulk("source"), bulk("unpriced"),
            bulk("priced"), Value::Integer(0),
            bulk("retired"), Value::Integer(0),
        ]);
        let price = LlmPrice::from_value(v).unwrap();
        assert_eq!(price.source, "unpriced");
        assert!(!price.priced);
        // Fields the canned reply omitted default to 0.0.
        assert_eq!(price.input_usd_per_mtok, 0.0);
    }

    #[test]
    fn llm_price_list_parses_an_array_of_entries() {
        let entry = |model: &str| {
            Value::Array(vec![
                bulk("model"), bulk(model),
                bulk("source"), bulk("default"),
                bulk("priced"), Value::Integer(1),
                bulk("retired"), Value::Integer(0),
                bulk("input_usd_per_mtok"), bulk("3.000000"),
            ])
        };
        let v = Value::Array(vec![entry("claude-sonnet-5"), entry("gpt-6-sol")]);
        let prices: Vec<LlmPrice> = FromValue::from_value(v).unwrap();
        assert_eq!(prices.len(), 2);
        assert_eq!(prices[0].model, "claude-sonnet-5");
        assert_eq!(prices[1].model, "gpt-6-sol");
    }
}
