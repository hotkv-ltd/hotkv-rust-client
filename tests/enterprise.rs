// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

mod common;

#[tokio::test]
async fn ratelimit_commands() {
    let Some(client) = common::connect().await else { return };
    let name = common::unique("rl");
    let rl = client.ratelimit();

    let result = rl.create(&name, 2, 60_000, Some("sliding_window"), None, None).await;
    let () = skip_if_not_licensed!(result, "rate-limiting");

    let key = "user1";
    let d1 = rl.check(&name, key, None).await.unwrap();
    assert!(d1.allowed);
    let d2 = rl.check(&name, key, None).await.unwrap();
    assert!(d2.allowed);
    let d3 = rl.check(&name, key, None).await.unwrap();
    assert!(!d3.allowed);

    let peeked = rl.peek(&name, key).await.unwrap();
    assert!(!peeked.allowed);

    let stats = rl.stats(&name).await.unwrap();
    assert_eq!(stats.name, name);
    assert_eq!(stats.limit, 2);
    assert!(stats.total_allowed >= 2);

    rl.reset(&name, key).await.unwrap();
    let after_reset = rl.check(&name, key, None).await.unwrap();
    assert!(after_reset.allowed);

    assert!(rl.list().await.unwrap().contains(&name));
    rl.drop(&name).await.unwrap();
}

#[tokio::test]
async fn timeseries_commands() {
    let Some(client) = common::connect().await else { return };
    let key = common::unique("ts");
    let ts = client.ts();

    let result = ts.create(&key, Some(0), &[("sensor", "temp")]).await;
    let () = skip_if_not_licensed!(result, "time-series");
    let t1 = ts.add(&key, 1000, 20.5).await.unwrap();
    assert_eq!(t1, 1000);
    ts.add(&key, 2000, 21.5).await.unwrap();
    ts.add(&key, 3000, 19.0).await.unwrap();

    let range = ts.range(&key, 0, 5000, None).await.unwrap();
    assert_eq!(range.len(), 3);
    assert_eq!(range[0], (1000, 20.5));

    let agg = ts.agg(&key, 0, 5000, 5000, "avg").await.unwrap();
    assert!(!agg.is_empty());

    let info = ts.info(&key).await.unwrap();
    assert_eq!(info.key, key);
    assert_eq!(info.total_samples, 3);
    assert_eq!(info.labels, vec![("sensor".to_string(), "temp".to_string())]);

    ts.retention(&key, 3_600_000).await.unwrap();

    let dest = common::unique("ts-downsampled");
    ts.create(&dest, None, &[]).await.unwrap();
    ts.downsample(&key, &dest, "avg", 1000).await.unwrap();

    let listed = ts.list(Some(("sensor", "temp"))).await.unwrap();
    assert!(listed.contains(&key));

    ts.delete(&dest).await.unwrap();
    ts.delete(&key).await.unwrap();
}

#[tokio::test]
async fn feature_store_commands() {
    let Some(client) = common::connect().await else { return };
    let group = common::unique("features");
    let fc = client.feature();

    let result = fc.create(&group, "user features", &[("age", "int64"), ("score", "double")]).await;
    let () = skip_if_not_licensed!(result, "feature-store");

    fc.set(&group, "user1", [("age", "30"), ("score", "0.5")]).await.unwrap();

    let record = fc.get(&group, "user1", &[]).await.unwrap().expect("record present");
    let features: std::collections::HashMap<String, String> = record.features.into_iter().collect();
    assert_eq!(features.get("age").map(String::as_str), Some("30"));
    // The server reformats typed values through its own schema (a double
    // field prints with trailing zeros), so compare numerically rather
    // than expecting a byte-exact round trip of what was sent.
    let score: f64 = features.get("score").unwrap().parse().unwrap();
    assert!((score - 0.5).abs() < 1e-9);

    assert!(fc.get(&group, "no-such-entity", &[]).await.unwrap().is_none());

    let batch = fc.batch(&group, &["user1", "no-such-entity"], &[]).await.unwrap();
    assert!(batch[0].is_some());
    assert!(batch[1].is_none());

    let mut schema = fc.schema(&group).await.unwrap();
    schema.sort();
    let mut expected_schema = vec![("age".to_string(), "int64".to_string()), ("score".to_string(), "double".to_string())];
    expected_schema.sort();
    assert_eq!(schema, expected_schema);

    assert!(fc.list().await.unwrap().contains(&group));

    let stats = fc.stats(&group).await.unwrap();
    assert_eq!(stats.entities, 1);

    fc.drop(&group).await.unwrap();
}

#[tokio::test]
async fn observe_and_alerts() {
    let Some(client) = common::connect().await else { return };

    let info = skip_if_not_licensed!(client.observe().info().await, "observability");
    assert!(info.uptime_seconds >= 0);

    let metrics = client.observe().metrics().await.unwrap();
    assert!(metrics.uptime_seconds >= 0);

    let alerts_info = client.alerts().info().await.unwrap();
    assert!(!alerts_info.supported_providers.is_empty());

    let off = client.alerts().off().await.unwrap();
    assert!(off.starts_with("OK"));
    let on = client.alerts().on().await.unwrap();
    assert!(on.starts_with("OK"));

    match client.alerts().test(Some("info"), Some("hotkv-rust integration test")).await {
        Ok(reply) => assert!(reply.starts_with("OK")),
        Err(e) => println!("alerts test rejected (rate limited/deduped/no destination configured): {e}"),
    }
}
