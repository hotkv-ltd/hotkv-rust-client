// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Exact wire arguments of the enterprise (licensed) command wrappers.
//!
//! The enterprise commands answer `NOLICENSE` on an unlicensed server, so
//! they cannot be checked end to end without a license. Instead each wrapper
//! is called against an in-process server that records the command it
//! receives, and the recorded argument vector is compared with the
//! documented grammar of each command (HotKV 0.3.0). These tests need no
//! HotKV server and always run.
//!
//! A passing test here proves the SDK sends what the server grammar expects.
//! It does not prove the server accepts it: that is only verified live for
//! the core commands (see `core_methods.rs` and `floats.rs`).

use std::future::Future;
use std::sync::{Arc, Mutex};

use hotkv::ai::{Expiry, LlmPriceOptions, LlmStatsRecordOptions, Usage};
use hotkv::Client;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

type Argv = Vec<String>;

/// Reads one `*N` array of bulk strings, or `None` at end of stream.
async fn read_command<R: AsyncBufReadExt + AsyncReadExt + Unpin>(r: &mut R) -> Option<Vec<Vec<u8>>> {
    let mut line = String::new();
    if r.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let n: usize = line.trim_end().strip_prefix('*')?.parse().ok()?;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        line.clear();
        r.read_line(&mut line).await.ok()?;
        let len: usize = line.trim_end().strip_prefix('$')?.parse().ok()?;
        let mut buf = vec![0u8; len + 2];
        r.read_exact(&mut buf).await.ok()?;
        buf.truncate(len);
        out.push(buf);
    }
    Some(out)
}

/// Starts a server that records every command except `HELLO` and answers
/// `+OK`. Returns the client URL and the shared log.
async fn mock() -> (String, Arc<Mutex<Vec<Argv>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log: Arc<Mutex<Vec<Argv>>> = Arc::new(Mutex::new(Vec::new()));
    let shared = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            let shared = shared.clone();
            tokio::spawn(async move {
                let (rd, mut wr) = stream.into_split();
                let mut rd = BufReader::new(rd);
                while let Some(cmd) = read_command(&mut rd).await {
                    let argv: Argv = cmd.iter().map(|a| String::from_utf8_lossy(a).into_owned()).collect();
                    let reply: &[u8] = if argv[0].eq_ignore_ascii_case("HELLO") {
                        b"-ERR unknown command 'HELLO'\r\n"
                    } else {
                        shared.lock().unwrap().push(argv);
                        b"+OK\r\n"
                    };
                    if wr.write_all(reply).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (format!("hotkv://127.0.0.1:{port}"), log)
}

/// Runs `f` against a fresh recording server and returns what it sent.
async fn sent<F, Fut>(f: F) -> Vec<Argv>
where
    F: FnOnce(Client) -> Fut,
    Fut: Future<Output = ()>,
{
    let (url, log) = mock().await;
    let client = Client::connect(&url).await.unwrap();
    f(client).await;
    let out = log.lock().unwrap().clone();
    out
}

/// The single command `f` sends, as strings.
async fn one<F, Fut>(f: F) -> Argv
where
    F: FnOnce(Client) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut v = sent(f).await;
    assert_eq!(v.len(), 1, "expected exactly one command, got {v:?}");
    v.remove(0)
}

fn argv(parts: &[&str]) -> Argv {
    parts.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn rate_limit_commands() {
    // RATELIMIT.CREATE <name> <limit> <window_ms> [ALGO a] [REFILL r] [DRAIN d]
    assert_eq!(
        one(|c| async move { let _ = c.ratelimit().create("rl", 100, 60_000, Some("token_bucket"), Some(1.5), Some(0.25)).await; }).await,
        argv(&["RATELIMIT.CREATE", "rl", "100", "60000", "ALGO", "token_bucket", "REFILL", "1.5", "DRAIN", "0.25"])
    );
    assert_eq!(
        one(|c| async move { let _ = c.ratelimit().create("rl", 5, 1000, None, Some(1e-5), None).await; }).await,
        argv(&["RATELIMIT.CREATE", "rl", "5", "1000", "REFILL", "0.00001"])
    );
    assert_eq!(one(|c| async move { let _ = c.ratelimit().check("rl", "u1", Some(3)).await; }).await, argv(&["RATELIMIT.CHECK", "rl", "u1", "COST", "3"]));
    assert_eq!(one(|c| async move { let _ = c.ratelimit().check("rl", "u1", None).await; }).await, argv(&["RATELIMIT.CHECK", "rl", "u1"]));
    assert_eq!(one(|c| async move { let _ = c.ratelimit().peek("rl", "u1").await; }).await, argv(&["RATELIMIT.PEEK", "rl", "u1"]));
    assert_eq!(one(|c| async move { let _ = c.ratelimit().reset("rl", "u1").await; }).await, argv(&["RATELIMIT.RESET", "rl", "u1"]));
    assert_eq!(one(|c| async move { let _ = c.ratelimit().stats("rl").await; }).await, argv(&["RATELIMIT.STATS", "rl"]));
    assert_eq!(one(|c| async move { let _ = c.ratelimit().drop("rl").await; }).await, argv(&["RATELIMIT.DROP", "rl"]));
    assert_eq!(one(|c| async move { let _ = c.ratelimit().list().await; }).await, argv(&["RATELIMIT.LIST"]));
}

#[tokio::test]
async fn time_series_commands() {
    // TS.CREATE <key> [RETENTION ms] [LABELS l1 v1 ...]
    assert_eq!(
        one(|c| async move { let _ = c.ts().create("ts", Some(60_000), &[("sensor", "t1"), ("zone", "a")]).await; }).await,
        argv(&["TS.CREATE", "ts", "RETENTION", "60000", "LABELS", "sensor", "t1", "zone", "a"])
    );
    assert_eq!(one(|c| async move { let _ = c.ts().create("ts", None, &[]).await; }).await, argv(&["TS.CREATE", "ts"]));
    // TS.ADD <key> <timestamp|*> <value>; the value is read with from_chars<double>.
    assert_eq!(one(|c| async move { let _ = c.ts().add("ts", "*", 1e-5).await; }).await, argv(&["TS.ADD", "ts", "*", "0.00001"]));
    assert_eq!(one(|c| async move { let _ = c.ts().add("ts", 1000, -2.5).await; }).await, argv(&["TS.ADD", "ts", "1000", "-2.5"]));
    assert_eq!(one(|c| async move { let _ = c.ts().add("ts", 1000, 1e21).await; }).await, argv(&["TS.ADD", "ts", "1000", "1000000000000000000000"]));
    assert_eq!(one(|c| async move { let _ = c.ts().range("ts", "-", "+", Some(5)).await; }).await, argv(&["TS.RANGE", "ts", "-", "+", "COUNT", "5"]));
    assert_eq!(one(|c| async move { let _ = c.ts().agg("ts", 0, 100, 10, "avg").await; }).await, argv(&["TS.AGG", "ts", "0", "100", "10", "avg"]));
    assert_eq!(one(|c| async move { let _ = c.ts().info("ts").await; }).await, argv(&["TS.INFO", "ts"]));
    assert_eq!(one(|c| async move { let _ = c.ts().delete("ts").await; }).await, argv(&["TS.DELETE", "ts"]));
    assert_eq!(one(|c| async move { let _ = c.ts().retention("ts", 5000).await; }).await, argv(&["TS.RETENTION", "ts", "5000"]));
    assert_eq!(one(|c| async move { let _ = c.ts().downsample("a", "b", "max", 60_000).await; }).await, argv(&["TS.DOWNSAMPLE", "a", "b", "max", "60000"]));
    assert_eq!(one(|c| async move { let _ = c.ts().list(Some(("zone", "a"))).await; }).await, argv(&["TS.LIST", "LABEL", "zone", "a"]));
    assert_eq!(one(|c| async move { let _ = c.ts().list(None).await; }).await, argv(&["TS.LIST"]));
}

#[tokio::test]
async fn feature_store_commands() {
    assert_eq!(
        one(|c| async move { let _ = c.feature().create("users", "user features", &[("age", "int64"), ("score", "double")]).await; }).await,
        argv(&["FEATURE.CREATE", "users", "user features", "SCHEMA", "age", "int64", "score", "double"])
    );
    // FEATURE.SET <group> <entity> <feature1> <value1> ...  (values are text)
    assert_eq!(
        one(|c| async move { let _ = c.feature().set("users", "u42", [("age", "30"), ("score", "0.5")]).await; }).await,
        argv(&["FEATURE.SET", "users", "u42", "age", "30", "score", "0.5"])
    );
    // FEATURE.SETAT <group> <entity> <unix-ms> <feature1> <value1> ...
    assert_eq!(
        one(|c| async move { let _ = c.feature().set_at("users", "u42", 1_700_000_000_000, [("age", "31")]).await; }).await,
        argv(&["FEATURE.SETAT", "users", "u42", "1700000000000", "age", "31"])
    );
    assert_eq!(one(|c| async move { let _ = c.feature().get("users", "u42", &["age"]).await; }).await, argv(&["FEATURE.GET", "users", "u42", "age"]));
    assert_eq!(one(|c| async move { let _ = c.feature().get("users", "u42", &[]).await; }).await, argv(&["FEATURE.GET", "users", "u42"]));
    assert_eq!(
        one(|c| async move { let _ = c.feature().batch("users", &["u1", "u2"], &["age", "score"]).await; }).await,
        argv(&["FEATURE.BATCH", "users", "u1", "u2", "FEATURES", "age", "score"])
    );
    assert_eq!(one(|c| async move { let _ = c.feature().batch("users", &["u1"], &[]).await; }).await, argv(&["FEATURE.BATCH", "users", "u1"]));
    assert_eq!(one(|c| async move { let _ = c.feature().schema("users").await; }).await, argv(&["FEATURE.SCHEMA", "users"]));
    assert_eq!(one(|c| async move { let _ = c.feature().list().await; }).await, argv(&["FEATURE.LIST"]));
    assert_eq!(one(|c| async move { let _ = c.feature().drop("users").await; }).await, argv(&["FEATURE.DROP", "users"]));
    assert_eq!(one(|c| async move { let _ = c.feature().stats("users").await; }).await, argv(&["FEATURE.STATS", "users"]));
}

#[tokio::test]
async fn observability_commands() {
    assert_eq!(one(|c| async move { let _ = c.observe().info().await; }).await, argv(&["OBSERVE", "INFO"]));
    assert_eq!(one(|c| async move { let _ = c.observe().metrics().await; }).await, argv(&["OBSERVE", "METRICS"]));
    assert_eq!(one(|c| async move { let _ = c.alerts().info().await; }).await, argv(&["ALERTS", "INFO"]));
    assert_eq!(one(|c| async move { let _ = c.alerts().test(Some("warning"), Some("disk almost full")).await; }).await, argv(&["ALERTS", "TEST", "warning", "disk almost full"]));
    assert_eq!(one(|c| async move { let _ = c.alerts().set_url("https://hooks.example/x").await; }).await, argv(&["ALERTS", "SETURL", "https://hooks.example/x"]));
    assert_eq!(one(|c| async move { let _ = c.alerts().set_severity("critical").await; }).await, argv(&["ALERTS", "SETSEVERITY", "critical"]));
    assert_eq!(one(|c| async move { let _ = c.alerts().on().await; }).await, argv(&["ALERTS", "ON"]));
    assert_eq!(one(|c| async move { let _ = c.alerts().off().await; }).await, argv(&["ALERTS", "OFF"]));
}

#[tokio::test]
async fn vector_snapshot_commands() {
    // VSIM.SAVE <snapshot_name> <blob>, VSIM.LOAD, VSIM.LIST, VSIM.DROP
    assert_eq!(one(|c| async move { let _ = c.vsim().save("snap1", b"blob").await; }).await, argv(&["VSIM.SAVE", "snap1", "blob"]));
    assert_eq!(one(|c| async move { let _ = c.vsim().load("snap1").await; }).await, argv(&["VSIM.LOAD", "snap1"]));
    assert_eq!(one(|c| async move { let _ = c.vsim().list_snapshots().await; }).await, argv(&["VSIM.LIST"]));
    assert_eq!(one(|c| async move { let _ = c.vsim().drop_snapshot("snap1").await; }).await, argv(&["VSIM.DROP", "snap1"]));
}

#[tokio::test]
async fn semantic_cache_commands() {
    // SCACHE.CREATE <ns> <dims> [METRIC m] [THRESHOLD t] [TTL s]; the threshold is read as a 32 bit float.
    assert_eq!(
        one(|c| async move { let _ = c.scache().create("faq", 3, Some("cosine"), Some(0.85), Some(3600)).await; }).await,
        argv(&["SCACHE.CREATE", "faq", "3", "METRIC", "cosine", "THRESHOLD", "0.85", "TTL", "3600"])
    );
    // SCACHE.SET <ns> <query_text> <response> <vector...> [TTL s | PXAT ms] [MODEL ...]
    let usage = Usage::new("claude-sonnet-5").input(10).output(5).cache_read(2);
    assert_eq!(
        one(|c| async move { let _ = c.scache().set("faq", "q", "r", &[0.1, -2.0, 1e-5], Some(60), Some(&usage)).await; }).await,
        argv(&["SCACHE.SET", "faq", "q", "r", "0.1", "-2", "0.00001", "TTL", "60", "MODEL", "claude-sonnet-5", "INPUT", "10", "OUTPUT", "5", "CACHEREAD", "2"])
    );
    assert_eq!(
        one(|c| async move { let _ = c.scache().set_with_expiry("faq", "q", "r", &[1.0], Some(Expiry::Pxat(1_900_000_000_000)), None).await; }).await,
        argv(&["SCACHE.SET", "faq", "q", "r", "1", "PXAT", "1900000000000"])
    );
    // SCACHE.GET <ns> <vector...> [THRESHOLD override]
    assert_eq!(one(|c| async move { let _ = c.scache().get("faq", &[0.5, 0.25], Some(0.9)).await; }).await, argv(&["SCACHE.GET", "faq", "0.5", "0.25", "THRESHOLD", "0.9"]));
    assert_eq!(one(|c| async move { let _ = c.scache().get("faq", &[0.5], None).await; }).await, argv(&["SCACHE.GET", "faq", "0.5"]));
    assert_eq!(one(|c| async move { let _ = c.scache().stats("faq").await; }).await, argv(&["SCACHE.STATS", "faq"]));
    assert_eq!(one(|c| async move { let _ = c.scache().flush("faq").await; }).await, argv(&["SCACHE.FLUSH", "faq"]));
    assert_eq!(one(|c| async move { let _ = c.scache().drop("faq").await; }).await, argv(&["SCACHE.DROP", "faq"]));
    assert_eq!(one(|c| async move { let _ = c.scache().list().await; }).await, argv(&["SCACHE.LIST"]));
    assert_eq!(one(|c| async move { let _ = c.scache().set_threshold("faq", 0.75).await; }).await, argv(&["SCACHE.THRESHOLD", "faq", "0.75"]));
}

#[tokio::test]
async fn prompt_cache_commands() {
    // PCACHE.CREATE <ns> [MAX_ENTRIES n] [TTL s] [COST_PER_HIT usd]
    assert_eq!(
        one(|c| async move { let _ = c.pcache().create("chat", Some(1000), Some(3600), Some(0.002)).await; }).await,
        argv(&["PCACHE.CREATE", "chat", "MAX_ENTRIES", "1000", "TTL", "3600", "COST_PER_HIT", "0.002"])
    );
    // PCACHE.SET <ns> <prompt> <response> [TTL s | PXAT ms] [META json] [MODEL ...]
    let usage = Usage::new("gpt-6-sol").input(100).cache_write_5m(7);
    assert_eq!(
        one(|c| async move { let _ = c.pcache().set("chat", "p", "r", Some(60), Some("{\"a\":1}"), Some(&usage)).await; }).await,
        argv(&["PCACHE.SET", "chat", "p", "r", "TTL", "60", "META", "{\"a\":1}", "MODEL", "gpt-6-sol", "INPUT", "100", "CACHEWRITE5M", "7"])
    );
    assert_eq!(
        one(|c| async move { let _ = c.pcache().set_with_expiry("chat", "p", "r", Some(Expiry::Pxat(1_900_000_000_000)), None, None).await; }).await,
        argv(&["PCACHE.SET", "chat", "p", "r", "PXAT", "1900000000000"])
    );
    assert_eq!(one(|c| async move { let _ = c.pcache().get("chat", "p").await; }).await, argv(&["PCACHE.GET", "chat", "p"]));
    assert_eq!(one(|c| async move { let _ = c.pcache().getex("chat", "p").await; }).await, argv(&["PCACHE.GETEX", "chat", "p"]));
    assert_eq!(one(|c| async move { let _ = c.pcache().del("chat", "p").await; }).await, argv(&["PCACHE.DEL", "chat", "p"]));
    assert_eq!(one(|c| async move { let _ = c.pcache().stats("chat").await; }).await, argv(&["PCACHE.STATS", "chat"]));
    assert_eq!(one(|c| async move { let _ = c.pcache().flush("chat").await; }).await, argv(&["PCACHE.FLUSH", "chat"]));
    assert_eq!(one(|c| async move { let _ = c.pcache().drop("chat").await; }).await, argv(&["PCACHE.DROP", "chat"]));
    assert_eq!(one(|c| async move { let _ = c.pcache().list().await; }).await, argv(&["PCACHE.LIST"]));
    // PCACHE.CONFIG <ns> SET <option> <value>
    assert_eq!(one(|c| async move { let _ = c.pcache().config_set("chat", "COST_PER_HIT", 0.5).await; }).await, argv(&["PCACHE.CONFIG", "chat", "SET", "COST_PER_HIT", "0.5"]));
    assert_eq!(one(|c| async move { let _ = c.pcache().config_set("chat", "TTL", 30).await; }).await, argv(&["PCACHE.CONFIG", "chat", "SET", "TTL", "30"]));
}

#[tokio::test]
async fn keyed_cache_commands_use_each_engine_prefix() {
    for prefix in ["TOOL.CACHE", "NCACHE", "GUARD", "RERANK"] {
        let pick = move |c: &Client| match prefix {
            "TOOL.CACHE" => c.tool_cache(),
            "NCACHE" => c.ncache(),
            "GUARD" => c.guard(),
            _ => c.rerank(),
        };
        let p = |s: &str| format!("{prefix}.{s}");
        // <PREFIX>.CREATE ns [MAX_ENTRIES n] [TTL s]
        let got = one(|c| async move { let _ = pick(&c).create("ns", Some(10), Some(60)).await; }).await;
        assert_eq!(got, vec![p("CREATE"), "ns".into(), "MAX_ENTRIES".into(), "10".into(), "TTL".into(), "60".into()]);
        // <PREFIX>.SET ns key value [TTL s | PXAT ms] [META json] [MODEL ...]
        let usage = Usage::new("m").output(9);
        let got = one(|c| async move { let _ = pick(&c).set("ns", "k", "v", Some(5), Some("{}"), Some(&usage)).await; }).await;
        assert_eq!(got, vec![p("SET"), "ns".into(), "k".into(), "v".into(), "TTL".into(), "5".into(), "META".into(), "{}".into(), "MODEL".into(), "m".into(), "OUTPUT".into(), "9".into()]);
        let got = one(|c| async move { let _ = pick(&c).set_with_expiry("ns", "k", "v", Some(Expiry::Pxat(1_900_000_000_000)), None, None).await; }).await;
        assert_eq!(got, vec![p("SET"), "ns".into(), "k".into(), "v".into(), "PXAT".into(), "1900000000000".into()]);
        let got = one(|c| async move { let _ = pick(&c).get("ns", "k").await; }).await;
        assert_eq!(got, vec![p("GET"), "ns".into(), "k".into()]);
        let got = one(|c| async move { let _ = pick(&c).del("ns", "k").await; }).await;
        assert_eq!(got, vec![p("DEL"), "ns".into(), "k".into()]);
        let got = one(|c| async move { let _ = pick(&c).stats("ns").await; }).await;
        assert_eq!(got, vec![p("STATS"), "ns".into()]);
        let got = one(|c| async move { let _ = pick(&c).flush("ns").await; }).await;
        assert_eq!(got, vec![p("FLUSH"), "ns".into()]);
        let got = one(|c| async move { let _ = pick(&c).drop("ns").await; }).await;
        assert_eq!(got, vec![p("DROP"), "ns".into()]);
        let got = one(|c| async move { let _ = pick(&c).list().await; }).await;
        assert_eq!(got, vec![p("LIST")]);
    }
}

#[tokio::test]
async fn llm_stats_and_budget_commands() {
    assert_eq!(one(|c| async move { let _ = c.llmstats().bucket("chat").await; }).await, argv(&["LLMSTATS.BUCKET", "chat"]));
    // LLMSTATS.RECORD <bucket> <in> <out> <cached> <cost_usd> <latency_ms> [ERROR 0|1] [MODEL m] [CACHEWRITE5M n] [CACHEWRITE1H n]
    assert_eq!(
        one(|c| async move { let _ = c.llmstats().record("chat", 1200, 340, 5, 0.0021, 610, None).await; }).await,
        argv(&["LLMSTATS.RECORD", "chat", "1200", "340", "5", "0.0021", "610"])
    );
    let opts = LlmStatsRecordOptions { error: Some(false), model: Some("claude-sonnet-5".into()), cache_write_5m: 100, cache_write_1h: 20 };
    assert_eq!(
        one(|c| async move { let _ = c.llmstats().record("chat", 1, 2, 3, 0.0, 4, Some(&opts)).await; }).await,
        argv(&["LLMSTATS.RECORD", "chat", "1", "2", "3", "0", "4", "ERROR", "0", "MODEL", "claude-sonnet-5", "CACHEWRITE5M", "100", "CACHEWRITE1H", "20"])
    );
    assert_eq!(one(|c| async move { let _ = c.llmstats().get("chat").await; }).await, argv(&["LLMSTATS.GET", "chat"]));
    assert_eq!(one(|c| async move { let _ = c.llmstats().reset("chat").await; }).await, argv(&["LLMSTATS.RESET", "chat"]));
    assert_eq!(one(|c| async move { let _ = c.llmstats().drop("chat").await; }).await, argv(&["LLMSTATS.DROP", "chat"]));
    assert_eq!(one(|c| async move { let _ = c.llmstats().list().await; }).await, argv(&["LLMSTATS.LIST"]));
    // LLMSTATS.PRICE SET <model> INPUT <usd> OUTPUT <usd> [CACHEREAD ..] [CACHEWRITE5M ..] [CACHEWRITE1H ..]: fixed point, at most six fractional digits
    let price = LlmPriceOptions { cache_read_usd_per_mtok: Some(0.3), cache_write_5m_usd_per_mtok: Some(3.75), cache_write_1h_usd_per_mtok: Some(0.1 + 0.2) };
    assert_eq!(
        one(|c| async move { let _ = c.llmstats().price_set("m", 3.0, 15.0, Some(&price)).await; }).await,
        argv(&["LLMSTATS.PRICE", "SET", "m", "INPUT", "3", "OUTPUT", "15", "CACHEREAD", "0.3", "CACHEWRITE5M", "3.75", "CACHEWRITE1H", "0.3"])
    );
    assert_eq!(one(|c| async move { let _ = c.llmstats().price_get("m").await; }).await, argv(&["LLMSTATS.PRICE", "GET", "m"]));
    assert_eq!(one(|c| async move { let _ = c.llmstats().price_list().await; }).await, argv(&["LLMSTATS.PRICE", "LIST"]));
    assert_eq!(one(|c| async move { let _ = c.llmstats().price_del("m").await; }).await, argv(&["LLMSTATS.PRICE", "DEL", "m"]));

    // LLMBUDGET.SET <name> <token_limit> <cost_limit_usd> <window_seconds>: fixed point USD
    assert_eq!(
        one(|c| async move { let _ = c.llmbudget().set("daily", 1_000_000, 25.0, 86_400).await; }).await,
        argv(&["LLMBUDGET.SET", "daily", "1000000", "25", "86400"])
    );
    assert_eq!(
        one(|c| async move { let _ = c.llmbudget().set("daily", 1, 0.000001, 60).await; }).await,
        argv(&["LLMBUDGET.SET", "daily", "1", "0.000001", "60"])
    );
    assert_eq!(
        one(|c| async move { let _ = c.llmbudget().set("daily", 1, 1e-7, 60).await; }).await,
        argv(&["LLMBUDGET.SET", "daily", "1", "0", "60"])
    );
    // LLMBUDGET.CONSUME <name> <tokens> <cost_usd> [MODEL m] [INPUT n] ...
    assert_eq!(one(|c| async move { let _ = c.llmbudget().consume("daily", 100, 0.1, None).await; }).await, argv(&["LLMBUDGET.CONSUME", "daily", "100", "0.1"]));
    let usage = Usage::new("claude-sonnet-5").input(10).output(5).cache_write_1h(3);
    assert_eq!(
        one(|c| async move { let _ = c.llmbudget().consume("daily", 0, 0.0, Some(&usage)).await; }).await,
        argv(&["LLMBUDGET.CONSUME", "daily", "0", "0", "MODEL", "claude-sonnet-5", "INPUT", "10", "OUTPUT", "5", "CACHEWRITE1H", "3"])
    );
    assert_eq!(one(|c| async move { let _ = c.llmbudget().status("daily").await; }).await, argv(&["LLMBUDGET.STATUS", "daily"]));
    assert_eq!(one(|c| async move { let _ = c.llmbudget().reset("daily").await; }).await, argv(&["LLMBUDGET.RESET", "daily"]));
    assert_eq!(one(|c| async move { let _ = c.llmbudget().drop("daily").await; }).await, argv(&["LLMBUDGET.DROP", "daily"]));
    assert_eq!(one(|c| async move { let _ = c.llmbudget().list().await; }).await, argv(&["LLMBUDGET.LIST"]));
}

#[tokio::test]
async fn bad_money_never_reaches_the_server() {
    let log = sent(|c| async move {
        for bad in [-1.0, f64::NAN, f64::INFINITY, 1e300] {
            assert!(c.llmbudget().set("b", 1, bad, 60).await.is_err());
            assert!(c.llmstats().price_set("m", bad, 1.0, None).await.is_err());
        }
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            assert!(c.llmbudget().consume("b", 1, bad, None).await.is_err());
            assert!(c.llmstats().record("b", 1, 1, 0, bad, 1, None).await.is_err());
            assert!(c.pcache().create("ns", None, None, Some(bad)).await.is_err());
        }
        assert!(c.scache().create("ns", 3, None, Some(f64::NAN), None).await.is_err());
        assert!(c.scache().get("ns", &[f64::INFINITY], None).await.is_err());
    })
    .await;
    assert!(log.is_empty(), "nothing may be sent for refused arguments, got {log:?}");
}

#[tokio::test]
async fn prompt_registry_and_embedding_dedup_commands() {
    assert_eq!(one(|c| async move { let _ = c.prompt().put("greet", "Hello {{name}}", Some("{\"a\":1}")).await; }).await, argv(&["PROMPT.PUT", "greet", "Hello {{name}}", "META", "{\"a\":1}"]));
    assert_eq!(one(|c| async move { let _ = c.prompt().put("greet", "x", None).await; }).await, argv(&["PROMPT.PUT", "greet", "x"]));
    assert_eq!(one(|c| async move { let _ = c.prompt().get("greet", Some(2)).await; }).await, argv(&["PROMPT.GET", "greet", "2"]));
    assert_eq!(one(|c| async move { let _ = c.prompt().get("greet", None).await; }).await, argv(&["PROMPT.GET", "greet"]));
    assert_eq!(one(|c| async move { let _ = c.prompt().latest("greet").await; }).await, argv(&["PROMPT.LATEST", "greet"]));
    assert_eq!(one(|c| async move { let _ = c.prompt().set_active("greet", 2).await; }).await, argv(&["PROMPT.SETACTIVE", "greet", "2"]));
    assert_eq!(one(|c| async move { let _ = c.prompt().versions("greet").await; }).await, argv(&["PROMPT.VERSIONS", "greet"]));
    assert_eq!(one(|c| async move { let _ = c.prompt().drop("greet").await; }).await, argv(&["PROMPT.DROP", "greet"]));
    assert_eq!(one(|c| async move { let _ = c.prompt().list().await; }).await, argv(&["PROMPT.LIST"]));

    assert_eq!(one(|c| async move { let _ = c.embed_dedup().put("ns", "h1", "text-embedding-3", "0.1,0.2").await; }).await, argv(&["EMBED.DEDUP.PUT", "ns", "h1", "text-embedding-3", "0.1,0.2"]));
    assert_eq!(one(|c| async move { let _ = c.embed_dedup().get("ns", "h1").await; }).await, argv(&["EMBED.DEDUP.GET", "ns", "h1"]));
    assert_eq!(one(|c| async move { let _ = c.embed_dedup().release("ns", "h1").await; }).await, argv(&["EMBED.DEDUP.RELEASE", "ns", "h1"]));
    assert_eq!(one(|c| async move { let _ = c.embed_dedup().stats("ns").await; }).await, argv(&["EMBED.DEDUP.STATS", "ns"]));
    assert_eq!(one(|c| async move { let _ = c.embed_dedup().drop("ns").await; }).await, argv(&["EMBED.DEDUP.DROP", "ns"]));
    assert_eq!(one(|c| async move { let _ = c.embed_dedup().purge("ns").await; }).await, argv(&["EMBED.DEDUP.PURGE", "ns"]));
    assert_eq!(one(|c| async move { let _ = c.embed_dedup().list().await; }).await, argv(&["EMBED.DEDUP.LIST"]));
}

#[tokio::test]
async fn agent_memory_commands() {
    // AMEM.CONV.CREATE <session_id> [WINDOW size] [TTL seconds | PXAT unix-ms]
    assert_eq!(one(|c| async move { let _ = c.amem().conv_create("s1", Some(20), Some(3600)).await; }).await, argv(&["AMEM.CONV.CREATE", "s1", "WINDOW", "20", "TTL", "3600"]));
    assert_eq!(
        one(|c| async move { let _ = c.amem().conv_create_with_expiry("s1", None, Some(Expiry::Pxat(1_900_000_000_000))).await; }).await,
        argv(&["AMEM.CONV.CREATE", "s1", "PXAT", "1900000000000"])
    );
    assert_eq!(one(|c| async move { let _ = c.amem().conv_append("s1", "user", "hi").await; }).await, argv(&["AMEM.CONV.APPEND", "s1", "user", "hi"]));
    assert_eq!(one(|c| async move { let _ = c.amem().conv_get("s1").await; }).await, argv(&["AMEM.CONV.GET", "s1"]));
    assert_eq!(one(|c| async move { let _ = c.amem().conv_window("s1", 5).await; }).await, argv(&["AMEM.CONV.WINDOW", "s1", "5"]));
    assert_eq!(one(|c| async move { let _ = c.amem().conv_clear("s1").await; }).await, argv(&["AMEM.CONV.CLEAR", "s1"]));
    assert_eq!(one(|c| async move { let _ = c.amem().conv_drop("s1").await; }).await, argv(&["AMEM.CONV.DROP", "s1"]));

    // semantic memory: vectors are 32 bit floats
    assert_eq!(one(|c| async move { let _ = c.amem().sem_create("agent", 3).await; }).await, argv(&["AMEM.SEM.CREATE", "agent", "3"]));
    assert_eq!(
        one(|c| async move { let _ = c.amem().sem_store("agent", "k", "content", &[0.1, 0.2, -1e-5], Some(60)).await; }).await,
        argv(&["AMEM.SEM.STORE", "agent", "k", "content", "0.1", "0.2", "-0.00001", "TTL", "60"])
    );
    assert_eq!(one(|c| async move { let _ = c.amem().sem_recall("agent", 5, &[0.5, 0.5, 0.0]).await; }).await, argv(&["AMEM.SEM.RECALL", "agent", "5", "0.5", "0.5", "0"]));
    assert_eq!(one(|c| async move { let _ = c.amem().sem_forget("agent", "k").await; }).await, argv(&["AMEM.SEM.FORGET", "agent", "k"]));
    assert_eq!(one(|c| async move { let _ = c.amem().sem_list("agent").await; }).await, argv(&["AMEM.SEM.LIST", "agent"]));

    // working memory
    assert_eq!(one(|c| async move { let _ = c.amem().work_set("s1", "k", "v", Some(30)).await; }).await, argv(&["AMEM.WORK.SET", "s1", "k", "v", "TTL", "30"]));
    assert_eq!(
        one(|c| async move { let _ = c.amem().work_set_with_expiry("s1", "k", "v", Some(Expiry::Pxat(1_900_000_000_000))).await; }).await,
        argv(&["AMEM.WORK.SET", "s1", "k", "v", "PXAT", "1900000000000"])
    );
    assert_eq!(one(|c| async move { let _ = c.amem().work_get("s1", "k").await; }).await, argv(&["AMEM.WORK.GET", "s1", "k"]));
    assert_eq!(one(|c| async move { let _ = c.amem().work_del("s1", "k").await; }).await, argv(&["AMEM.WORK.DEL", "s1", "k"]));
    assert_eq!(one(|c| async move { let _ = c.amem().work_clear("s1").await; }).await, argv(&["AMEM.WORK.CLEAR", "s1"]));
    assert_eq!(one(|c| async move { let _ = c.amem().work_list("s1").await; }).await, argv(&["AMEM.WORK.LIST", "s1"]));

    // episodic memory
    assert_eq!(
        one(|c| async move { let _ = c.amem().epis_log("agent", "tool_call", "did a thing", Some("{\"ok\":true}")).await; }).await,
        argv(&["AMEM.EPIS.LOG", "agent", "tool_call", "did a thing", "METADATA", "{\"ok\":true}"])
    );
    assert_eq!(
        one(|c| async move { let _ = c.amem().epis_query("agent", Some("tool_call"), Some(10), Some(20), Some(5)).await; }).await,
        argv(&["AMEM.EPIS.QUERY", "agent", "TYPE", "tool_call", "FROM", "10", "TO", "20", "LIMIT", "5"])
    );
    assert_eq!(one(|c| async move { let _ = c.amem().epis_query("agent", None, None, None, None).await; }).await, argv(&["AMEM.EPIS.QUERY", "agent"]));
    assert_eq!(one(|c| async move { let _ = c.amem().epis_clear("agent").await; }).await, argv(&["AMEM.EPIS.CLEAR", "agent"]));
    assert_eq!(one(|c| async move { let _ = c.amem().epis_stats("agent").await; }).await, argv(&["AMEM.EPIS.STATS", "agent"]));
    assert_eq!(one(|c| async move { let _ = c.amem().search("agent", 3, &[1.0, 0.0]).await; }).await, argv(&["AMEM.SEARCH", "agent", "3", "1", "0"]));

    // summaries
    assert_eq!(one(|c| async move { let _ = c.amem().summary_set("agent", "likes tea").await; }).await, argv(&["AMEM.SUMMARY.SET", "agent", "likes tea"]));
    assert_eq!(one(|c| async move { let _ = c.amem().summary_append("agent", "and cake").await; }).await, argv(&["AMEM.SUMMARY.APPEND", "agent", "and cake"]));
    assert_eq!(one(|c| async move { let _ = c.amem().summary_get("agent").await; }).await, argv(&["AMEM.SUMMARY.GET", "agent"]));
    assert_eq!(one(|c| async move { let _ = c.amem().summary_drop("agent").await; }).await, argv(&["AMEM.SUMMARY.DROP", "agent"]));
    assert_eq!(one(|c| async move { let _ = c.amem().summary_list().await; }).await, argv(&["AMEM.SUMMARY.LIST"]));
}

#[tokio::test]
async fn rag_commands() {
    // RAG.CREATE <name> <dims> [STRATEGY s] [CHUNK_SIZE n] [CHUNK_OVERLAP n]
    assert_eq!(
        one(|c| async move { let _ = c.rag().create("docs", 384, Some("sentence"), Some(512), Some(64)).await; }).await,
        argv(&["RAG.CREATE", "docs", "384", "STRATEGY", "sentence", "CHUNK_SIZE", "512", "CHUNK_OVERLAP", "64"])
    );
    // RAG.INGEST <pipeline> <doc_id> <text> [META k v ...] [VECTORS v1_d1,v1_d2,... v2_d1,...]
    let v1: &[f64] = &[0.1, 0.2];
    let v2: &[f64] = &[-1e-5, 1.0];
    assert_eq!(
        one(|c| async move { let _ = c.rag().ingest("docs", "d1", "some text", &[("author", "ada")], &[v1, v2]).await; }).await,
        argv(&["RAG.INGEST", "docs", "d1", "some text", "META", "author", "ada", "VECTORS", "0.1,0.2", "-0.00001,1"])
    );
    // RAG.SEARCH <pipeline> <query_text> [K n] [ALPHA f] [VECTOR v1,v2,...] [FILTER k v ...]
    assert_eq!(
        one(|c| async move { let _ = c.rag().search("docs", "query", Some(5), Some(0.5), Some(&[0.25, 0.75]), &[("author", "ada")]).await; }).await,
        argv(&["RAG.SEARCH", "docs", "query", "K", "5", "ALPHA", "0.5", "VECTOR", "0.25,0.75", "FILTER", "author", "ada"])
    );
    assert_eq!(one(|c| async move { let _ = c.rag().search("docs", "query", None, None, None, &[]).await; }).await, argv(&["RAG.SEARCH", "docs", "query"]));
    assert_eq!(one(|c| async move { let _ = c.rag().stats("docs").await; }).await, argv(&["RAG.STATS", "docs"]));
    assert_eq!(one(|c| async move { let _ = c.rag().drop("docs").await; }).await, argv(&["RAG.DROP", "docs"]));
    assert_eq!(one(|c| async move { let _ = c.rag().list().await; }).await, argv(&["RAG.LIST"]));
}
