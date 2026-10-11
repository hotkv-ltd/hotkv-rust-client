// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

mod common;

use std::collections::HashMap;

use hotkv::commands::zsets::{ZAddOptions, ZRangeBy, ZRangeOptions};

#[tokio::test]
async fn lists() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("list");

    assert_eq!(client.rpush(&k, ["a", "b", "c"]).await.unwrap(), 3);
    assert_eq!(client.lpush(&k, ["z"]).await.unwrap(), 4);
    assert_eq!(client.llen(&k).await.unwrap(), 4);
    assert_eq!(client.lrange(&k, 0, -1).await.unwrap(), vec!["z", "a", "b", "c"]);
    assert_eq!(client.lindex(&k, 1).await.unwrap(), Some("a".to_string()));

    client.lset(&k, 1, "A").await.unwrap();
    assert_eq!(client.lindex(&k, 1).await.unwrap(), Some("A".to_string()));

    assert_eq!(client.linsert(&k, true, "A", "pre-A").await.unwrap(), 5);
    assert_eq!(client.lrange(&k, 0, -1).await.unwrap(), vec!["z", "pre-A", "A", "b", "c"]);

    assert_eq!(client.lrem(&k, 1, "pre-A").await.unwrap(), 1);
    client.ltrim(&k, 0, 2).await.unwrap();
    assert_eq!(client.lrange(&k, 0, -1).await.unwrap(), vec!["z", "A", "b"]);

    assert_eq!(client.lpop(&k).await.unwrap(), Some("z".to_string()));
    assert_eq!(client.rpop(&k).await.unwrap(), Some("b".to_string()));

    let k2 = common::unique("list2");
    client.rpush(&k2, ["x", "y"]).await.unwrap();
    let moved = client.rpoplpush(&k2, &k).await.unwrap();
    assert_eq!(moved, Some("y".to_string()));

    let k3 = common::unique("list3");
    client.rpush(&k3, ["1", "2", "3"]).await.unwrap();
    let lmoved = client.lmove(&k3, &k, true, false).await.unwrap();
    assert_eq!(lmoved, Some("1".to_string()));

    // LPOP/RPOP COUNT.
    let k4 = common::unique("list4");
    client.rpush(&k4, ["a", "b", "c", "d"]).await.unwrap();
    let popped = client.lpop_count(&k4, 2).await.unwrap();
    assert_eq!(popped, Some(vec!["a".to_string(), "b".to_string()]));
}

#[tokio::test]
async fn sets() {
    let Some(client) = common::connect().await else { return };
    let k1 = common::unique("set1");
    let k2 = common::unique("set2");

    assert_eq!(client.sadd(&k1, ["a", "b", "c"]).await.unwrap(), 3);
    assert_eq!(client.sadd(&k2, ["b", "c", "d"]).await.unwrap(), 3);
    assert_eq!(client.scard(&k1).await.unwrap(), 3);
    assert!(client.sismember(&k1, "a").await.unwrap());
    assert!(!client.sismember(&k1, "z").await.unwrap());
    assert_eq!(client.smismember(&k1, ["a", "z"]).await.unwrap(), vec![true, false]);

    let mut union = client.sunion([k1.as_str(), k2.as_str()]).await.unwrap();
    union.sort();
    assert_eq!(union, vec!["a", "b", "c", "d"]);

    let mut inter = client.sinter([k1.as_str(), k2.as_str()]).await.unwrap();
    inter.sort();
    assert_eq!(inter, vec!["b", "c"]);

    let diff = client.sdiff([k1.as_str(), k2.as_str()]).await.unwrap();
    assert_eq!(diff, vec!["a".to_string()]);

    let dest = common::unique("setdest");
    assert_eq!(client.sunionstore(&dest, [k1.as_str(), k2.as_str()]).await.unwrap(), 4);
    assert_eq!(client.sinterstore(&dest, [k1.as_str(), k2.as_str()]).await.unwrap(), 2);
    assert_eq!(client.sdiffstore(&dest, [k1.as_str(), k2.as_str()]).await.unwrap(), 1);

    assert!(client.smove(&k1, &k2, "a").await.unwrap());
    assert!(!client.sismember(&k1, "a").await.unwrap());
    assert!(client.sismember(&k2, "a").await.unwrap());

    assert_eq!(client.srem(&k2, ["a"]).await.unwrap(), 1);

    let mut members = client.smembers(&k1).await.unwrap();
    members.sort();
    assert_eq!(members, vec!["b", "c"]);

    let popped = client.spop_count(&k1, 1).await.unwrap();
    assert_eq!(popped.len(), 1);
    assert!(client.srandmember(&k2).await.unwrap().is_some());
}

#[tokio::test]
async fn hashes() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("hash");

    assert_eq!(client.hset(&k, [("f1", "v1"), ("f2", "v2")]).await.unwrap(), 2);
    assert_eq!(client.hget(&k, "f1").await.unwrap(), Some("v1".to_string()));
    assert!(client.hexists(&k, "f1").await.unwrap());
    assert_eq!(client.hlen(&k).await.unwrap(), 2);

    let all = client.hgetall(&k).await.unwrap();
    let expected: HashMap<String, String> = [("f1".to_string(), "v1".to_string()), ("f2".to_string(), "v2".to_string())].into();
    assert_eq!(all, expected);

    let mut keys = client.hkeys(&k).await.unwrap();
    keys.sort();
    assert_eq!(keys, vec!["f1", "f2"]);
    let mut vals = client.hvals(&k).await.unwrap();
    vals.sort();
    assert_eq!(vals, vec!["v1", "v2"]);

    assert_eq!(client.hmget(&k, ["f1", "missing"]).await.unwrap(), vec![Some("v1".to_string()), None]);

    assert!(client.hsetnx(&k, "f3", "v3").await.unwrap());
    assert!(!client.hsetnx(&k, "f3", "other").await.unwrap());

    assert_eq!(client.hincrby(&k, "counter", 5).await.unwrap(), 5);
    let f = client.hincrbyfloat(&k, "fcounter", 1.5).await.unwrap();
    assert!((f - 1.5).abs() < 1e-9);

    assert!(client.hrandfield(&k).await.unwrap().is_some());
    assert_eq!(client.hdel(&k, ["f1"]).await.unwrap(), 1);

    // Hash field TTL (the full family is in core_methods.rs).
    assert_eq!(client.hexpire(&k, 1000, None, &["f2"]).await.unwrap(), vec![1]);
    let ttl = client.httl(&k, &["f2"]).await.unwrap();
    assert_eq!(ttl.len(), 1);
    assert!((1..=1000).contains(&ttl[0]), "{ttl:?}");
}

#[tokio::test]
async fn sorted_sets() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("zset");

    assert_eq!(client.zadd(&k, [(1.0, "a"), (2.0, "b"), (3.0, "c")]).await.unwrap(), 3);
    assert_eq!(client.zcard(&k).await.unwrap(), 3);
    assert_eq!(client.zscore(&k, "b").await.unwrap(), Some(2.0));
    assert_eq!(client.zmscore(&k, ["a", "missing"]).await.unwrap(), vec![Some(1.0), None]);
    assert_eq!(client.zrank(&k, "b").await.unwrap(), Some(1));
    assert_eq!(client.zrevrank(&k, "b").await.unwrap(), Some(1));

    assert_eq!(client.zrange(&k, 0, -1, &ZRangeOptions::default()).await.unwrap(), vec!["a", "b", "c"]);
    let ws = client.zrange_withscores(&k, 0, -1, &ZRangeOptions::default()).await.unwrap();
    assert_eq!(ws, vec![("a".to_string(), 1.0), ("b".to_string(), 2.0), ("c".to_string(), 3.0)]);

    assert_eq!(client.zcount(&k, "1", "2").await.unwrap(), 2);
    assert_eq!(client.zincrby(&k, 10.0, "a").await.unwrap(), 11.0);

    let added = client
        .zadd_options(&k, ZAddOptions { nx: true, ..Default::default() }, [(99.0, "a")])
        .await
        .unwrap();
    assert_eq!(added, 0); // NX blocked the update; "a" already exists
    assert_eq!(client.zscore(&k, "a").await.unwrap(), Some(11.0));

    let incr = client.zadd_incr(&k, ZAddOptions::default(), "a", 1.0).await.unwrap();
    assert_eq!(incr, Some(12.0));

    assert_eq!(client.zrem(&k, ["a"]).await.unwrap(), 1);

    let popped = client.zpopmin(&k).await.unwrap();
    assert_eq!(popped, Some(("b".to_string(), 2.0)));

    let k2 = common::unique("zset2");
    client.zadd(&k2, [(5.0, "x"), (6.0, "y")]).await.unwrap();
    let dest = common::unique("zdest");
    let n = client.zunionstore(&dest, [k.as_str(), k2.as_str()], None, None).await.unwrap();
    assert!(n >= 3);

    // Exercise BYSCORE and REV individually, plus the legacy ZRANGEBYSCORE form.
    let k3 = common::unique("zrangeopts");
    client.zadd(&k3, [(1.0, "a"), (2.0, "b"), (3.0, "c")]).await.unwrap();
    let by_score = ZRangeOptions { by: Some(ZRangeBy::Score), ..Default::default() };
    assert_eq!(client.zrange(&k3, "-inf", "+inf", &by_score).await.unwrap(), vec!["a", "b", "c"]);
    let rev_only = ZRangeOptions { rev: true, ..Default::default() };
    assert_eq!(client.zrange(&k3, 0, -1, &rev_only).await.unwrap(), vec!["c", "b", "a"]);
    assert_eq!(client.zrangebyscore(&k3, "1", "2", None).await.unwrap(), vec!["a", "b"]);
    assert_eq!(client.zrevrange(&k3, 0, -1).await.unwrap(), vec!["c", "b", "a"]);

    let popped_max = client.zpopmax(&k3).await.unwrap();
    assert_eq!(popped_max, Some(("c".to_string(), 3.0)));
}

#[tokio::test]
async fn zrange_byscore_rev_limit() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("zrange-byscore-rev-limit");
    client.zadd(&k, [(1.0, "a"), (2.0, "b"), (3.0, "c"), (4.0, "d")]).await.unwrap();

    // ZRANGE key max min BYSCORE REV LIMIT offset count: in REV order the
    // higher bound is given first on the wire.
    let opts = ZRangeOptions { by: Some(ZRangeBy::Score), rev: true, limit: Some((1, 2)) };
    let page = client.zrange(&k, "4", "1", &opts).await.unwrap();
    assert_eq!(page, vec!["c", "b"]);

    let lex = common::unique("zrange-bylex-rev");
    client.zadd(&lex, [(0.0, "a"), (0.0, "b"), (0.0, "c")]).await.unwrap();
    let lex_opts = ZRangeOptions { by: Some(ZRangeBy::Lex), rev: true, ..Default::default() };
    let all = client.zrange(&lex, "+", "-", &lex_opts).await.unwrap();
    assert_eq!(all, vec!["c", "b", "a"]);
}

#[tokio::test]
async fn zrangestore_byscore_and_bylex() {
    let Some(client) = common::connect().await else { return };
    let src = common::unique("zrangestore-src");
    let dst = common::unique("zrangestore-dst");
    client.zadd(&src, [(1.0, "a"), (2.0, "b"), (3.0, "c"), (4.0, "d")]).await.unwrap();

    let by_score = ZRangeOptions { by: Some(ZRangeBy::Score), ..Default::default() };
    let n = client.zrangestore(&dst, &src, "2", "3", &by_score).await.unwrap();
    assert_eq!(n, 2);
    assert_eq!(client.zrange(&dst, 0, -1, &ZRangeOptions::default()).await.unwrap(), vec!["b", "c"]);

    let rev_limit = ZRangeOptions { by: Some(ZRangeBy::Score), rev: true, limit: Some((0, 2)) };
    let n = client.zrangestore(&dst, &src, "4", "1", &rev_limit).await.unwrap();
    assert_eq!(n, 2);
    assert_eq!(client.zrange(&dst, 0, -1, &ZRangeOptions::default()).await.unwrap(), vec!["c", "d"]);

    let lex = common::unique("zrangestore-lex");
    client.zadd(&lex, [(0.0, "a"), (0.0, "b"), (0.0, "c")]).await.unwrap();
    let by_lex = ZRangeOptions { by: Some(ZRangeBy::Lex), ..Default::default() };
    let n = client.zrangestore(&dst, &lex, "-", "+", &by_lex).await.unwrap();
    assert_eq!(n, 3);
    assert_eq!(client.zrange(&dst, 0, -1, &ZRangeOptions::default()).await.unwrap(), vec!["a", "b", "c"]);
}
