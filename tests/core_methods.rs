// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Live checks of the typed core methods the other test files do not reach.
//! Every test asserts the reply value, not just that the call succeeded.

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use hotkv::commands::bitmap::BitOp;
use hotkv::commands::keys::ExpireCondition;
use hotkv::commands::streams::XAddOptions;
use hotkv::commands::zsets::{Aggregate, ZRangeOptions};
use std::collections::HashMap;

use hotkv::{Client, ClientOptions, FromValue, PubSubMessage, Value};

/// A flat or map shaped `XINFO` entry as a field name -> value lookup.
fn fields(v: Value) -> HashMap<String, Value> {
    v.into_pairs().unwrap().into_iter().map(|(k, v)| (String::from_value(k).unwrap(), v)).collect()
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}

#[tokio::test]
async fn binary_safe_keys_and_values() {
    let Some(client) = common::connect().await else { return };
    let key = [common::unique("bin").into_bytes(), vec![0u8, 255, 128, b'\r', b'\n', b' ']].concat();
    let value: Vec<u8> = (0u8..=255).collect();

    client.set(key.as_slice(), value.as_slice()).await.unwrap();
    assert_eq!(client.get_bytes(key.as_slice()).await.unwrap(), Some(value.clone()));
    assert_eq!(client.strlen(key.as_slice()).await.unwrap(), 256);

    // DUMP / RESTORE round trip keeps the bytes and the TTL argument works.
    let dumped = client.dump(key.as_slice()).await.unwrap().expect("dump");
    let copy = common::unique("bin-restored");
    client.restore(&copy, 0, &dumped, false).await.unwrap();
    assert_eq!(client.get_bytes(&copy).await.unwrap(), Some(value));
    assert_eq!(client.ttl(&copy).await.unwrap(), -1);
    // RESTORE onto an existing key fails without REPLACE and succeeds with it.
    let err = client.restore(&copy, 0, &dumped, false).await.unwrap_err();
    assert!(err.server_error().is_some(), "{err}");
    client.restore(&copy, 60_000, &dumped, true).await.unwrap();
    let pttl = client.pttl(&copy).await.unwrap();
    assert!(pttl > 0 && pttl <= 60_000, "{pttl}");

    assert_eq!(client.get_bytes(common::unique("bin-missing")).await.unwrap(), None);
    assert_eq!(client.unlink([copy.as_str()]).await.unwrap(), 1);
    assert_eq!(client.del([key.as_slice()]).await.unwrap(), 1);
}

#[tokio::test]
async fn string_setters_with_expiry() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("setters");

    assert!(client.setnx(&k, "first").await.unwrap());
    assert!(!client.setnx(&k, "second").await.unwrap());
    assert_eq!(client.get(&k).await.unwrap().as_deref(), Some("first"));

    client.set_ex(&k, "ex", 100).await.unwrap();
    let ttl = client.ttl(&k).await.unwrap();
    assert!((1..=100).contains(&ttl), "{ttl}");
    assert_eq!(client.get(&k).await.unwrap().as_deref(), Some("ex"));

    client.set_px(&k, "px", 50_000).await.unwrap();
    let pttl = client.pttl(&k).await.unwrap();
    assert!((1..=50_000).contains(&pttl), "{pttl}");
    assert_eq!(client.get(&k).await.unwrap().as_deref(), Some("px"));
}

#[tokio::test]
async fn expiry_commands_with_absolute_times() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("abs-exp");
    client.set(&k, "v").await.unwrap();

    assert!(client.expireat(&k, now_secs() + 300, None).await.unwrap());
    let ttl = client.ttl(&k).await.unwrap();
    assert!((290..=300).contains(&ttl), "{ttl}");

    // NX only sets when there is no TTL: there is one now, so it must refuse.
    assert!(!client.expireat(&k, now_secs() + 900, Some(ExpireCondition::Nx)).await.unwrap());
    assert!(client.pexpireat(&k, now_millis() + 120_000, Some(ExpireCondition::Lt)).await.unwrap());
    let pttl = client.pttl(&k).await.unwrap();
    assert!((100_000..=120_000).contains(&pttl), "{pttl}");

    // A time in the past deletes the key.
    assert!(client.pexpireat(&k, now_millis() - 1000, None).await.unwrap());
    assert_eq!(client.exists([k.as_str()]).await.unwrap(), 0);
    assert!(!client.expireat(&k, now_secs() + 10, None).await.unwrap());
}

#[tokio::test]
async fn rename_nx_unlink_and_object_encoding() {
    let Some(client) = common::connect().await else { return };
    let a = common::unique("rn-a");
    let b = common::unique("rn-b");
    client.set(&a, "A").await.unwrap();
    client.set(&b, "B").await.unwrap();

    assert!(!client.renamenx(&a, &b).await.unwrap());
    assert_eq!(client.get(&b).await.unwrap().as_deref(), Some("B"));
    client.del([b.as_str()]).await.unwrap();
    assert!(client.renamenx(&a, &b).await.unwrap());
    assert_eq!(client.get(&b).await.unwrap().as_deref(), Some("A"));
    assert_eq!(client.get(&a).await.unwrap(), None);

    // OBJECT ENCODING: a key that exists has some encoding, a missing one is nil.
    let enc = client.object_encoding(&b).await.unwrap();
    assert!(enc.as_deref().is_some_and(|e| !e.is_empty()), "{enc:?}");
    let n = common::unique("rn-int");
    client.set(&n, "12345").await.unwrap();
    assert_eq!(client.object_encoding(&n).await.unwrap().as_deref(), Some("int"));
    assert_eq!(client.object_encoding(common::unique("rn-none")).await.unwrap(), None);

    assert_eq!(client.unlink([b.as_str(), n.as_str(), "does-not-exist-hotkv-rust"]).await.unwrap(), 2);
    assert_eq!(client.exists([b.as_str(), n.as_str()]).await.unwrap(), 0);
}

#[tokio::test]
async fn list_conditional_pushes_and_counted_pops() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("lx");

    assert_eq!(client.lpushx(&k, ["a"]).await.unwrap(), 0);
    assert_eq!(client.rpushx(&k, ["a"]).await.unwrap(), 0);
    assert_eq!(client.exists([k.as_str()]).await.unwrap(), 0);

    client.rpush(&k, ["m"]).await.unwrap();
    assert_eq!(client.lpushx(&k, ["l1", "l2"]).await.unwrap(), 3);
    assert_eq!(client.rpushx(&k, ["r1"]).await.unwrap(), 4);
    assert_eq!(client.lrange(&k, 0, -1).await.unwrap(), vec!["l2", "l1", "m", "r1"]);

    assert_eq!(client.rpop_count(&k, 2).await.unwrap(), Some(vec!["r1".to_string(), "m".to_string()]));
    assert_eq!(client.rpop_count(&k, 10).await.unwrap(), Some(vec!["l1".to_string(), "l2".to_string()]));
    // The list is empty (and gone), so a counted pop is nil.
    assert_eq!(client.rpop_count(&k, 1).await.unwrap(), None);
    assert_eq!(client.lpop_count(&k, 1).await.unwrap(), None);
    assert_eq!(client.lpop(&k).await.unwrap(), None);
}

#[tokio::test]
async fn set_random_and_pop() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("setr");
    client.sadd(&k, ["a", "b", "c", "d"]).await.unwrap();

    let mut distinct = client.srandmember_count(&k, 3).await.unwrap();
    assert_eq!(distinct.len(), 3);
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "positive COUNT returns distinct members");

    let repeated = client.srandmember_count(&k, -10).await.unwrap();
    assert_eq!(repeated.len(), 10, "negative COUNT may repeat and returns |COUNT| members");
    assert!(repeated.iter().all(|m| ["a", "b", "c", "d"].contains(&m.as_str())));
    assert_eq!(client.scard(&k).await.unwrap(), 4);

    let one = client.spop(&k).await.unwrap().expect("member");
    assert!(["a", "b", "c", "d"].contains(&one.as_str()));
    assert_eq!(client.scard(&k).await.unwrap(), 3);
    assert!(!client.sismember(&k, &one).await.unwrap());

    let gone = common::unique("setr-none");
    assert_eq!(client.spop(&gone).await.unwrap(), None);
    assert!(client.srandmember_count(&gone, 3).await.unwrap().is_empty());
}

#[tokio::test]
async fn hash_multi_set_and_random() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("hm");

    client.hmset(&k, [("a", "1"), ("b", "2"), ("c", "3")]).await.unwrap();
    assert_eq!(client.hlen(&k).await.unwrap(), 3);
    assert_eq!(
        client.hmget(&k, ["a", "b", "c", "d"]).await.unwrap(),
        vec![Some("1".to_string()), Some("2".to_string()), Some("3".to_string()), None]
    );

    let mut fields = client.hrandfield_count(&k, 3).await.unwrap();
    fields.sort();
    assert_eq!(fields, vec!["a", "b", "c"]);
    let mut with_values = client.hrandfield_with_values(&k, 3).await.unwrap();
    with_values.sort();
    assert_eq!(
        with_values,
        vec![("a".to_string(), "1".to_string()), ("b".to_string(), "2".to_string()), ("c".to_string(), "3".to_string())]
    );
    assert_eq!(client.hrandfield_count(&k, -5).await.unwrap().len(), 5);
}

#[tokio::test]
async fn hash_field_ttl_family() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("httl");
    client.hset(&k, [("f1", "v"), ("f2", "v"), ("f3", "v"), ("f4", "v"), ("f5", "v")]).await.unwrap();

    // No TTL yet: -1 for fields that exist, -2 for ones that do not.
    assert_eq!(client.httl(&k, &["f1", "nope"]).await.unwrap(), vec![-1, -2]);

    assert_eq!(client.hexpire(&k, 100, None, &["f1", "nope"]).await.unwrap(), vec![1, -2]);
    let ttl = client.httl(&k, &["f1"]).await.unwrap();
    assert!((1..=100).contains(&ttl[0]), "{ttl:?}");

    // NX refuses (f1 has a TTL), XX refuses for a field without one.
    assert_eq!(client.hexpire(&k, 500, Some(ExpireCondition::Nx), &["f1", "f2"]).await.unwrap(), vec![0, 1]);
    assert_eq!(client.hexpire(&k, 500, Some(ExpireCondition::Xx), &["f3"]).await.unwrap(), vec![0]);

    assert_eq!(client.hpexpire(&k, 50_000, None, &["f3"]).await.unwrap(), vec![1]);
    let pttl = client.hpttl(&k, &["f3", "f4"]).await.unwrap();
    assert!((1..=50_000).contains(&pttl[0]), "{pttl:?}");
    assert_eq!(pttl[1], -1);

    let at_s = now_secs() + 1000;
    assert_eq!(client.hexpireat(&k, at_s, None, &["f4"]).await.unwrap(), vec![1]);
    assert_eq!(client.hexpiretime(&k, &["f4", "f5", "nope"]).await.unwrap(), vec![at_s, -1, -2]);

    let at_ms = now_millis() + 2_000_000;
    assert_eq!(client.hpexpireat(&k, at_ms, None, &["f5"]).await.unwrap(), vec![1]);
    assert_eq!(client.hpexpiretime(&k, &["f5"]).await.unwrap(), vec![at_ms]);

    assert_eq!(client.hpersist(&k, &["f5", "f1", "nope"]).await.unwrap(), vec![1, 1, -2]);
    assert_eq!(client.httl(&k, &["f5", "f1"]).await.unwrap(), vec![-1, -1]);
    assert_eq!(client.hpersist(&k, &["f5"]).await.unwrap(), vec![-1]);

    // A time in the past removes the field.
    assert_eq!(client.hpexpireat(&k, now_millis() - 1000, None, &["f2"]).await.unwrap(), vec![2]);
    assert!(!client.hexists(&k, "f2").await.unwrap());
}

#[tokio::test]
async fn sorted_set_ranges_by_score_and_lex() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("zr");
    client.zadd(&k, [(1.0, "a"), (2.0, "b"), (3.0, "c"), (4.0, "d"), (5.0, "e")]).await.unwrap();

    assert_eq!(
        client.zrangebyscore_withscores(&k, "2", "4", None).await.unwrap(),
        vec![("b".to_string(), 2.0), ("c".to_string(), 3.0), ("d".to_string(), 4.0)]
    );
    assert_eq!(
        client.zrangebyscore_withscores(&k, "(1", "+inf", Some((1, 2))).await.unwrap(),
        vec![("c".to_string(), 3.0), ("d".to_string(), 4.0)]
    );
    assert_eq!(client.zrevrangebyscore(&k, "4", "2", None).await.unwrap(), vec!["d", "c", "b"]);
    assert_eq!(client.zrevrangebyscore(&k, "+inf", "-inf", Some((0, 2))).await.unwrap(), vec!["e", "d"]);
    assert_eq!(
        client.zrevrange_withscores(&k, 0, 1).await.unwrap(),
        vec![("e".to_string(), 5.0), ("d".to_string(), 4.0)]
    );

    assert_eq!(client.zrank_withscore(&k, "c").await.unwrap(), Some((2, 3.0)));
    assert_eq!(client.zrevrank_withscore(&k, "c").await.unwrap(), Some((2, 3.0)));
    assert_eq!(client.zrank_withscore(&k, "e").await.unwrap(), Some((4, 5.0)));
    assert_eq!(client.zrevrank_withscore(&k, "e").await.unwrap(), Some((0, 5.0)));
    assert_eq!(client.zrank_withscore(&k, "nope").await.unwrap(), None);
    assert_eq!(client.zrevrank_withscore(&k, "nope").await.unwrap(), None);

    let lex = common::unique("zlex");
    client.zadd(&lex, [(0.0, "a"), (0.0, "b"), (0.0, "c"), (0.0, "d"), (0.0, "e")]).await.unwrap();
    assert_eq!(client.zlexcount(&lex, "[b", "(e").await.unwrap(), 3);
    assert_eq!(client.zlexcount(&lex, "-", "+").await.unwrap(), 5);
    assert_eq!(client.zrangebylex(&lex, "[b", "[d", None).await.unwrap(), vec!["b", "c", "d"]);
    assert_eq!(client.zrangebylex(&lex, "-", "+", Some((1, 2))).await.unwrap(), vec!["b", "c"]);
    assert_eq!(client.zrevrangebylex(&lex, "[d", "[b", None).await.unwrap(), vec!["d", "c", "b"]);
    assert_eq!(client.zrevrangebylex(&lex, "+", "-", Some((0, 1))).await.unwrap(), vec!["e"]);

    assert_eq!(client.zremrangebylex(&lex, "[a", "(c").await.unwrap(), 2);
    assert_eq!(client.zrange(&lex, 0, -1, &ZRangeOptions::default()).await.unwrap(), vec!["c", "d", "e"]);

    assert_eq!(client.zremrangebyscore(&k, "-inf", "(2").await.unwrap(), 1);
    assert_eq!(client.zremrangebyrank(&k, 0, 0).await.unwrap(), 1);
    assert_eq!(client.zrange(&k, 0, -1, &ZRangeOptions::default()).await.unwrap(), vec!["c", "d", "e"]);
}

#[tokio::test]
async fn sorted_set_random_and_pop_counts() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("zp");
    client.zadd(&k, [(1.0, "a"), (2.0, "b"), (3.0, "c")]).await.unwrap();

    let one = client.zrandmember(&k).await.unwrap().expect("member");
    assert!(["a", "b", "c"].contains(&one.as_str()));
    let mut all = client.zrandmember_count(&k, 3).await.unwrap();
    all.sort();
    assert_eq!(all, vec!["a", "b", "c"]);
    let mut scored = client.zrandmember_with_scores(&k, 3).await.unwrap();
    scored.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(scored, vec![("a".to_string(), 1.0), ("b".to_string(), 2.0), ("c".to_string(), 3.0)]);
    assert_eq!(client.zrandmember(common::unique("zp-none")).await.unwrap(), None);

    assert_eq!(client.zpopmax_count(&k, 2).await.unwrap(), vec![("c".to_string(), 3.0), ("b".to_string(), 2.0)]);
    assert_eq!(client.zpopmin_count(&k, 5).await.unwrap(), vec![("a".to_string(), 1.0)]);
    assert!(client.zpopmax_count(&k, 2).await.unwrap().is_empty());
    assert_eq!(client.zpopmin(&k).await.unwrap(), None);
}

#[tokio::test]
async fn sorted_set_store_operations_with_weights_and_aggregate() {
    let Some(client) = common::connect().await else { return };
    let a = common::unique("zs-a");
    let b = common::unique("zs-b");
    let dest = common::unique("zs-dest");
    client.zadd(&a, [(1.0, "x"), (2.0, "y"), (3.0, "z")]).await.unwrap();
    client.zadd(&b, [(10.0, "y"), (20.0, "z"), (30.0, "w")]).await.unwrap();

    assert_eq!(client.zinterstore(&dest, [a.as_str(), b.as_str()], None, None).await.unwrap(), 2);
    assert_eq!(
        client.zrange_withscores(&dest, 0, -1, &ZRangeOptions::default()).await.unwrap(),
        vec![("y".to_string(), 12.0), ("z".to_string(), 23.0)]
    );

    let n = client.zinterstore(&dest, [a.as_str(), b.as_str()], Some(&[2.0, 0.5]), Some(Aggregate::Max)).await.unwrap();
    assert_eq!(n, 2);
    assert_eq!(
        client.zrange_withscores(&dest, 0, -1, &ZRangeOptions::default()).await.unwrap(),
        vec![("y".to_string(), 5.0), ("z".to_string(), 10.0)]
    );

    let n = client.zunionstore(&dest, [a.as_str(), b.as_str()], Some(&[1.0, 1.0]), Some(Aggregate::Min)).await.unwrap();
    assert_eq!(n, 4);
    assert_eq!(client.zscore(&dest, "y").await.unwrap(), Some(2.0));
    assert_eq!(client.zscore(&dest, "w").await.unwrap(), Some(30.0));

    // Infinite weights and scores are valid for sorted sets.
    let n = client.zunionstore(&dest, [a.as_str()], Some(&[f64::INFINITY]), None).await.unwrap();
    assert_eq!(n, 3);
    assert_eq!(client.zscore(&dest, "x").await.unwrap(), Some(f64::INFINITY));
}

#[tokio::test]
async fn stream_groups_consumers_and_pending() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("sg");
    let id1 = client.xadd(&k, "*", [("n", "1")], &XAddOptions::default()).await.unwrap();
    let id2 = client.xadd(&k, "*", [("n", "2")], &XAddOptions::default()).await.unwrap();
    let id3 = client.xadd(&k, "*", [("n", "3")], &XAddOptions::default()).await.unwrap();

    client.xgroup_create(&k, "g", "0", false).await.unwrap();
    assert!(client.xgroup_createconsumer(&k, "g", "c1").await.unwrap());
    assert!(!client.xgroup_createconsumer(&k, "g", "c1").await.unwrap(), "second create reports 0");

    let read = client.xreadgroup("g", "c1", [(k.as_str(), ">")], Some(2), None, false).await.unwrap().expect("data");
    assert_eq!(read[0].entries.iter().map(|e| e.id.clone()).collect::<Vec<_>>(), vec![id1.clone(), id2.clone()]);

    let groups = client.xinfo_groups(&k).await.unwrap();
    assert_eq!(groups.len(), 1);
    let group = fields(groups.into_iter().next().unwrap());
    assert_eq!(String::from_value(group["name"].clone()).unwrap(), "g");
    assert_eq!(i64::from_value(group["consumers"].clone()).unwrap(), 1);
    assert_eq!(i64::from_value(group["pending"].clone()).unwrap(), 2);

    let consumers = client.xinfo_consumers(&k, "g").await.unwrap();
    assert_eq!(consumers.len(), 1);
    let c1 = fields(consumers.into_iter().next().unwrap());
    assert_eq!(String::from_value(c1["name"].clone()).unwrap(), "c1");
    assert_eq!(i64::from_value(c1["pending"].clone()).unwrap(), 2);

    let pending = client.xpending_extended(&k, "g", "-", "+", 10, None).await.unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].id, id1);
    assert_eq!(pending[0].consumer, "c1");
    assert_eq!(pending[0].deliveries, 1);
    let only_c2 = client.xpending_extended(&k, "g", "-", "+", 10, Some("c2")).await.unwrap();
    assert!(only_c2.is_empty());

    let ids = client.xclaim_justid(&k, "g", "c2", 0, [id1.as_str(), id2.as_str()]).await.unwrap();
    assert_eq!(ids, vec![id1.clone(), id2.clone()]);
    let by_c2 = client.xpending_extended(&k, "g", "-", "+", 10, Some("c2")).await.unwrap();
    assert_eq!(by_c2.len(), 2);

    // Deleting a consumer reports how many pending entries it dropped.
    assert_eq!(client.xgroup_delconsumer(&k, "g", "c2").await.unwrap(), 2);
    assert_eq!(client.xgroup_delconsumer(&k, "g", "never-existed").await.unwrap(), 0);

    // SETID rewinds the group so the entries can be delivered again.
    client.xgroup_setid(&k, "g", "0").await.unwrap();
    let again = client.xreadgroup("g", "c3", [(k.as_str(), ">")], None, None, false).await.unwrap().expect("data");
    assert_eq!(again[0].entries.len(), 3);
    assert_eq!(again[0].entries[2].id, id3);
    assert!(client.xgroup_destroy(&k, "g").await.unwrap());
}

#[tokio::test]
async fn bitmaps_exact_values() {
    let Some(client) = common::connect().await else { return };
    let a = common::unique("bm-a");
    let b = common::unique("bm-b");
    let dest = common::unique("bm-d");
    client.set(&a, "foobar").await.unwrap();
    assert_eq!(client.bitcount(&a, None).await.unwrap(), 26);
    assert_eq!(client.bitcount(&a, Some((0, 0, false))).await.unwrap(), 4);
    assert_eq!(client.bitcount(&a, Some((1, 1, false))).await.unwrap(), 6);
    assert_eq!(client.bitcount(&a, Some((5, 30, true))).await.unwrap(), 17);

    client.set(&b, &b"\x00\xff\xf0"[..]).await.unwrap();
    assert_eq!(client.bitpos(&b, true, None, None, false).await.unwrap(), 8);
    assert_eq!(client.bitpos(&b, false, Some(1), None, false).await.unwrap(), 16 + 4);
    assert_eq!(client.bitpos(&b, true, Some(13), Some(-1), true).await.unwrap(), 13);

    client.set(&b, "abcdef").await.unwrap();
    assert_eq!(client.bitop(BitOp::And, &dest, [a.as_str(), b.as_str()]).await.unwrap(), 6);
    assert_eq!(client.get(&dest).await.unwrap().as_deref(), Some("`bc`ab"));
    assert_eq!(client.bitop(BitOp::Xor, &dest, [a.as_str(), a.as_str()]).await.unwrap(), 6);
    assert_eq!(client.bitcount(&dest, None).await.unwrap(), 0);
    assert_eq!(client.bitop(BitOp::Not, &dest, [a.as_str()]).await.unwrap(), 6);
    assert_eq!(client.get_bytes(&dest).await.unwrap().unwrap().first().copied(), Some(!b'f'));
}

#[tokio::test]
async fn config_set_and_get_round_trip() {
    let Some(client) = common::connect().await else { return };
    // Both values are harmless runtime settings; the original is restored.
    let before = client.config_get("slowlog-max-len").await.unwrap();
    let original = before.get("slowlog-max-len").cloned().expect("slowlog-max-len is readable");
    client.config_set("slowlog-max-len", "321").await.unwrap();
    assert_eq!(client.config_get("slowlog-max-len").await.unwrap().get("slowlog-max-len").map(String::as_str), Some("321"));
    client.config_set("slowlog-max-len", original.as_str()).await.unwrap();
    assert_eq!(client.config_get("slowlog-max-len").await.unwrap().get("slowlog-max-len"), Some(&original));

    let err = client.config_set("no-such-parameter-hotkv", "1").await.unwrap_err();
    assert!(err.server_error().is_some(), "{err}");
}

#[tokio::test]
async fn select_and_flushdb_on_a_private_connection() {
    let Some(url) = std::env::var("HOTKV_TEST_URL").ok() else {
        println!("skipping: HOTKV_TEST_URL is not set");
        return;
    };
    // One pooled connection, so SELECT and FLUSHDB hit the same session and
    // never touch the database the other tests share (db 0).
    let mut opts = ClientOptions::from_url(&url).unwrap();
    opts.pool_size = 1;
    let client = Client::with_options(opts).await.unwrap();

    client.select(9).await.unwrap();
    client.flushdb(false).await.unwrap();
    assert_eq!(client.dbsize().await.unwrap(), 0);
    client.set("only-in-db-9", "x").await.unwrap();
    client.set("also-in-db-9", "y").await.unwrap();
    assert_eq!(client.dbsize().await.unwrap(), 2);

    client.select(10).await.unwrap();
    assert_eq!(client.get("only-in-db-9").await.unwrap(), None);
    assert_eq!(client.dbsize().await.unwrap(), 0);

    client.select(9).await.unwrap();
    assert_eq!(client.get("only-in-db-9").await.unwrap().as_deref(), Some("x"));
    client.flushdb(true).await.unwrap();
    // FLUSHDB ASYNC returns before the keys are gone on some servers.
    for _ in 0..50 {
        if client.dbsize().await.unwrap() == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(client.dbsize().await.unwrap(), 0);

    let err = client.select(9999).await.unwrap_err();
    assert!(err.server_error().is_some(), "{err}");
    client.select(0).await.unwrap();
}

#[tokio::test]
async fn script_flush_forgets_cached_scripts() {
    let Some(client) = common::connect().await else { return };
    let body = format!("return '{}'", common::unique("flushme"));
    let sha = client.script_load(&body).await.unwrap();
    assert_eq!(client.script_exists([sha.as_str()]).await.unwrap(), vec![true]);
    client.script_flush().await.unwrap();
    assert_eq!(client.script_exists([sha.as_str()]).await.unwrap(), vec![false]);
}

#[tokio::test]
async fn pattern_subscriptions_and_unsubscribe() {
    let Some(client) = common::connect().await else { return };
    let prefix = common::unique("psub");
    let pattern = format!("{prefix}:*");
    let channel = format!("{prefix}:room1");

    let mut sub = client.psubscribe([pattern.as_str()]).await.unwrap();
    match sub.next().await.unwrap().unwrap() {
        PubSubMessage::PSubscribed { pattern: p, count } => {
            assert_eq!(p, pattern);
            assert_eq!(count, 1);
        }
        other => panic!("expected PSubscribed, got {other:?}"),
    }
    let n = i64::from_value(client.execute("PUBLISH", [channel.as_str(), "hello"]).await.unwrap()).unwrap();
    assert_eq!(n, 1, "one subscriber matched the pattern");
    match sub.next().await.unwrap().unwrap() {
        PubSubMessage::PMessage { pattern: p, channel: c, payload } => {
            assert_eq!(p, pattern);
            assert_eq!(c, channel);
            assert_eq!(payload, b"hello".to_vec());
        }
        other => panic!("expected PMessage, got {other:?}"),
    }
    sub.punsubscribe([pattern.as_str()]).await.unwrap();
    match sub.next().await.unwrap().unwrap() {
        PubSubMessage::PUnsubscribed { pattern: p, count } => {
            assert_eq!(p, pattern);
            assert_eq!(count, 0);
        }
        other => panic!("expected PUnsubscribed, got {other:?}"),
    }
    let n = i64::from_value(client.execute("PUBLISH", [channel.as_str(), "nobody"]).await.unwrap()).unwrap();
    assert_eq!(n, 0);

    let ch = common::unique("unsub");
    let mut sub = client.subscribe([ch.as_str()]).await.unwrap();
    assert!(matches!(sub.next().await.unwrap().unwrap(), PubSubMessage::Subscribed { .. }));
    sub.unsubscribe([ch.as_str()]).await.unwrap();
    match sub.next().await.unwrap().unwrap() {
        PubSubMessage::Unsubscribed { channel: c, count } => {
            assert_eq!(c, ch);
            assert_eq!(count, 0);
        }
        other => panic!("expected Unsubscribed, got {other:?}"),
    }

    let sch = common::unique("sunsub");
    let mut sub = client.ssubscribe([sch.as_str()]).await.unwrap();
    assert!(matches!(sub.next().await.unwrap().unwrap(), PubSubMessage::SSubscribed { .. }));
    sub.sunsubscribe([sch.as_str()]).await.unwrap();
    match sub.next().await.unwrap().unwrap() {
        PubSubMessage::SUnsubscribed { channel: c, count } => {
            assert_eq!(c, sch);
            assert_eq!(count, 0);
        }
        other => panic!("expected SUnsubscribed, got {other:?}"),
    }
}

#[tokio::test]
async fn unwatch_lets_a_transaction_commit_after_the_key_changed() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("unwatch");
    client.set(&k, "orig").await.unwrap();

    let mut tx = client.transaction().await.unwrap();
    tx.watch([k.as_str()]).await.unwrap();
    client.set(&k, "changed").await.unwrap();
    tx.unwatch().await.unwrap();
    tx.multi().await.unwrap();
    tx.queue(hotkv::args::Cmd::new("SET").arg(&k).arg("from-tx")).await.unwrap();
    let results = tx.exec().await.unwrap().expect("UNWATCH cleared the watch, so EXEC commits");
    assert_eq!(results.len(), 1);
    assert_eq!(client.get(&k).await.unwrap().as_deref(), Some("from-tx"));
}
