// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Live checks of the enterprise API on a licensed server: float formatting
//! end to end, absolute expiry, rate limiter algorithms, time series and
//! feature store values, vector snapshots, alert settings, and the commands
//! without typed methods (tenants, IAM, audit, encryption, replication).
//!
//! A command answering `NOLICENSE` makes the test print why and return. When
//! `HOTKV_TEST_LICENSED` is set (the server is declared licensed) a missing
//! license fails the test instead.

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hotkv::ai::{Expiry, LlmPriceOptions, LlmStatsRecordOptions, Usage};
use hotkv::{Error, FromValue, Value};

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

#[tokio::test]
async fn llm_budget_floats_end_to_end() {
    let Some(client) = common::connect().await else { return };
    let lb = client.llmbudget();
    let _ = skip_if_not_licensed!(lb.list().await, "llm-budget");
    let lb = client.llmbudget();

    // The limit goes out as fixed point USD with at most six fractional digits.
    for (limit, expected) in [(0.00001, 0.00001), (0.1 + 0.2, 0.3), (25.0, 25.0), (1e12, 1e12), (1e-7, 0.0), (0.0000015, 0.000002)] {
        let name = common::unique("budget-limit").replace(':', "-");
        lb.set(&name, 1_000_000, limit, 60).await.unwrap_or_else(|e| panic!("limit {limit}: {e}"));
        let status = lb.status(&name).await.unwrap();
        assert!((status.cost_limit_usd - expected).abs() <= expected * 1e-9 + 1e-12, "limit {limit}: stored {}", status.cost_limit_usd);
        lb.drop(&name).await.unwrap();
    }

    // Enforcement at the micro-dollar: 0.00001 USD allows two 0.000005 charges.
    let name = common::unique("budget-enforce").replace(':', "-");
    lb.set(&name, 1_000_000, 0.00001, 60).await.unwrap();
    assert!(lb.consume(&name, 0, 0.000005, None).await.unwrap());
    assert!(lb.consume(&name, 0, 0.000005, None).await.unwrap());
    assert!(!lb.consume(&name, 0, 0.000005, None).await.unwrap(), "third charge exceeds the limit");
    let status = lb.status(&name).await.unwrap();
    assert!((status.cost_used_usd - 0.00001).abs() < 1e-9, "{}", status.cost_used_usd);
    assert_eq!(status.requests_allowed, 2);
    assert_eq!(status.requests_rejected, 1);
    // A cost below a micro-dollar is accepted as a number (from_chars) and rounds down to nothing.
    lb.reset(&name).await.unwrap();
    assert!(lb.consume(&name, 0, 1e-7, None).await.unwrap());
    lb.drop(&name).await.unwrap();

    // Refused before sending: negative, NaN, infinity, too large.
    for bad in [-0.5, f64::NAN, f64::INFINITY, 1e300] {
        let err = lb.set("never-created", 1, bad, 60).await.unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "{bad}: {err}");
    }
    assert!(!lb.list().await.unwrap().iter().any(|n| n == "never-created"));

    // The token limit is enforced too, with a priced model in the usage group.
    let name = common::unique("budget-tokens").replace(':', "-");
    lb.set(&name, 100, 1000.0, 60).await.unwrap();
    assert!(lb.consume(&name, 60, 0.0, None).await.unwrap());
    assert!(!lb.consume(&name, 60, 0.0, None).await.unwrap());
    lb.drop(&name).await.unwrap();
}

#[tokio::test]
async fn llm_stats_prices_and_costs_end_to_end() {
    let Some(client) = common::connect().await else { return };
    let ls = client.llmstats();
    let _ = skip_if_not_licensed!(ls.price_list().await, "llm-stats");
    let ls = client.llmstats();
    let model = common::unique("model").replace(':', "-");

    // Prices are fixed point too: 0.1 + 0.2 becomes 0.3, 1e-7 becomes 0.
    let opts = LlmPriceOptions { cache_read_usd_per_mtok: Some(1e-5), cache_write_5m_usd_per_mtok: Some(3.75), cache_write_1h_usd_per_mtok: Some(6.0) };
    ls.price_set(&model, 0.1 + 0.2, 15.0, Some(&opts)).await.unwrap();
    let price = ls.price_get(&model).await.unwrap();
    assert_eq!(price.source, "custom");
    assert!((price.input_usd_per_mtok - 0.3).abs() < 1e-12);
    assert_eq!(price.output_usd_per_mtok, 15.0);
    assert!((price.cache_read_usd_per_mtok - 0.00001).abs() < 1e-12);
    assert_eq!(price.cache_write_5m_usd_per_mtok, 3.75);
    assert_eq!(price.cache_write_1h_usd_per_mtok, 6.0);

    // 1,000,000 input and 1,000,000 output tokens cost 0.3 + 15.
    let bucket = common::unique("stats-cost").replace(':', "-");
    ls.bucket(&bucket).await.unwrap();
    let priced = LlmStatsRecordOptions { model: Some(model.clone()), ..Default::default() };
    ls.record(&bucket, 1_000_000, 1_000_000, 0, 0.0, 100, Some(&priced)).await.unwrap();
    let stats = ls.get(&bucket).await.unwrap();
    assert!((stats.cost_usd - 15.3).abs() < 1e-6, "cost {}", stats.cost_usd);
    assert_eq!(stats.unpriced_requests, 0);
    // The legacy path takes the caller's cost as a plain decimal.
    ls.record(&bucket, 1, 1, 0, 0.0000025, 1, None).await.unwrap();
    let stats = ls.get(&bucket).await.unwrap();
    assert!((stats.cost_usd - 15.3000025).abs() < 1e-6, "cost {}", stats.cost_usd);
    // An unpriced model counts as unpriced.
    let unknown = LlmStatsRecordOptions { model: Some("no-such-model-xyz".into()), ..Default::default() };
    ls.record(&bucket, 5, 5, 0, 0.0, 1, Some(&unknown)).await.unwrap();
    assert_eq!(ls.get(&bucket).await.unwrap().unpriced_requests, 1);
    for bad in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(ls.record(&bucket, 1, 1, 0, bad, 1, None).await.unwrap_err(), Error::InvalidArgument(_)));
        assert!(matches!(ls.price_set(&model, bad, 1.0, None).await.unwrap_err(), Error::InvalidArgument(_)));
    }
    assert!(ls.price_del(&model).await.unwrap());
    ls.drop(&bucket).await.unwrap();
}

#[tokio::test]
async fn semantic_cache_thresholds_and_vectors_end_to_end() {
    let Some(client) = common::connect().await else { return };
    let sc = client.scache();
    let _ = skip_if_not_licensed!(sc.list().await, "semantic-cache");
    let sc = client.scache();
    let ns = common::unique("sc-float").replace(':', "-");
    sc.create(&ns, 3, Some("cosine"), Some(0.85), None).await.unwrap();

    sc.set(&ns, "q", "answer", &[1.0, 0.0, 0.0], None, None).await.unwrap();
    // cosine(0.9, 0.1, 0) vs (1, 0, 0) is about 0.994.
    let near = [0.9, 0.1, 0.0];
    let (resp, sim) = sc.get(&ns, &near, None).await.unwrap().expect("above 0.85");
    assert_eq!(resp, "answer");
    assert!((sim - 0.9939).abs() < 1e-3, "{sim}");
    // A per-call threshold override is sent as an f32 decimal. Only the call is
    // checked here, not its effect, which can differ between server versions.
    assert!(sc.get(&ns, &near, Some(0.9999)).await.is_ok());
    sc.set_threshold(&ns, 0.9999).await.unwrap();
    assert!(sc.get(&ns, &near, None).await.unwrap().is_none());
    sc.set_threshold(&ns, 0.00001).await.unwrap();
    assert!(sc.get(&ns, &[0.2, 0.9, 0.0], None).await.unwrap().is_some());
    // Out of range thresholds are the server's error, NaN is ours.
    assert!(sc.set_threshold(&ns, 1.5).await.unwrap_err().server_error().is_some());
    assert!(matches!(sc.set_threshold(&ns, f64::NAN).await.unwrap_err(), Error::InvalidArgument(_)));

    // Tiny and large components survive as f32.
    let ns2 = common::unique("sc-float2").replace(':', "-");
    sc.create(&ns2, 4, Some("l2"), Some(0.1), None).await.unwrap();
    let v = [1e-30, 3.0e30, -2.5, 0.1];
    sc.set(&ns2, "q", "tiny-huge", &v, None, None).await.unwrap();
    assert!(matches!(sc.set(&ns2, "bad", "x", &[1e39, 0.0, 0.0, 0.0], None, None).await.unwrap_err(), Error::InvalidArgument(_)));
    sc.drop(&ns).await.unwrap();
    sc.drop(&ns2).await.unwrap();
}

#[tokio::test]
async fn prompt_cache_cost_per_hit_float() {
    let Some(client) = common::connect().await else { return };
    let pc = client.pcache();
    let _ = skip_if_not_licensed!(pc.list().await, "prompt-cache");
    let pc = client.pcache();
    let ns = common::unique("pc-cost").replace(':', "-");
    pc.create(&ns, None, None, Some(0.0025)).await.unwrap();
    pc.set(&ns, "p", "r", None, None, None).await.unwrap();
    for _ in 0..4 {
        assert_eq!(pc.get(&ns, "p").await.unwrap().as_deref(), Some("r"));
    }
    let stats = pc.stats(&ns).await.unwrap();
    assert_eq!(stats.hits, 4);
    assert!((stats.cost_saved_usd - 0.01).abs() < 1e-9, "{}", stats.cost_saved_usd);
    // The saving is hits times the current cost per hit (not a running sum),
    // so after CONFIG SET of a tiny float and one more hit: 5 x 0.00001.
    pc.config_set(&ns, "COST_PER_HIT", 1e-5).await.unwrap();
    pc.get(&ns, "p").await.unwrap();
    let stats = pc.stats(&ns).await.unwrap();
    assert!((stats.cost_saved_usd - 0.00005).abs() < 1e-9, "{}", stats.cost_saved_usd);
    assert!(pc.create(&common::unique("pc-bad").replace(':', "-"), None, None, Some(-1.0)).await.is_err());
    pc.drop(&ns).await.unwrap();
}

#[tokio::test]
async fn absolute_expiry_with_pxat_on_caches_and_agent_memory() {
    let Some(client) = common::connect().await else { return };
    let _ = skip_if_not_licensed!(client.scache().list().await, "semantic-cache");
    let soon = Some(Expiry::Pxat(now_ms() + 1200));

    let ns = common::unique("pxat-sc").replace(':', "-");
    client.scache().create(&ns, 2, Some("cosine"), Some(0.5), None).await.unwrap();
    client.scache().set_with_expiry(&ns, "q", "kept", &[1.0, 0.0], soon, None).await.unwrap();
    client.scache().set_with_expiry(&ns, "q2", "long", &[0.0, 1.0], Some(Expiry::Ttl(600)), None).await.unwrap();

    let pns = common::unique("pxat-pc").replace(':', "-");
    client.pcache().create(&pns, None, None, None).await.unwrap();
    client.pcache().set_with_expiry(&pns, "p", "kept", soon, None, None).await.unwrap();

    let kns = common::unique("pxat-kc").replace(':', "-");
    let guard = client.guard();
    guard.create(&kns, None, None).await.unwrap();
    guard.set_with_expiry(&kns, "k", "kept", soon, None, None).await.unwrap();

    let session = common::unique("pxat-conv").replace(':', "-");
    client.amem().conv_create_with_expiry(&session, Some(5), soon).await.unwrap();
    client.amem().conv_append(&session, "user", "hello").await.unwrap();
    let work = common::unique("pxat-work").replace(':', "-");
    client.amem().work_set_with_expiry(&work, "k", "v", soon).await.unwrap();
    client.amem().work_set_with_expiry(&work, "forever", "v", None).await.unwrap();

    // Present now ...
    assert_eq!(client.scache().get(&ns, &[1.0, 0.0], None).await.unwrap().map(|r| r.0).as_deref(), Some("kept"));
    assert_eq!(client.pcache().get(&pns, "p").await.unwrap().as_deref(), Some("kept"));
    assert_eq!(guard.get(&kns, "k").await.unwrap().as_deref(), Some("kept"));
    assert_eq!(client.amem().work_get(&work, "k").await.unwrap().as_deref(), Some("v"));
    assert_eq!(client.amem().conv_get(&session).await.unwrap().len(), 1);

    // ... gone after the deadline, while the TTL entry and the no-expiry key remain.
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert!(client.scache().get(&ns, &[1.0, 0.0], None).await.unwrap().is_none());
    assert_eq!(client.scache().get(&ns, &[0.0, 1.0], None).await.unwrap().map(|r| r.0).as_deref(), Some("long"));
    assert_eq!(client.pcache().get(&pns, "p").await.unwrap(), None);
    assert_eq!(guard.get(&kns, "k").await.unwrap(), None);
    assert_eq!(client.amem().work_get(&work, "k").await.unwrap(), None);
    assert_eq!(client.amem().work_get(&work, "forever").await.unwrap().as_deref(), Some("v"));
    assert!(client.amem().conv_get(&session).await.map(|t| t.is_empty()).unwrap_or(true));

    // TTL and PXAT together are the server's error, and so is a PXAT it cannot parse.
    let both = client.execute("PCACHE.SET", [pns.as_str(), "p2", "r", "TTL", "10", "PXAT", "4102444800000"]).await.unwrap_err();
    assert!(both.server_error().is_some(), "{both}");
    client.scache().drop(&ns).await.unwrap();
    client.pcache().drop(&pns).await.unwrap();
    guard.drop(&kns).await.unwrap();
    client.amem().work_clear(&work).await.unwrap();
    client.amem().conv_drop(&session).await.unwrap();
}

#[tokio::test]
async fn rate_limit_algorithms_with_float_rates() {
    let Some(client) = common::connect().await else { return };
    let rl = client.ratelimit();
    let _ = skip_if_not_licensed!(rl.list().await, "rate-limiting");
    let rl = client.ratelimit();

    let bucket = common::unique("rl-token").replace(':', "-");
    rl.create(&bucket, 2, 60_000, Some("token_bucket"), Some(0.001), None).await.unwrap();
    let d1 = rl.check(&bucket, "u", None).await.unwrap();
    assert!(d1.allowed);
    assert_eq!(d1.limit, 2);
    assert!(rl.check(&bucket, "u", None).await.unwrap().allowed);
    let d3 = rl.check(&bucket, "u", None).await.unwrap();
    assert!(!d3.allowed, "the bucket is empty and refills at 0.001 tokens per second");
    assert!(d3.retry_after_ms > 0);
    // A cost bigger than the limit is never allowed.
    assert!(!rl.check(&bucket, "other-key", Some(3)).await.unwrap().allowed);
    assert_eq!(rl.stats(&bucket).await.unwrap().algo, "token_bucket");
    rl.drop(&bucket).await.unwrap();

    let leaky = common::unique("rl-leaky").replace(':', "-");
    rl.create(&leaky, 2, 60_000, Some("leaky_bucket"), None, Some(0.5)).await.unwrap();
    assert!(rl.check(&leaky, "u", None).await.unwrap().allowed);
    assert!(rl.check(&leaky, "u", None).await.unwrap().allowed);
    assert!(!rl.check(&leaky, "u", None).await.unwrap().allowed);
    let stats = rl.stats(&leaky).await.unwrap();
    assert_eq!((stats.algo.as_str(), stats.total_allowed, stats.total_denied), ("leaky_bucket", 2, 1));
    rl.drop(&leaky).await.unwrap();

    let sliding = common::unique("rl-slide").replace(':', "-");
    rl.create(&sliding, 3, 60_000, Some("sliding_window"), None, None).await.unwrap();
    assert_eq!(rl.check(&sliding, "u", Some(2)).await.unwrap().remaining, 1);
    assert!(!rl.check(&sliding, "u", Some(2)).await.unwrap().allowed);
    let peek = rl.peek(&sliding, "u").await.unwrap();
    assert_eq!(peek.current, 2);
    rl.reset(&sliding, "u").await.unwrap();
    assert_eq!(rl.peek(&sliding, "u").await.unwrap().current, 0);
    rl.drop(&sliding).await.unwrap();

    // A rate of zero or NaN is refused (the server wants a positive finite number).
    let bad = common::unique("rl-bad").replace(':', "-");
    assert!(rl.create(&bad, 1, 1000, Some("token_bucket"), Some(0.0), None).await.is_err());
    assert!(matches!(rl.create(&bad, 1, 1000, Some("token_bucket"), Some(f64::NAN), None).await.unwrap_err(), Error::InvalidArgument(_)));
}

#[tokio::test]
async fn time_series_values_and_aggregates() {
    let Some(client) = common::connect().await else { return };
    let ts = client.ts();
    let _ = skip_if_not_licensed!(ts.list(None).await, "time-series");
    let ts = client.ts();
    let key = common::unique("ts-vals").replace(':', "-");
    ts.create(&key, None, &[("unit", "test")]).await.unwrap();

    // The server stores doubles and prints six fractional digits.
    for (t, v) in [(1000, 0.00001), (2000, -2.5), (3000, 1e21), (4000, 7.0), (5000, 0.1 + 0.2)] {
        assert_eq!(ts.add(&key, t, v).await.unwrap(), t);
    }
    let range = ts.range(&key, "-", "+", None).await.unwrap();
    assert_eq!(range.len(), 5);
    assert_eq!(range[0].0, 1000);
    assert!((range[0].1 - 0.00001).abs() < 1e-9);
    assert_eq!(range[1], (2000, -2.5));
    assert!((range[2].1 / 1e21 - 1.0).abs() < 1e-12);
    assert_eq!(range[3], (4000, 7.0));
    assert!((range[4].1 - 0.3).abs() < 1e-9);
    assert_eq!(ts.range(&key, 2000, 4000, None).await.unwrap().len(), 3);
    assert_eq!(ts.range(&key, "-", "+", Some(2)).await.unwrap().len(), 2);

    let one_bucket = |agg: &'static str| {
        let ts = client.ts();
        let key = key.clone();
        async move { ts.agg(&key, 0, 10_000, 10_000, agg).await.unwrap() }
    };
    assert_eq!(one_bucket("count").await, vec![(0, 5.0)]);
    let min = one_bucket("min").await;
    assert_eq!(min[0].1, -2.5);
    let max = one_bucket("max").await;
    assert!((max[0].1 / 1e21 - 1.0).abs() < 1e-12);

    let info = ts.info(&key).await.unwrap();
    assert_eq!(info.total_samples, 5);
    assert_eq!(info.first_timestamp, 1000);
    assert_eq!(info.last_timestamp, 5000);
    assert!((info.last_value - 0.3).abs() < 1e-9);
    assert_eq!(info.labels, vec![("unit".to_string(), "test".to_string())]);
    assert!(ts.list(Some(("unit", "test"))).await.unwrap().contains(&key));
    // "*" takes the server's clock.
    let auto = ts.add(&key, "*", 1.0).await.unwrap();
    assert!((auto - now_ms()).abs() < 60_000);
    ts.delete(&key).await.unwrap();
    assert!(ts.info(&key).await.is_err());
}

#[tokio::test]
async fn feature_store_arrays_types_and_set_at() {
    let Some(client) = common::connect().await else { return };
    let fc = client.feature();
    let _ = skip_if_not_licensed!(fc.list().await, "feature-store");
    let fc = client.feature();
    let group = common::unique("fs-types").replace(':', "-");
    fc.create(&group, "all types", &[("a", "int64"), ("s", "double"), ("ia", "int_array"), ("da", "double_array"), ("t", "string")]).await.unwrap();

    fc.set(&group, "e1", [("a", "3"), ("s", "0.00001"), ("ia", "1,2,3"), ("da", "0.5,1e-5"), ("t", "hello world")]).await.unwrap();
    let rec = fc.get(&group, "e1", &[]).await.unwrap().expect("record");
    let f: std::collections::HashMap<String, String> = rec.features.iter().cloned().collect();
    assert_eq!(f["a"], "3");
    assert!((f["s"].parse::<f64>().unwrap() - 0.00001).abs() < 1e-9);
    assert_eq!(f["ia"], "1,2,3");
    assert_eq!(f["t"], "hello world");
    assert!((now_ms() - rec.updated_at_ms).abs() < 60_000);
    let only = fc.get(&group, "e1", &["a", "t"]).await.unwrap().unwrap();
    assert_eq!(only.features.len(), 2);

    // FEATURE.SETAT stamps the record with the given time.
    let at = 1_700_000_000_000;
    fc.set_at(&group, "e2", at, [("a", "5")]).await.unwrap();
    let rec = fc.get(&group, "e2", &["a"]).await.unwrap().expect("record");
    assert_eq!(rec.updated_at_ms, at);
    assert_eq!(rec.features, vec![("a".to_string(), "5".to_string())]);
    assert!(rec.freshness_ms > 1_000_000, "freshness is the age of a 2023 record: {}", rec.freshness_ms);

    let batch = fc.batch(&group, &["e1", "e2", "missing"], &["a"]).await.unwrap();
    assert_eq!(batch.len(), 3);
    assert_eq!(batch[0].as_ref().unwrap().features, vec![("a".to_string(), "3".to_string())]);
    assert_eq!(batch[1].as_ref().unwrap().updated_at_ms, at);
    assert!(batch[2].is_none());
    assert_eq!(fc.stats(&group).await.unwrap().entities, 2);

    // On server 0.3.0 a value that does not parse as the field's type is stored as 0.
    fc.set(&group, "e3", [("a", "not-a-number")]).await.unwrap();
    assert_eq!(fc.get(&group, "e3", &["a"]).await.unwrap().unwrap().features, vec![("a".to_string(), "0".to_string())]);
    fc.drop(&group).await.unwrap();
    assert!(!fc.list().await.unwrap().contains(&group));
}

#[tokio::test]
async fn vector_snapshots_round_trip_binary_blobs() {
    let Some(client) = common::connect().await else { return };
    let vs = client.vsim();
    let _ = skip_if_not_licensed!(vs.list_snapshots().await, "vsim-persist");
    let vs = client.vsim();
    let name = common::unique("snap").replace([':', '.'], "-");
    let blob: Vec<u8> = (0u8..=255).chain([0, 13, 10, 0]).collect();

    let stored = vs.save(&name, &blob).await.unwrap();
    assert!(stored >= blob.len() as i64, "the reply counts the bytes stored (with framing): {stored}");
    assert_eq!(vs.load(&name).await.unwrap(), Some(blob));
    assert!(vs.list_snapshots().await.unwrap().contains(&name));
    vs.save(&name, b"second").await.unwrap();
    assert_eq!(vs.load(&name).await.unwrap(), Some(b"second".to_vec()));
    assert!(vs.drop_snapshot(&name).await.unwrap());
    assert!(!vs.drop_snapshot(&name).await.unwrap());
    assert_eq!(vs.load(&name).await.unwrap(), None);
    // Names with path characters are refused by the server.
    for bad in ["a/b", "a.b", "a:b", ""] {
        assert!(vs.save(bad, b"x").await.unwrap_err().server_error().is_some(), "{bad:?}");
    }
}

#[tokio::test]
async fn alert_settings_are_read_back() {
    let Some(client) = common::connect().await else { return };
    let _ = skip_if_not_licensed!(client.observe().info().await, "observability");
    let alerts = client.alerts();
    let before = alerts.info().await.unwrap();
    // Alert settings are global to the server, so this test puts them back.
    alerts.set_severity("critical").await.unwrap();
    assert_eq!(alerts.info().await.unwrap().min_severity, "critical");
    alerts.set_severity("warning").await.unwrap();
    assert_eq!(alerts.info().await.unwrap().min_severity, "warning");
    assert!(alerts.set_severity("loud").await.is_err());
    alerts.set_severity(&before.min_severity).await.unwrap();

    // The server accepts only https:// webhook URLs and refuses private or loopback hosts.
    assert!(alerts.set_url("http://127.0.0.1:9/hotkv-rust-test").await.unwrap_err().server_error().is_some());
    alerts.set_url("https://example.com/hotkv-rust-test").await.unwrap();
    assert_eq!(alerts.info().await.unwrap().webhook_url, "https://example.com/hotkv-rust-test");
    alerts.set_url("").await.unwrap();
    assert!(!alerts.info().await.unwrap().enabled);

    let metrics = client.observe().metrics().await.unwrap();
    assert!(metrics.uptime_seconds >= 0 && metrics.num_workers >= 1);
    let info = client.observe().info().await.unwrap();
    assert!(info.enterprise_features.contains("prompt-cache") || info.enterprise_features.contains("semantic"), "{}", info.enterprise_features);
}

/// Commands with no typed method, sent with `execute`.
#[tokio::test]
async fn tenancy_iam_audit_encryption_and_replication_reads() {
    let Some(client) = common::connect().await else { return };

    let audit = skip_if_not_licensed!(client.execute("AUDIT.INFO", Vec::<String>::new()).await, "audit").into_pairs().unwrap();
    let get = |pairs: &[(Value, Value)], name: &str| pairs.iter().find(|(k, _)| String::from_value(k.clone()).ok().as_deref() == Some(name)).map(|(_, v)| v.clone());
    assert_eq!(i64::from_value(get(&audit, "chain_valid").unwrap()).unwrap(), 1);
    assert!(i64::from_value(get(&audit, "total_entries").unwrap()).unwrap() >= 0);

    let enc = client.execute("ENCRYPTION.INFO", Vec::<String>::new()).await.unwrap().into_pairs().unwrap();
    assert_eq!(String::from_value(get(&enc, "algorithm").unwrap()).unwrap(), "AES-256-GCM");
    assert!(get(&enc, "enabled").is_some());

    let repl = client.execute("REPLICATION.INFO", Vec::<String>::new()).await.unwrap().into_pairs().unwrap();
    assert_eq!(String::from_value(get(&repl, "role").unwrap()).unwrap(), "primary");
    assert_eq!(i64::from_value(get(&repl, "connected_replicas").unwrap()).unwrap(), 0);

    // These answer with arrays (empty on a fresh server).
    for cmd in ["TENANT.LIST", "IAM.ROLE.LIST", "IAM.SESSION.LIST"] {
        let reply = client.execute(cmd, Vec::<String>::new()).await.unwrap();
        assert!(matches!(reply, Value::Array(_)), "{cmd}: {reply:?}");
    }
    // An unknown tenant is a plain answer or a server error, never NOLICENSE here.
    match client.execute("TENANT.INFO", ["no-such-tenant-rust"]).await {
        Ok(_) => {}
        Err(e) => assert!(!e.is_not_licensed() && e.server_error().is_some(), "{e}"),
    }
}

#[tokio::test]
async fn usage_savings_are_attributed_per_hit_in_the_keyed_caches() {
    let Some(client) = common::connect().await else { return };
    let _ = skip_if_not_licensed!(client.llmstats().price_list().await, "llm-stats");
    let model = common::unique("model").replace(':', "-");
    client.llmstats().price_set(&model, 2.0, 10.0, None).await.unwrap();

    let ns = common::unique("usage-tc").replace(':', "-");
    let tc = client.tool_cache();
    tc.create(&ns, None, None).await.unwrap();
    let usage = Usage::new(model.clone()).input(1_000_000).output(100_000);
    tc.set(&ns, "call", "result", None, None, Some(&usage)).await.unwrap();
    for _ in 0..3 {
        assert_eq!(tc.get(&ns, "call").await.unwrap().as_deref(), Some("result"));
    }
    let stats = tc.stats(&ns).await.unwrap();
    assert_eq!(stats.hits, 3);
    assert_eq!(stats.usage_saved.requests, 3);
    assert_eq!(stats.usage_saved.input_tokens, 3_000_000);
    assert_eq!(stats.usage_saved.output_tokens, 300_000);
    // 3 x (2.00 + 1.00) USD
    assert!((stats.usage_saved.cost_usd - 9.0).abs() < 1e-6, "{}", stats.usage_saved.cost_usd);
    assert_eq!(tc.flush(&ns).await.unwrap(), 1);
    tc.drop(&ns).await.unwrap();
    client.llmstats().price_del(&model).await.unwrap();
}
