// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

mod common;

use hotkv::commands::keys::ExpireCondition;
use hotkv::commands::strings::{GetExOptions, SetOptions};

#[tokio::test]
async fn strings_round_trip() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("str");

    assert_eq!(client.get(&k).await.unwrap(), None);
    client.set(&k, "hello").await.unwrap();
    assert_eq!(client.get(&k).await.unwrap(), Some("hello".to_string()));

    assert_eq!(client.append(&k, " world").await.unwrap(), 11);
    assert_eq!(client.get(&k).await.unwrap(), Some("hello world".to_string()));
    assert_eq!(client.strlen(&k).await.unwrap(), 11);

    assert_eq!(client.getrange(&k, 0, 4).await.unwrap(), "hello");
    client.setrange(&k, 6, "there").await.unwrap();
    assert_eq!(client.get(&k).await.unwrap(), Some("hello there".to_string()));

    let old = client.getset(&k, "new value").await.unwrap();
    assert_eq!(old, Some("hello there".to_string()));

    let deleted = client.getdel(&k).await.unwrap();
    assert_eq!(deleted, Some("new value".to_string()));
    assert_eq!(client.get(&k).await.unwrap(), None);

    assert_eq!(client.get_bytes(&k).await.unwrap(), None);
}

#[tokio::test]
async fn set_conditions_and_options() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("setopts");

    assert!(client.set_nx(&k, "a").await.unwrap());
    assert!(!client.set_nx(&k, "b").await.unwrap());
    assert_eq!(client.get(&k).await.unwrap(), Some("a".to_string()));

    assert!(client.set_xx(&k, "c").await.unwrap());
    assert_eq!(client.get(&k).await.unwrap(), Some("c".to_string()));

    let k2 = common::unique("setopts2");
    assert!(!client.set_xx(&k2, "nope").await.unwrap());

    // Full SET options (NX/XX/GET/EX/PX/EXAT/PXAT/KEEPTTL).
    let opts = SetOptions { get: true, ..Default::default() };
    let v = client.set_options(&k, "d", &opts).await.unwrap();
    assert_eq!(v, hotkv::Value::Bulk(b"c".to_vec()));
    assert_eq!(client.get(&k).await.unwrap(), Some("d".to_string()));

    let opts = SetOptions { keepttl: true, ..Default::default() };
    client.expire(&k, 100, None).await.unwrap();
    client.set_options(&k, "e", &opts).await.unwrap();
    let ttl = client.ttl(&k).await.unwrap();
    assert!(ttl > 0, "expected TTL to survive SET KEEPTTL, got {ttl}");

    let future = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 100;
    let opts = SetOptions { exat: Some(future), ..Default::default() };
    client.set_options(&k, "f", &opts).await.unwrap();
    let ttl = client.ttl(&k).await.unwrap();
    assert!(ttl > 0 && ttl <= 100, "expected a positive TTL after SET EXAT, got {ttl}");
}

#[tokio::test]
async fn setex_psetex_and_getex() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("setex");

    client.setex(&k, 100, "v1").await.unwrap();
    let ttl = client.ttl(&k).await.unwrap();
    assert!(ttl > 0 && ttl <= 100);

    client.psetex(&k, 100_000, "v2").await.unwrap();
    assert_eq!(client.get(&k).await.unwrap(), Some("v2".to_string()));

    let got = client.getex(&k, &GetExOptions { persist: true, ..Default::default() }).await.unwrap();
    assert_eq!(got, Some("v2".to_string()));
    assert_eq!(client.ttl(&k).await.unwrap(), -1);
}

#[tokio::test]
async fn counters() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("counter");

    assert_eq!(client.incr(&k).await.unwrap(), 1);
    assert_eq!(client.incrby(&k, 9).await.unwrap(), 10);
    assert_eq!(client.decr(&k).await.unwrap(), 9);
    assert_eq!(client.decrby(&k, 4).await.unwrap(), 5);
    let f = client.incrbyfloat(&k, 0.5).await.unwrap();
    assert!((f - 5.5).abs() < 1e-9);
}

#[tokio::test]
async fn multi_key_strings() {
    let Some(client) = common::connect().await else { return };
    let k1 = common::unique("m1");
    let k2 = common::unique("m2");
    let k3 = common::unique("m3");

    client.mset([(k1.as_str(), "a"), (k2.as_str(), "b")]).await.unwrap();
    let vals = client.mget([k1.as_str(), k2.as_str(), k3.as_str()]).await.unwrap();
    assert_eq!(vals, vec![Some("a".to_string()), Some("b".to_string()), None]);

    assert!(!client.msetnx([(k1.as_str(), "z")]).await.unwrap());
    let k4 = common::unique("m4");
    let k5 = common::unique("m5");
    assert!(client.msetnx([(k4.as_str(), "x"), (k5.as_str(), "y")]).await.unwrap());
}

#[tokio::test]
async fn generic_key_commands() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("generic");
    let k2 = common::unique("generic2");

    assert_eq!(client.exists([k.as_str()]).await.unwrap(), 0);
    client.set(&k, "v").await.unwrap();
    assert_eq!(client.exists([k.as_str()]).await.unwrap(), 1);
    assert_eq!(client.key_type(&k).await.unwrap(), "string");

    assert!(client.expire(&k, 1000, None).await.unwrap());
    assert!(client.expire(&k, 2000, Some(ExpireCondition::Gt)).await.unwrap());
    assert!(!client.expire(&k, 500, Some(ExpireCondition::Gt)).await.unwrap());
    assert!(client.persist(&k).await.unwrap());
    assert_eq!(client.ttl(&k).await.unwrap(), -1);

    assert!(client.pexpire(&k, 500_000, None).await.unwrap());
    assert!(client.pttl(&k).await.unwrap() > 0);

    client.rename(&k, &k2).await.unwrap();
    assert_eq!(client.get(&k2).await.unwrap(), Some("v".to_string()));
    assert_eq!(client.get(&k).await.unwrap(), None);

    let k3 = common::unique("generic3");
    assert!(client.copy(&k2, &k3, None, false).await.unwrap());
    assert_eq!(client.get(&k3).await.unwrap(), Some("v".to_string()));

    assert_eq!(client.touch([k2.as_str(), k3.as_str()]).await.unwrap(), 2);

    let dump = client.dump(&k3).await.unwrap();
    assert!(dump.is_some());

    assert_eq!(client.del([k2.as_str(), k3.as_str()]).await.unwrap(), 2);
    assert_eq!(client.exists([k2.as_str(), k3.as_str()]).await.unwrap(), 0);
}

#[tokio::test]
async fn keys_and_randomkey() {
    let Some(client) = common::connect().await else { return };
    let prefix = common::unique("pattern");
    let k1 = format!("{prefix}:a");
    let k2 = format!("{prefix}:b");
    client.set(&k1, "1").await.unwrap();
    client.set(&k2, "2").await.unwrap();

    let mut found = client.keys(format!("{prefix}:*")).await.unwrap();
    found.sort();
    assert_eq!(found, vec![k1.clone(), k2.clone()]);

    assert!(client.randomkey().await.unwrap().is_some());
    client.del([k1.as_str(), k2.as_str()]).await.unwrap();
}
