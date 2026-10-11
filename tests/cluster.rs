// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! `ClusterClient` against a real cluster: three masters, each with a replica.
//! Gated on `HOTKV_TEST_CLUSTER_SEEDS` (comma separated `host:port`, any
//! nodes of the cluster) and skipped when it is unset. Each test uses its own
//! keys and puts the cluster back as it found it.

mod common;

use hotkv::args::Cmd;
use hotkv::cluster::key_slot;
use hotkv::{Client, ClientOptions, ClusterClient, ClusterOptions, Error, FromValue, Value};

fn seeds() -> Option<Vec<String>> {
    match std::env::var("HOTKV_TEST_CLUSTER_SEEDS") {
        Ok(s) if !s.is_empty() => Some(s.split(',').map(|x| x.trim().to_string()).collect()),
        _ => {
            println!("skipping: HOTKV_TEST_CLUSTER_SEEDS is not set");
            None
        }
    }
}

async fn node(addr: &str) -> Client {
    let (host, port) = addr.rsplit_once(':').unwrap();
    Client::with_options(ClientOptions { host: host.to_string(), port: port.parse().unwrap(), pool_size: 2, ..Default::default() })
        .await
        .unwrap()
}

struct NodeInfo {
    id: String,
    addr: String,
    master: bool,
}

async fn topology(seed: &str) -> Vec<NodeInfo> {
    let text = String::from_utf8(node(seed).await.execute("CLUSTER", ["NODES"]).await.unwrap().into_bytes().unwrap()).unwrap();
    text.lines()
        .map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            NodeInfo {
                id: f[0].to_string(),
                addr: f[1].split('@').next().unwrap().to_string(),
                master: f[2].contains("master"),
            }
        })
        .collect()
}

async fn connect(seeds: &[String], opts: ClusterOptions) -> ClusterClient {
    let refs: Vec<&str> = seeds.iter().map(String::as_str).collect();
    ClusterClient::connect(&refs, ClientOptions::default(), opts).await.unwrap()
}

fn text(v: Value) -> Option<String> {
    Option::<String>::from_value(v).unwrap()
}

#[tokio::test]
async fn keys_are_routed_to_the_node_that_owns_their_slot() {
    let Some(seeds) = seeds() else { return };
    let cluster = connect(&seeds[..1], ClusterOptions::default()).await;
    let topo = topology(&seeds[0]).await;
    let masters: Vec<&NodeInfo> = topo.iter().filter(|n| n.master).collect();
    assert_eq!(masters.len(), 3);

    let prefix = common::unique("clu");
    let mut owners_seen = std::collections::HashSet::new();
    for i in 0..120 {
        let key = format!("{prefix}:{i}");
        let reply = cluster.execute(&key, Cmd::new("SET").arg(&key).arg(i)).await.unwrap();
        assert_eq!(reply, Value::SimpleString("OK".into()));
    }
    for i in 0..120 {
        let key = format!("{prefix}:{i}");
        let got = cluster.execute(&key, Cmd::new("GET").arg(&key)).await.unwrap();
        assert_eq!(text(got).as_deref(), Some(i.to_string().as_str()), "{key}");
    }
    // Each key sits on exactly one master, and that master owns the key's slot.
    for i in (0..120).step_by(7) {
        let key = format!("{prefix}:{i}");
        let slot = key_slot(key.as_bytes());
        let mut holders = Vec::new();
        for m in &masters {
            let exists = node(&m.addr).await.exists([key.as_str()]).await;
            match exists {
                Ok(1) => holders.push(m.addr.clone()),
                Ok(_) => {}
                Err(e) => assert!(e.is_redirect(), "{e}"), // not the owner: MOVED
            }
        }
        assert_eq!(holders.len(), 1, "{key} (slot {slot}) held by {holders:?}");
        owners_seen.insert(holders.remove(0));
    }
    assert_eq!(owners_seen.len(), 3, "120 keys spread over all three masters");
    assert!(cluster.node_count() >= 3, "the client connected to each master it routed to, got {}", cluster.node_count());
    for i in 0..120 {
        let key = format!("{prefix}:{i}");
        cluster.execute(&key, Cmd::new("DEL").arg(&key)).await.unwrap();
    }
}

#[tokio::test]
async fn hash_tags_keep_keys_together_and_crossslot_is_a_server_error() {
    let Some(seeds) = seeds() else { return };
    let cluster = connect(&seeds[..1], ClusterOptions::default()).await;
    let tag = common::unique("tag");
    let (a, b) = (format!("{{{tag}}}:a"), format!("{{{tag}}}:b"));
    assert_eq!(key_slot(a.as_bytes()), key_slot(b.as_bytes()));

    let r = cluster.execute(&a, Cmd::new("MSET").arg(&a).arg("1").arg(&b).arg("2")).await.unwrap();
    assert_eq!(r, Value::SimpleString("OK".into()));
    let got = cluster.execute(&a, Cmd::new("MGET").arg(&a).arg(&b)).await.unwrap();
    assert_eq!(Vec::<String>::from_value(got).unwrap(), vec!["1", "2"]);

    // Two keys in different slots in one command.
    let (x, y) = (common::unique("x"), common::unique("y"));
    let mut y = y;
    while key_slot(x.as_bytes()) == key_slot(y.as_bytes()) {
        y.push('!');
    }
    let err = cluster.execute(&x, Cmd::new("MGET").arg(&x).arg(&y)).await.unwrap_err();
    assert!(err.server_error().is_some_and(|e| e.is_code("CROSSSLOT")), "{err}");
    cluster.execute(&a, Cmd::new("DEL").arg(&a).arg(&b)).await.unwrap();
}

/// Finds a slot owned by `from` and a key in it that does not exist.
fn key_in_slot_range(prefix: &str, from_slots: (u16, u16)) -> (String, u16) {
    for i in 0..100_000 {
        let k = format!("{prefix}:{i}");
        let s = key_slot(k.as_bytes());
        if s >= from_slots.0 && s <= from_slots.1 {
            return (k, s);
        }
    }
    panic!("no key found");
}

#[tokio::test]
async fn moved_and_ask_redirects_are_followed() {
    let Some(seeds) = seeds() else { return };
    let topo = topology(&seeds[0]).await;
    let masters: Vec<&NodeInfo> = topo.iter().filter(|n| n.master).collect();
    // The slot ranges of the masters, as a standard cluster tool splits them.
    let slots = node(&seeds[0]).await.execute("CLUSTER", ["SLOTS"]).await.unwrap().into_items().unwrap();
    let mut owner_of: Vec<(u16, u16, String)> = Vec::new();
    for s in slots {
        let f = s.into_items().unwrap();
        let start = i64::from_value(f[0].clone()).unwrap() as u16;
        let end = i64::from_value(f[1].clone()).unwrap() as u16;
        let m = f[2].clone().into_items().unwrap();
        let port = i64::from_value(m[1].clone()).unwrap();
        owner_of.push((start, end, format!("127.0.0.1:{port}")));
    }
    let (from_addr, to_addr) = (&masters[0].addr, &masters[1].addr);
    let from_range = owner_of.iter().find(|(_, _, a)| a == from_addr).map(|(s, e, _)| (*s, *e)).unwrap();
    let (key, slot) = key_in_slot_range(&common::unique("mv"), from_range);

    let from = node(from_addr).await;
    let to = node(to_addr).await;
    let from_id = &masters[0].id;
    let to_id = &masters[1].id;
    let slot_s = slot.to_string();

    // The slot moves from `from` to `to` the way a reshard tool does: IMPORTING on
    // the target, MIGRATING on the source, then NODE on the target, the source
    // and the other masters.
    let others: Vec<&NodeInfo> = masters.iter().skip(2).copied().collect();
    let cluster = connect(&seeds[..1], ClusterOptions::default()).await;

    // ASK: while the slot is migrating the key (not on `from`) is answered ASK.
    to.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "IMPORTING", from_id.as_str()]).await.unwrap();
    from.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "MIGRATING", to_id.as_str()]).await.unwrap();
    let raw = from.set(&key, "x").await.unwrap_err();
    assert!(raw.server_error().is_some_and(|e| e.is_ask()), "a plain client sees ASK: {raw}");
    let asked = cluster.execute(&key, Cmd::new("SET").arg(&key).arg("asked")).await;
    // Finish the move (also the cleanup when an assertion below fails).
    to.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "NODE", to_id.as_str()]).await.unwrap();
    from.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "NODE", to_id.as_str()]).await.unwrap();
    for n in &others {
        node(&n.addr).await.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "NODE", to_id.as_str()]).await.unwrap();
    }
    assert_eq!(asked.expect("ASK followed with ASKING"), Value::SimpleString("OK".into()));
    assert_eq!(to.get(&key).await.unwrap().as_deref(), Some("asked"), "the ASK-redirected write landed on the new owner");

    // MOVED: the client still maps the slot to `from`. The first attempt gets MOVED,
    // the map is refreshed and the command lands on the new owner.
    let moved = cluster.execute(&key, Cmd::new("SET").arg(&key).arg("moved")).await;
    let stored = to.get(&key).await;
    let r = moved.expect("MOVED followed after the slot changed owner");
    assert_eq!(r, Value::SimpleString("OK".into()));
    assert_eq!(stored.unwrap().as_deref(), Some("moved"), "the key is on the new owner");
    let got = cluster.execute(&key, Cmd::new("GET").arg(&key)).await.unwrap();
    assert_eq!(text(got).as_deref(), Some("moved"));

    // Move it back so the cluster is as it was.
    cluster.execute(&key, Cmd::new("DEL").arg(&key)).await.unwrap();
    from.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "IMPORTING", to_id.as_str()]).await.unwrap();
    to.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "MIGRATING", from_id.as_str()]).await.unwrap();
    from.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "NODE", from_id.as_str()]).await.unwrap();
    to.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "NODE", from_id.as_str()]).await.unwrap();
    for n in &others {
        node(&n.addr).await.execute("CLUSTER", ["SETSLOT", slot_s.as_str(), "NODE", from_id.as_str()]).await.unwrap();
    }
}

#[tokio::test]
async fn reads_can_be_served_by_replicas() {
    let Some(seeds) = seeds() else { return };
    let topo = topology(&seeds[0]).await;
    let cluster = connect(&seeds[..1], ClusterOptions { read_from_replicas: true, ..Default::default() }).await;

    let prefix = common::unique("ro");
    let keys: Vec<String> = (0..30).map(|i| format!("{prefix}:{i}")).collect();
    for k in &keys {
        cluster.execute(k, Cmd::new("SET").arg(k).arg("v")).await.unwrap();
    }
    // GET calls served so far by each replica.
    let replicas: Vec<&NodeInfo> = topo.iter().filter(|n| !n.master).collect();
    assert_eq!(replicas.len(), 3);
    let gets = |addr: String| async move {
        let info = node(&addr).await.info(Some("commandstats")).await.unwrap();
        info.lines()
            .find_map(|l| l.strip_prefix("cmdstat_get:calls=").and_then(|r| r.split(',').next()).and_then(|n| n.parse::<u64>().ok()))
            .unwrap_or(0)
    };
    let mut before = Vec::new();
    for r in &replicas {
        before.push(gets(r.addr.clone()).await);
    }
    // Replication is asynchronous: retry a read until the replica has the key.
    for k in &keys {
        let mut value = None;
        for _ in 0..50 {
            let v = text(cluster.execute_readonly(k, Cmd::new("GET").arg(k)).await.unwrap());
            if v.is_some() {
                value = v;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(value.as_deref(), Some("v"), "{k}");
    }
    let mut served = 0;
    for (r, b) in replicas.iter().zip(before) {
        served += gets(r.addr.clone()).await - b;
    }
    assert!(served >= keys.len() as u64, "replicas served {served} of {} reads", keys.len());
    for k in &keys {
        cluster.execute(k, Cmd::new("DEL").arg(k)).await.unwrap();
    }
}

#[tokio::test]
async fn a_cluster_client_needs_a_reachable_seed_and_reports_unknown_errors() {
    let Some(seeds) = seeds() else { return };
    // The first seed is dead, the second works.
    let dead = "127.0.0.1:1".to_string();
    let cluster = connect(&[dead.clone(), seeds[0].clone()], ClusterOptions::default()).await;
    let k = common::unique("seed");
    cluster.execute(&k, Cmd::new("SET").arg(&k).arg("1")).await.unwrap();
    cluster.execute(&k, Cmd::new("DEL").arg(&k)).await.unwrap();

    let refs = [dead.as_str()];
    let err = ClusterClient::connect(&refs, ClientOptions::default(), ClusterOptions::default()).await.err().expect("no reachable seed");
    assert!(matches!(err, Error::Connection(_) | Error::Io(_) | Error::Timeout(_) | Error::Protocol(_)), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_replica_reads_never_fall_back_to_a_redirect_error() {
    let Some(seeds) = seeds() else { return };
    let cluster = std::sync::Arc::new(connect(&seeds[..1], ClusterOptions { read_from_replicas: true, max_redirects: 0 }).await);
    let prefix = common::unique("cro");
    let keys: Vec<String> = (0..24).map(|i| format!("{prefix}:{i}")).collect();
    for k in &keys {
        cluster.execute(k, Cmd::new("SET").arg(k).arg("v")).await.unwrap();
    }
    // Let the replicas catch up (a replica read of a missing key is nil, not an error).
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // With max_redirects 0 any MOVED the replica answers (because READONLY was
    // not in force on the connection the read used) surfaces as an error.
    let mut tasks = Vec::new();
    for t in 0..16 {
        let cluster = cluster.clone();
        let keys = keys.clone();
        tasks.push(tokio::spawn(async move {
            for round in 0..40 {
                let k = &keys[(t + round) % keys.len()];
                let r = cluster.execute_readonly(k, Cmd::new("GET").arg(k)).await;
                assert!(r.is_ok(), "task {t} round {round}: {:?}", r.err());
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    for k in &keys {
        cluster.execute(k, Cmd::new("DEL").arg(k)).await.unwrap();
    }
}
