// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

mod common;

use hotkv::args::Cmd;
use hotkv::cluster::key_slot;
use hotkv::{ClusterClient, ClusterOptions};

#[tokio::test]
async fn pipeline_runs_everything_in_one_round_trip() {
    let Some(client) = common::connect().await else { return };
    let k1 = common::unique("pipe1");
    let k2 = common::unique("pipe2");

    let mut pipe = client.pipeline();
    pipe.queue(Cmd::new("SET").arg(&k1).arg("a"));
    pipe.queue(Cmd::new("SET").arg(&k2).arg("b"));
    pipe.queue(Cmd::new("GET").arg(&k1));
    pipe.queue(Cmd::new("INCR").arg(&k1)); // deliberately fails: k1 is not an integer
    pipe.queue(Cmd::new("GET").arg(&k2));
    let results = pipe.execute().await.unwrap();

    assert_eq!(results.len(), 5);
    assert!(results[0].is_ok());
    assert!(results[1].is_ok());
    assert_eq!(results[2].as_ref().unwrap().clone().into_bytes().unwrap(), b"a".to_vec());
    assert!(results[3].is_err(), "INCR on a non-integer value must fail without aborting the batch");
    assert_eq!(results[4].as_ref().unwrap().clone().into_bytes().unwrap(), b"b".to_vec());

    client.del([k1.as_str(), k2.as_str()]).await.unwrap();
}

#[tokio::test]
async fn transaction_commits() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("tx");

    let mut tx = client.transaction().await.unwrap();
    tx.multi().await.unwrap();
    tx.queue(Cmd::new("SET").arg(&k).arg("1")).await.unwrap();
    tx.queue(Cmd::new("INCR").arg(&k)).await.unwrap();
    let results = tx.exec().await.unwrap().expect("not aborted");
    assert_eq!(results.len(), 2);
    assert_eq!(client.get(&k).await.unwrap(), Some("2".to_string()));

    client.del([k.as_str()]).await.unwrap();
}

#[tokio::test]
async fn transaction_aborts_when_watched_key_changes() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("watch");
    client.set(&k, "orig").await.unwrap();

    let mut tx = client.transaction().await.unwrap();
    tx.watch([k.as_str()]).await.unwrap();
    tx.multi().await.unwrap();
    tx.queue(Cmd::new("SET").arg(&k).arg("from-tx")).await.unwrap();

    // A different connection changes the watched key before EXEC.
    client.set(&k, "changed-elsewhere").await.unwrap();

    let result = tx.exec().await.unwrap();
    assert!(result.is_none(), "EXEC must abort (return null) when a watched key changed");
    assert_eq!(client.get(&k).await.unwrap(), Some("changed-elsewhere".to_string()));

    client.del([k.as_str()]).await.unwrap();
}

#[tokio::test]
async fn transaction_discard() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("discard");

    let mut tx = client.transaction().await.unwrap();
    tx.multi().await.unwrap();
    tx.queue(Cmd::new("SET").arg(&k).arg("nope")).await.unwrap();
    tx.discard().await.unwrap();

    assert_eq!(client.get(&k).await.unwrap(), None);
}

#[tokio::test]
async fn pubsub_delivers_published_messages() {
    let Some(client) = common::connect().await else { return };
    let channel = common::unique("channel");

    let mut sub = client.subscribe([channel.as_str()]).await.unwrap();
    let confirm = sub.next().await.unwrap().unwrap();
    match confirm {
        hotkv::PubSubMessage::Subscribed { channel: c, count } => {
            assert_eq!(c, channel);
            assert_eq!(count, 1);
        }
        other => panic!("expected a Subscribed confirmation, got {other:?}"),
    }

    let publisher = client.clone();
    let ch = channel.clone();
    tokio::spawn(async move {
        // Give the subscriber a moment to be listening.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        publisher.execute("PUBLISH", [ch.as_str(), "payload"]).await.unwrap();
    });

    let msg = sub.next().await.unwrap().unwrap();
    match msg {
        hotkv::PubSubMessage::Message { channel: c, payload } => {
            assert_eq!(c, channel);
            assert_eq!(payload, b"payload".to_vec());
        }
        other => panic!("expected a Message, got {other:?}"),
    }
}

#[tokio::test]
async fn ssubscribe_delivers_shard_messages() {
    let Some(client) = common::connect().await else { return };
    let channel = common::unique("schannel");

    let mut sub = client.ssubscribe([channel.as_str()]).await.unwrap();
    let confirm = sub.next().await.unwrap().unwrap();
    match confirm {
        hotkv::PubSubMessage::SSubscribed { channel: c, count } => {
            assert_eq!(c, channel);
            assert_eq!(count, 1);
        }
        other => panic!("expected an SSubscribed confirmation, got {other:?}"),
    }

    let publisher = client.clone();
    let ch = channel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        publisher.execute("SPUBLISH", [ch.as_str(), "shard-payload"]).await.unwrap();
    });

    let msg = sub.next().await.unwrap().unwrap();
    match msg {
        hotkv::PubSubMessage::SMessage { channel: c, payload } => {
            assert_eq!(c, channel);
            assert_eq!(payload, b"shard-payload".to_vec());
        }
        other => panic!("expected an SMessage, got {other:?}"),
    }
}

#[tokio::test]
async fn pubsub_as_a_stream() {
    use futures_core::Stream;
    use std::pin::Pin;

    let Some(client) = common::connect().await else { return };
    let channel = common::unique("stream-channel");
    let mut sub = client.subscribe([channel.as_str()]).await.unwrap();

    // Drive it purely through the `Stream` trait via `StreamExt`-style
    // manual polling (no extra crate dependency needed for this check).
    let first = std::future::poll_fn(|cx| Pin::new(&mut sub).poll_next(cx)).await;
    assert!(matches!(first, Some(Ok(hotkv::PubSubMessage::Subscribed { .. }))));
}

#[tokio::test]
async fn cluster_client_handles_a_standalone_server_gracefully() {
    let Some(url) = std::env::var("HOTKV_TEST_URL").ok() else {
        println!("skipping: HOTKV_TEST_URL is not set");
        return;
    };
    let opts = hotkv::ClientOptions::from_url(&url).unwrap();
    let addr = format!("{}:{}", opts.host, opts.port);

    // The test server runs standalone (no CLUSTER SLOTS data), so this is
    // a smoke test that ClusterClient::connect fails cleanly (not a hang
    // or panic) rather than a full multi-node exercise.
    match ClusterClient::connect(&[addr.as_str()], opts, ClusterOptions::default()).await {
        Ok(_) => println!("server unexpectedly reported cluster slots; nothing further to assert"),
        Err(e) => println!("standalone server correctly rejected as a cluster (as expected): {e}"),
    }
}

#[test]
fn key_slot_matches_cluster_keyslot_vectors() {
    // Re-checked here (in addition to the crate's own unit tests) as a
    // black box use of the public `hotkv::cluster` API.
    assert_eq!(key_slot(b"foo"), 12182);
    assert_eq!(key_slot(b"{user1000}.following"), 3443);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn more_tasks_than_pooled_connections_all_get_served() {
    let Ok(url) = std::env::var("HOTKV_TEST_URL") else {
        println!("skipping: HOTKV_TEST_URL is not set");
        return;
    };
    let mut opts = hotkv::ClientOptions::from_url(&url).unwrap();
    opts.pool_size = 2;
    opts.connect_timeout = std::time::Duration::from_secs(2);
    let client = hotkv::Client::with_options(opts).await.unwrap();
    let key = common::unique("pool-wait");
    client.set(&key, "v").await.unwrap();

    let started = std::time::Instant::now();
    let mut tasks = Vec::new();
    for t in 0..12 {
        let c = client.clone();
        let k = key.clone();
        tasks.push(tokio::spawn(async move {
            for round in 0..50 {
                let got = c.get(&k).await.unwrap_or_else(|e| panic!("task {t} round {round}: {e}"));
                assert_eq!(got.as_deref(), Some("v"));
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(2), "waiting tasks must be woken when a connection is returned, took {:?}", started.elapsed());
    assert!(client.pool_stats().max_size == 2);
    client.del([key.as_str()]).await.unwrap();
}
