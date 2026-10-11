// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Commands this crate has no typed method for, sent through the generic
//! `Client::command` / `Client::execute`. Each assertion checks the reply
//! value the server gives (HotKV 0.3.0).

mod common;

use hotkv::args::Cmd;
use hotkv::{Client, ClientOptions, Error, FromValue, Value};

/// A RESP2 client with one pooled connection. RESP2 gives the reply shapes
/// asserted below (RESP3 replies some of these as booleans, doubles and maps),
/// and the single connection keeps session state such as client tracking.
async fn single_connection() -> Option<Client> {
    let url = std::env::var("HOTKV_TEST_URL").ok()?;
    let mut opts = ClientOptions::from_url(&url).unwrap();
    opts.pool_size = 1;
    opts.resp3 = false;
    Some(Client::with_options(opts).await.unwrap())
}

async fn ints(client: &Client, name: &str, args: &[&str]) -> i64 {
    i64::from_value(client.execute(name, args.iter().copied()).await.unwrap()).unwrap()
}

async fn strings(client: &Client, name: &str, args: &[&str]) -> Vec<String> {
    Vec::<String>::from_value(client.execute(name, args.iter().copied()).await.unwrap()).unwrap()
}

#[tokio::test]
async fn newer_hash_and_string_commands() {
    let Some(client) = single_connection().await else { return };
    let h = common::unique("g-hash");

    client.hset(&h, [("a", "1"), ("b", "2"), ("c", "3")]).await.unwrap();
    // HGETDEL returns the values and deletes the fields.
    let got = client.execute("HGETDEL", [h.as_str(), "FIELDS", "2", "a", "nope"]).await.unwrap();
    assert_eq!(got, Value::Array(vec![Value::Bulk(b"1".to_vec()), Value::Nil]));
    assert!(!client.hexists(&h, "a").await.unwrap());

    // HGETEX returns the values and sets a TTL on the fields.
    let got = client.execute("HGETEX", [h.as_str(), "EX", "100", "FIELDS", "1", "b"]).await.unwrap();
    assert_eq!(got, Value::Array(vec![Value::Bulk(b"2".to_vec())]));
    assert!((1..=100).contains(&client.httl(&h, &["b"]).await.unwrap()[0]));

    // HSETEX sets fields together with a TTL; the reply is 1 when all were set.
    assert_eq!(ints(&client, "HSETEX", &[&h, "EX", "100", "FIELDS", "2", "x", "10", "y", "20"]).await, 1);
    assert_eq!(client.hget(&h, "y").await.unwrap().as_deref(), Some("20"));
    assert_eq!(client.httl(&h, &["x", "c"]).await.unwrap().len(), 2);
    assert_eq!(ints(&client, "HSTRLEN", &[&h, "x"]).await, 2);

    // MSETEX sets several keys with one expiry.
    let (k1, k2) = (common::unique("g-mset1"), common::unique("g-mset2"));
    assert_eq!(ints(&client, "MSETEX", &["2", &k1, "v1", &k2, "v2", "EX", "100"]).await, 1);
    assert!((1..=100).contains(&client.ttl(&k1).await.unwrap()));
    assert_eq!(client.get(&k2).await.unwrap().as_deref(), Some("v2"));

    // DELEX deletes only when the value matches; DIGEST is a stable hex digest.
    let k = common::unique("g-delex");
    client.set(&k, "dv").await.unwrap();
    assert_eq!(ints(&client, "DELEX", &[&k, "IFEQ", "other"]).await, 0);
    assert!(client.exists([k.as_str()]).await.unwrap() == 1);
    let digest = String::from_value(client.execute("DIGEST", [k.as_str()]).await.unwrap()).unwrap();
    assert_eq!(digest.len(), 16);
    assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()), "{digest}");
    assert_eq!(ints(&client, "DELEX", &[&k, "IFEQ", "dv"]).await, 1);
    assert_eq!(client.exists([k.as_str()]).await.unwrap(), 0);
}

#[tokio::test]
async fn expiretime_object_and_lists_without_typed_methods() {
    let Some(client) = single_connection().await else { return };
    let k = common::unique("g-exp");
    client.set(&k, "v").await.unwrap();
    assert_eq!(ints(&client, "EXPIRETIME", &[&k]).await, -1);
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64 + 500;
    client.expireat(&k, at, None).await.unwrap();
    assert_eq!(ints(&client, "EXPIRETIME", &[&k]).await, at);
    assert_eq!(ints(&client, "PEXPIRETIME", &[&k]).await, at * 1000);
    assert_eq!(ints(&client, "OBJECT", &["REFCOUNT", &k]).await, 1);
    assert!(ints(&client, "OBJECT", &["IDLETIME", &k]).await >= 0);

    let l = common::unique("g-list");
    client.rpush(&l, ["a", "b", "c", "b"]).await.unwrap();
    assert_eq!(ints(&client, "LPOS", &[&l, "b"]).await, 1);
    assert_eq!(ints(&client, "LPOS", &[&l, "b", "RANK", "-1"]).await, 3);
    assert_eq!(strings(&client, "LPOS", &[&l, "b", "COUNT", "0"]).await.len(), 2);
    let popped = client.execute("LMPOP", ["1", l.as_str(), "LEFT", "COUNT", "2"]).await.unwrap();
    assert_eq!(
        popped,
        Value::Array(vec![Value::Bulk(l.clone().into_bytes()), Value::Array(vec![Value::Bulk(b"a".to_vec()), Value::Bulk(b"b".to_vec())])])
    );

    let n = common::unique("g-sort");
    client.rpush(&n, ["10", "9", "100"]).await.unwrap();
    assert_eq!(strings(&client, "SORT", &[&n]).await, vec!["9", "10", "100"]);
    assert_eq!(strings(&client, "SORT", &[&n, "DESC", "LIMIT", "0", "2"]).await, vec!["100", "10"]);
    assert_eq!(strings(&client, "SORT_RO", &[&n, "ALPHA"]).await, vec!["10", "100", "9"]);
}

#[tokio::test]
async fn set_operations_and_publish_without_typed_methods() {
    let Some(client) = single_connection().await else { return };
    let (z1, z2) = (common::unique("g-z1"), common::unique("g-z2"));
    client.zadd(&z1, [(1.0, "a"), (2.0, "b")]).await.unwrap();
    client.zadd(&z2, [(2.0, "b"), (3.0, "c")]).await.unwrap();
    assert_eq!(strings(&client, "ZINTER", &["2", &z1, &z2, "WITHSCORES"]).await, vec!["b", "4"]);
    assert_eq!(strings(&client, "ZUNION", &["2", &z1, &z2]).await, vec!["a", "c", "b"]);
    assert_eq!(strings(&client, "ZDIFF", &["2", &z1, &z2]).await, vec!["a"]);
    assert_eq!(ints(&client, "ZINTERCARD", &["2", &z1, &z2]).await, 1);
    let popped = client.execute("ZMPOP", ["2", z1.as_str(), z2.as_str(), "MIN"]).await.unwrap();
    assert_eq!(
        popped,
        Value::Array(vec![Value::Bulk(z1.clone().into_bytes()), Value::Array(vec![Value::Array(vec![Value::Bulk(b"a".to_vec()), Value::Bulk(b"1".to_vec())])])])
    );

    let (s1, s2) = (common::unique("g-s1"), common::unique("g-s2"));
    client.sadd(&s1, ["a", "b", "c"]).await.unwrap();
    client.sadd(&s2, ["b", "c", "d"]).await.unwrap();
    assert_eq!(ints(&client, "SINTERCARD", &["2", &s1, &s2]).await, 2);
    assert_eq!(ints(&client, "SINTERCARD", &["2", &s1, &s2, "LIMIT", "1"]).await, 1);

    // Nobody is subscribed, so PUBLISH reaches zero receivers.
    assert_eq!(ints(&client, "PUBLISH", &[&common::unique("g-chan"), "x"]).await, 0);
}

#[tokio::test]
async fn probabilistic_structures_without_typed_methods() {
    let Some(client) = single_connection().await else { return };
    let bf = common::unique("g-bf");
    assert_eq!(client.execute("BF.RESERVE", [bf.as_str(), "0.01", "100"]).await.unwrap(), Value::SimpleString("OK".into()));
    assert_eq!(ints(&client, "BF.ADD", &[&bf, "x"]).await, 1);
    assert_eq!(ints(&client, "BF.ADD", &[&bf, "x"]).await, 0);
    assert_eq!(client.execute("BF.MEXISTS", [bf.as_str(), "x", "y"]).await.unwrap(), Value::Array(vec![Value::Integer(1), Value::Integer(0)]));
    assert_eq!(ints(&client, "BF.CARD", &[&bf]).await, 1);

    let cms = common::unique("g-cms");
    client.execute("CMS.INITBYDIM", [cms.as_str(), "100", "5"]).await.unwrap();
    assert_eq!(client.execute("CMS.INCRBY", [cms.as_str(), "a", "3"]).await.unwrap(), Value::Array(vec![Value::Integer(3)]));
    assert_eq!(client.execute("CMS.QUERY", [cms.as_str(), "a", "b"]).await.unwrap(), Value::Array(vec![Value::Integer(3), Value::Integer(0)]));

    let tk = common::unique("g-topk");
    client.execute("TOPK.RESERVE", [tk.as_str(), "3"]).await.unwrap();
    client.execute("TOPK.ADD", [tk.as_str(), "a", "b", "a"]).await.unwrap();
    assert_eq!(strings(&client, "TOPK.LIST", &[&tk, "WITHCOUNT"]).await, vec!["a", "2", "b", "1"]);

    // A server error on a command with no typed method is a normal Error::Server.
    let err = client.execute("BF.ADD", [common::unique("g-missing-type").as_str()]).await.unwrap_err();
    assert!(err.server_error().is_some(), "{err}");
}

#[tokio::test]
async fn stream_admin_commands_without_typed_methods() {
    let Some(client) = single_connection().await else { return };
    let s = common::unique("g-stream");
    client.xadd(&s, "1-1", [("f", "v")], &Default::default()).await.unwrap();
    assert_eq!(client.execute("XSETID", [s.as_str(), "5-0"]).await.unwrap(), Value::SimpleString("OK".into()));
    let info = client.xinfo_stream(&s).await.unwrap().into_pairs().unwrap();
    let last = info
        .into_iter()
        .find(|(k, _)| String::from_value(k.clone()).ok().as_deref() == Some("last-generated-id"))
        .map(|(_, v)| String::from_value(v).unwrap());
    assert_eq!(last.as_deref(), Some("5-0"));
    assert_eq!(client.execute("XCFGSET", [s.as_str(), "IDMP-DURATION", "100"]).await.unwrap(), Value::SimpleString("OK".into()));
}

#[tokio::test]
async fn client_tracking_commands_on_one_connection() {
    let Some(client) = single_connection().await else { return };
    // The tracking state lives on the connection, hence the pool of one.
    assert_eq!(ints(&client, "CLIENT", &["GETREDIR"]).await, -1);
    assert_eq!(client.execute("CLIENT", ["TRACKING", "ON"]).await.unwrap(), Value::SimpleString("OK".into()));
    let field = |info: &[(Value, Value)], name: &str| info.iter().find(|(k, _)| String::from_value(k.clone()).ok().as_deref() == Some(name)).map(|(_, v)| v.clone());
    let info = client.execute("CLIENT", ["TRACKINGINFO"]).await.unwrap().into_pairs().unwrap();
    assert_eq!(Vec::<String>::from_value(field(&info, "flags").unwrap()).unwrap(), vec!["on"]);
    // Tracking is on without a redirect target, which reads as 0 (-1 is "off").
    assert_eq!(i64::from_value(field(&info, "redirect").unwrap()).unwrap(), 0);
    // CACHING is only valid in OPTIN/OPTOUT mode: a normal server error.
    let err = client.execute("CLIENT", ["CACHING", "YES"]).await.unwrap_err();
    assert!(err.server_error().is_some_and(|e| e.is_code("ERR")), "{err}");
    assert_eq!(client.execute("CLIENT", ["TRACKING", "OFF"]).await.unwrap(), Value::SimpleString("OK".into()));
    let info = client.execute("CLIENT", ["TRACKINGINFO"]).await.unwrap().into_pairs().unwrap();
    assert_eq!(Vec::<String>::from_value(field(&info, "flags").unwrap()).unwrap(), vec!["off"]);
    assert_eq!(i64::from_value(field(&info, "redirect").unwrap()).unwrap(), -1);
    assert_eq!(client.execute("CLIENT", ["TRACKING", "ON", "OPTIN"]).await.unwrap(), Value::SimpleString("OK".into()));
    assert_eq!(client.execute("CLIENT", ["CACHING", "YES"]).await.unwrap(), Value::SimpleString("OK".into()));
    client.execute("CLIENT", ["TRACKING", "OFF"]).await.unwrap();
}

#[tokio::test]
async fn cluster_only_commands_report_a_clean_server_error_on_a_standalone_server() {
    let Some(client) = single_connection().await else { return };
    for (name, args) in [("SFLUSH", vec!["0", "100"]), ("CLUSTERSCAN", vec!["0"]), ("CLUSTER", vec!["ADDSLOTS", "1"])] {
        match client.execute(name, args.clone()).await {
            Err(Error::Server(e)) => assert!(e.to_string().contains("cluster support disabled"), "{name}: {e}"),
            other => panic!("{name} {args:?}: expected a server error on a standalone server, got {other:?}"),
        }
    }
    // TRIMSLOTS needs arguments the server checks first.
    let err = client.command(Cmd::new("TRIMSLOTS")).await.unwrap_err();
    assert!(err.server_error().is_some(), "{err}");
}

#[tokio::test]
async fn an_enterprise_command_without_a_typed_method_reaches_the_server() {
    let Some(client) = single_connection().await else { return };
    // FEATURE.SETAT has no typed method. On an unlicensed server the reply is
    // NOLICENSE, which the SDK reports as Error::NotLicensed; on a licensed
    // one it would be an error about the missing group.
    match client.execute("FEATURE.SETAT", ["no-such-group", "e1", "1700000000000", "f", "v"]).await {
        Err(e) if e.is_not_licensed() => {}
        Err(e) => assert!(e.server_error().is_some(), "{e}"),
        Ok(v) => panic!("unexpected success: {v:?}"),
    }
}
