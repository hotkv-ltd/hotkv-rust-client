// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Timeouts, dropped connections, large payloads, big pipelines and the Unix
//! socket transport. Needs `HOTKV_TEST_URL`.

mod common;

use std::time::{Duration, Instant};

use hotkv::args::Cmd;
use hotkv::{Client, ClientOptions, Error};

async fn single(url: &str, tweak: impl FnOnce(&mut ClientOptions)) -> Client {
    let mut opts = ClientOptions::from_url(url).unwrap();
    opts.pool_size = 1;
    tweak(&mut opts);
    Client::with_options(opts).await.unwrap()
}

fn url() -> Option<String> {
    let u = std::env::var("HOTKV_TEST_URL").ok();
    if u.is_none() {
        println!("skipping: HOTKV_TEST_URL is not set");
    }
    u
}

#[tokio::test]
async fn a_command_timeout_fails_the_call_and_the_client_recovers() {
    let Some(url) = url() else { return };
    let client = single(&url, |o| o.command_timeout = Some(Duration::from_millis(300))).await;
    let key = common::unique("blocked");

    let started = Instant::now();
    let err = client.execute("BLPOP", [key.as_str(), "5"]).await.unwrap_err();
    assert!(matches!(err, Error::Timeout(_)), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2), "returned after {:?}", started.elapsed());

    // The timed-out connection is not reused (its reply would arrive late):
    // the next call gets a fresh connection and the right reply.
    assert_eq!(client.ping().await.unwrap(), "PONG");
    client.rpush(&key, ["late"]).await.unwrap();
    assert_eq!(client.lpop(&key).await.unwrap().as_deref(), Some("late"));
}

#[tokio::test]
async fn reads_retry_after_a_dropped_connection_but_writes_do_not() {
    let Some(url) = url() else { return };
    let client = single(&url, |_| {}).await;
    let admin = Client::connect(&url).await.unwrap();
    let key = common::unique("kill");
    client.set(&key, "v").await.unwrap();

    // Kill the client's only connection from outside.
    let id = client.client_id().await.unwrap();
    assert_eq!(admin.execute("CLIENT", ["KILL", "ID", id.to_string().as_str()]).await.unwrap(), hotkv::Value::Integer(1));
    // GET is retried on a fresh connection and succeeds.
    assert_eq!(client.get(&key).await.unwrap().as_deref(), Some("v"));

    // A write on a dead connection is not retried: the first call fails ...
    let id = client.client_id().await.unwrap();
    admin.execute("CLIENT", ["KILL", "ID", id.to_string().as_str()]).await.unwrap();
    let err = client.set(&key, "w").await.unwrap_err();
    assert!(err.is_retryable(), "{err}");
    // ... and the one after it works, on a new connection.
    client.set(&key, "w").await.unwrap();
    assert_eq!(client.get(&key).await.unwrap().as_deref(), Some("w"));
    client.del([key.as_str()]).await.unwrap();
}

#[tokio::test]
async fn an_idle_connection_that_died_is_replaced_before_use() {
    let Some(url) = url() else { return };
    let client = single(&url, |_| {}).await;
    let admin = Client::connect(&url).await.unwrap();
    let key = common::unique("idle");

    let id = client.client_id().await.unwrap();
    admin.execute("CLIENT", ["KILL", "ID", id.to_string().as_str()]).await.unwrap();
    // After 2 seconds idle a pooled connection is PING checked on borrow, so
    // even a write does not hit the dead connection.
    tokio::time::sleep(Duration::from_millis(2300)).await;
    client.set(&key, "ok").await.unwrap();
    assert_eq!(client.get(&key).await.unwrap().as_deref(), Some("ok"));
    client.del([key.as_str()]).await.unwrap();
}

#[tokio::test]
async fn large_values_and_large_pipelines() {
    let Some(url) = url() else { return };
    let client = Client::connect(&url).await.unwrap();
    let key = common::unique("big");
    let value: Vec<u8> = (0..8_000_000u32).map(|i| (i % 251) as u8).collect();
    client.set(&key, value.as_slice()).await.unwrap();
    assert_eq!(client.strlen(&key).await.unwrap(), 8_000_000);
    assert_eq!(client.get_bytes(&key).await.unwrap().as_deref(), Some(value.as_slice()));
    client.del([key.as_str()]).await.unwrap();

    // 20,000 commands in one pipeline come back in order.
    let prefix = common::unique("pipe");
    let mut pipe = client.pipeline();
    for i in 0..10_000 {
        pipe.queue(Cmd::new("SET").arg(format!("{prefix}:{i}")).arg(i));
    }
    for i in 0..10_000 {
        pipe.queue(Cmd::new("GET").arg(format!("{prefix}:{i}")));
    }
    let replies = pipe.execute().await.unwrap();
    assert_eq!(replies.len(), 20_000);
    for (i, r) in replies[10_000..].iter().enumerate() {
        let bytes = r.as_ref().unwrap().clone().into_bytes().unwrap();
        assert_eq!(bytes, i.to_string().into_bytes(), "reply {i}");
    }
    let keys: Vec<String> = (0..10_000).map(|i| format!("{prefix}:{i}")).collect();
    assert_eq!(client.del(keys.iter().map(String::as_str)).await.unwrap(), 10_000);
}

/// This test puts a small bridge between a Unix socket and the TCP server
/// and drives the SDK's Unix transport through it, so it does not depend on
/// the server listening on a Unix socket itself.
#[cfg(unix)]
#[tokio::test]
async fn unix_socket_transport() {
    let Some(url) = url() else { return };
    let server = ClientOptions::from_url(&url).unwrap();
    let path = std::env::temp_dir().join(format!("hotkv-sdk-test-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let target = format!("{}:{}", server.host, server.port);
    let bridge = tokio::spawn(async move {
        loop {
            let Ok((mut unix, _)) = listener.accept().await else { return };
            let target = target.clone();
            tokio::spawn(async move {
                if let Ok(mut tcp) = tokio::net::TcpStream::connect(&target).await {
                    let _ = tokio::io::copy_bidirectional(&mut unix, &mut tcp).await;
                }
            });
        }
    });

    let by_option = Client::with_options(ClientOptions { unix_socket: Some(path.clone()), ..Default::default() }).await.unwrap();
    assert_eq!(by_option.ping().await.unwrap(), "PONG");
    let key = common::unique("unix");
    by_option.set(&key, "over-a-socket").await.unwrap();
    assert_eq!(by_option.get(&key).await.unwrap().as_deref(), Some("over-a-socket"));
    by_option.del([key.as_str()]).await.unwrap();

    let by_url = Client::connect(&format!("unix://{}", path.display())).await.unwrap();
    assert_eq!(by_url.ping().await.unwrap(), "PONG");
    bridge.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn connecting_to_nothing_fails_within_the_connect_timeout() {
    let client = Client::with_options(ClientOptions {
        host: "127.0.0.1".into(),
        port: 1,
        connect_timeout: Duration::from_millis(500),
        max_retries: 0,
        ..Default::default()
    })
    .await
    .unwrap();
    let started = Instant::now();
    let err = client.ping().await.unwrap_err();
    assert!(matches!(err, Error::Connection(_) | Error::Io(_) | Error::Timeout(_)), "{err}");
    assert!(started.elapsed() < Duration::from_secs(2));
}
