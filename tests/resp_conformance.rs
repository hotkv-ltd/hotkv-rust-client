// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Checks that HGETALL, HRANDFIELD WITHVALUES, SMEMBERS, ZSCORE, ZRANGE
//! WITHSCORES, ZPOPMIN with a count, ZRANDMEMBER WITHSCORES, XREAD, XINFO
//! STREAM, CONFIG GET and INFO decode to the same shape whether the
//! connection negotiated RESP3 (the default) or RESP2.

mod common;

use hotkv::{Client, ClientOptions, FromValue};

async fn connect(resp3: bool) -> Option<Client> {
    let Ok(url) = std::env::var("HOTKV_TEST_URL") else {
        println!("skipping: HOTKV_TEST_URL is not set");
        return None;
    };
    let mut opts = ClientOptions::from_url(&url).expect("valid HOTKV_TEST_URL");
    opts.resp3 = resp3;
    Some(Client::with_options(opts).await.expect("connect"))
}

async fn run_checks(client: &Client, label: &str) {
    let hkey = common::unique(&format!("conform-{label}-hash"));
    let skey = common::unique(&format!("conform-{label}-set"));
    let zkey = common::unique(&format!("conform-{label}-zset"));
    let xkey = common::unique(&format!("conform-{label}-stream"));

    client.hset(&hkey, [("f1", "v1"), ("f2", "v2")]).await.unwrap();
    let hgetall = client.hgetall(&hkey).await.unwrap();
    assert_eq!(hgetall.len(), 2);
    assert_eq!(hgetall.get("f1"), Some(&"v1".to_string()));
    assert_eq!(hgetall.get("f2"), Some(&"v2".to_string()));

    let fvs = client.hrandfield_with_values(&hkey, 2).await.unwrap();
    assert_eq!(fvs.len(), 2);
    for (f, v) in &fvs {
        assert_eq!(v, if f == "f1" { "v1" } else { "v2" });
    }

    client.sadd(&skey, ["a", "b", "c"]).await.unwrap();
    let mut members = client.smembers(&skey).await.unwrap();
    members.sort();
    assert_eq!(members, vec!["a", "b", "c"]);

    client.zadd(&zkey, [(1.0, "a"), (2.0, "b"), (3.0, "c")]).await.unwrap();
    assert_eq!(client.zscore(&zkey, "b").await.unwrap(), Some(2.0));

    let with_scores = client
        .zrange_withscores(&zkey, 0, -1, &hotkv::commands::zsets::ZRangeOptions::default())
        .await
        .unwrap();
    assert_eq!(with_scores, vec![("a".to_string(), 1.0), ("b".to_string(), 2.0), ("c".to_string(), 3.0)]);

    let popped = client.zpopmin_count(&zkey, 2).await.unwrap();
    assert_eq!(popped, vec![("a".to_string(), 1.0), ("b".to_string(), 2.0)]);

    client.zadd(&zkey, [(1.0, "a"), (2.0, "b")]).await.unwrap();
    let rand_scored = client.zrandmember_with_scores(&zkey, 3).await.unwrap();
    assert_eq!(rand_scored.len(), 3);

    let id1 = client.xadd(&xkey, "*", [("field1", "v1")], &Default::default()).await.unwrap();
    let id2 = client.xadd(&xkey, "*", [("field2", "v2")], &Default::default()).await.unwrap();
    let streams = client.xread([(xkey.as_str(), "0")], None, None).await.unwrap().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].stream, xkey);
    assert_eq!(streams[0].entries.len(), 2);
    assert_eq!(streams[0].entries[0].id, id1);
    assert_eq!(streams[0].entries[0].fields, vec![("field1".to_string(), "v1".to_string())]);
    assert_eq!(streams[0].entries[1].id, id2);

    let info = client.xinfo_stream(&xkey).await.unwrap();
    let pairs = info.into_pairs().unwrap();
    let length = pairs
        .into_iter()
        .find(|(k, _)| String::from_value(k.clone()).ok().as_deref() == Some("length"))
        .map(|(_, v)| i64::from_value(v).unwrap());
    assert_eq!(length, Some(2));

    let cfg = client.config_get("maxmemory").await.unwrap();
    assert!(cfg.contains_key("maxmemory"));

    let info_str = client.info(None).await.unwrap();
    assert!(!info_str.starts_with("txt:") && !info_str.starts_with("mkd:"), "INFO must be plain text without an encoding prefix");
    assert!(info_str.contains("hotkv_version"));
}

#[tokio::test]
async fn resp3() {
    let Some(client) = connect(true).await else { return };
    run_checks(&client, "resp3").await;
}

#[tokio::test]
async fn resp2() {
    let Some(client) = connect(false).await else { return };
    run_checks(&client, "resp2").await;
}
