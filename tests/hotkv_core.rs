// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

mod common;

#[tokio::test]
async fn tag_commands() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("tagged");
    // Tag values are unique per run too: TAG.MEMBERS/TAG.INTER search the
    // whole keyspace, not just this test's keys, so a shared literal tag
    // (e.g. "red") would pick up unrelated keys left by earlier runs.
    let red = common::unique("tag-red");
    let small = common::unique("tag-small");
    client.set(&k, "v").await.unwrap();

    assert_eq!(client.tag().set(&k, [red.as_str(), small.as_str()]).await.unwrap(), 2);
    assert!(client.tag().exists(&k, &red).await.unwrap());
    assert!(!client.tag().exists(&k, "no-such-tag").await.unwrap());
    assert_eq!(client.tag().count(&k).await.unwrap(), 2);

    let mut tags = client.tag().get(&k).await.unwrap();
    tags.sort();
    let mut expected_tags = vec![red.clone(), small.clone()];
    expected_tags.sort();
    assert_eq!(tags, expected_tags);

    let k2 = common::unique("tagged2");
    client.set(&k2, "v").await.unwrap();
    client.tag().set(&k2, [red.as_str()]).await.unwrap();

    let mut members = client.tag().members(&red).await.unwrap();
    members.sort();
    let mut expected = vec![k.clone(), k2.clone()];
    expected.sort();
    assert_eq!(members, expected);

    let inter = client.tag().inter([red.as_str(), small.as_str()]).await.unwrap();
    assert_eq!(inter, vec![k.clone()]);

    assert_eq!(client.tag().rem(&k, [small.as_str()]).await.unwrap(), 1);
    assert_eq!(client.tag().count(&k).await.unwrap(), 1);

    client.del([k.as_str(), k2.as_str()]).await.unwrap();
}

#[tokio::test]
async fn nexpire_family() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("nexpire");
    client.set(&k, "v").await.unwrap();

    assert_eq!(client.nttl(&k).await.unwrap(), -1);
    assert!(client.nexpire(&k, 60_000_000_000).await.unwrap()); // 60s in ns
    let ttl_ns = client.nttl(&k).await.unwrap();
    assert!(ttl_ns > 0 && ttl_ns <= 60_000_000_000);

    let missing = common::unique("nexpire-missing");
    assert!(!client.nexpire(&missing, 1_000_000_000).await.unwrap());
    assert_eq!(client.nttl(&missing).await.unwrap(), -2);

    let future_ns = (std::time::SystemTime::now() + std::time::Duration::from_secs(120))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    assert!(client.nexpireat(&k, future_ns).await.unwrap());
}

#[tokio::test]
async fn hotkeys() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("hot");
    client.set(&k, "v").await.unwrap();
    for _ in 0..3 {
        client.get(&k).await.unwrap();
    }

    let top = client.hotkeys_top(Some(5)).await.unwrap();
    // HOTKEYS tracking is best effort/sampled server side; just check the
    // call round trips with the documented shape.
    for (_key, count) in &top {
        assert!(*count >= 0);
    }
    client.hotkeys_reset().await.unwrap();
}

/// HOTKEYS needs `hotkey-tracking yes` on the server, which is off by
/// default, so this runs against `HOTKV_TEST_HOTKEYS_URL`, a server started
/// with it. Without that variable the test is skipped.
#[tokio::test]
async fn hotkeys_with_tracking_enabled() {
    let Ok(url) = std::env::var("HOTKV_TEST_HOTKEYS_URL") else {
        println!("skipping: HOTKV_TEST_HOTKEYS_URL is not set");
        return;
    };
    let client = hotkv::Client::connect(&url).await.unwrap();
    client.hotkeys_reset().await.unwrap();
    assert!(client.hotkeys_top(None).await.unwrap().is_empty());

    let hot = common::unique("hot-key");
    let warm = common::unique("warm-key");
    for _ in 0..6 {
        client.set(&hot, "v").await.unwrap();
    }
    client.get(&warm).await.unwrap();
    client.get(&warm).await.unwrap();

    let top = client.hotkeys_top(Some(5)).await.unwrap();
    assert_eq!(top, vec![(hot.clone(), 6), (warm.clone(), 2)]);
    assert_eq!(client.hotkeys_top(Some(1)).await.unwrap(), vec![(hot, 6)]);
    client.hotkeys_reset().await.unwrap();
    assert!(client.hotkeys_top(Some(5)).await.unwrap().is_empty());
}

#[tokio::test]
async fn gcra_rate_limiter() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("gcra");

    let d1 = client.gcra().limit(&k, 1000, 2, None).await.unwrap();
    assert!(!d1.denied);
    assert_eq!(d1.limit, 2);

    let d2 = client.gcra().limit(&k, 1000, 2, None).await.unwrap();
    assert!(!d2.denied);

    let d3 = client.gcra().limit(&k, 1000, 2, None).await.unwrap();
    assert!(d3.denied);
    assert!(d3.retry_after_ms >= 0);

    assert_eq!(client.gcra().reset([k.as_str()]).await.unwrap(), 1);
}

#[tokio::test]
async fn vsim_core() {
    let Some(client) = common::connect().await else { return };
    let index = common::unique("vsimidx");

    client.vsim().create_index(&index, 3, Some("cosine"), None, None, None).await.unwrap();

    assert!(client.vsim().set(&index, "a", &[1.0, 0.0, 0.0]).await.unwrap());
    assert!(client.vsim().set(&index, "b", &[0.0, 1.0, 0.0]).await.unwrap());
    assert!(!client.vsim().set(&index, "a", &[1.0, 0.0, 0.1]).await.unwrap()); // replaces

    let got = client.vsim().get(&index, "a").await.unwrap();
    assert_eq!(got.map(|v| v.len()), Some(3));
    assert!(client.vsim().get(&index, "missing").await.unwrap().is_none());

    let results = client.vsim().search(&index, 2, &[1.0, 0.0, 0.0], &[], None).await.unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].0, "a");

    let names = client.vsim().list_indexes().await.unwrap();
    assert!(names.contains(&index));

    let info = client.vsim().info(&index).await.unwrap();
    assert_eq!(info.name, index);
    assert_eq!(info.dimensions, 3);
    assert_eq!(info.metric, "cosine");

    assert!(client.vsim().del(&index, "b").await.unwrap());
    client.vsim().drop_index(&index).await.unwrap();
}

#[tokio::test]
async fn vsim_persistence_gated() {
    let Some(client) = common::connect().await else { return };
    // Snapshot names may not contain '/', '\', '.' or ':'.
    let name = common::unique("vsimsnap").replace(':', "-");

    match client.vsim().save(&name, b"blob-data").await {
        Ok(n) => {
            assert!(n > 0);
            let loaded = client.vsim().load(&name).await.unwrap();
            assert_eq!(loaded, Some(b"blob-data".to_vec()));
            let names = client.vsim().list_snapshots().await.unwrap();
            assert!(names.contains(&name));
            assert!(client.vsim().drop_snapshot(&name).await.unwrap());
        }
        Err(e) if e.is_not_licensed() => {
            common::unlicensed("vsim_persistence_gated", &e);
        }
        Err(e) => panic!("unexpected error: {e}"),
    }
}
