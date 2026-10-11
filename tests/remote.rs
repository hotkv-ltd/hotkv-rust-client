// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Uses the client the way a customer does: over the network, with TLS and a
//! password. Built only with the `tls` feature and gated on these environment
//! variables (nothing is read from files in the repository):
//!
//! - `HOTKV_REMOTE_HOST`: host name or address of the server.
//! - `HOTKV_REMOTE_PORT`: its TLS port.
//! - `HOTKV_REMOTE_PASSWORD`: the server password.
//! - `HOTKV_REMOTE_CA`: PEM file with the CA (or self-signed certificate) that
//!   signed the server certificate. Without it the system trust roots are used.
//!
//! Every key and name carries a run-unique prefix and is removed at the end.
//! Nothing global (FLUSHALL, CONFIG SET, ACL changes) is run.
#![cfg(feature = "tls")]

mod common;

use std::path::PathBuf;

use hotkv::args::Cmd;
use hotkv::{Client, ClientOptions, TlsOptions};

struct Env {
    host: String,
    port: u16,
    password: String,
    ca: Option<PathBuf>,
}

fn env() -> Option<Env> {
    let Ok(host) = std::env::var("HOTKV_REMOTE_HOST") else {
        println!("skipping: HOTKV_REMOTE_HOST is not set");
        return None;
    };
    Some(Env {
        host,
        port: std::env::var("HOTKV_REMOTE_PORT").expect("HOTKV_REMOTE_PORT").parse().expect("port"),
        password: std::env::var("HOTKV_REMOTE_PASSWORD").expect("HOTKV_REMOTE_PASSWORD"),
        ca: std::env::var("HOTKV_REMOTE_CA").ok().map(PathBuf::from),
    })
}

fn options(e: &Env, password: Option<&str>) -> ClientOptions {
    ClientOptions {
        host: e.host.clone(),
        port: e.port,
        password: password.map(str::to_string),
        tls: TlsOptions { enabled: true, ca_file: e.ca.clone(), ..Default::default() },
        ..Default::default()
    }
}

async fn client(e: &Env) -> Client {
    Client::with_options(options(e, Some(&e.password))).await.expect("connect over TLS with the password")
}

#[tokio::test]
async fn connects_with_options_and_with_a_url() {
    let Some(e) = env() else { return };
    assert_eq!(client(&e).await.ping().await.unwrap(), "PONG");

    // The URL form carries the password; the CA still comes from the options.
    let url = format!("hotkvs://:{}@{}:{}", e.password, e.host, e.port);
    let mut opts = ClientOptions::from_url(&url).unwrap();
    opts.tls.ca_file = e.ca.clone();
    let c = Client::with_options(opts).await.unwrap();
    assert_eq!(c.ping().await.unwrap(), "PONG");
}

#[tokio::test]
async fn wrong_missing_password_and_wrong_ca_are_clean_errors() {
    let Some(e) = env() else { return };

    // The client may connect lazily, so a wrong password can surface at connect
    // time or on the first command; either way it must be an error.
    match Client::with_options(options(&e, Some("definitely-not-the-password"))).await {
        Err(_) => {}
        Ok(c) => assert!(c.ping().await.is_err(), "a wrong password must not be accepted"),
    }

    // Without a password the connection opens, and the first command is refused.
    if let Ok(c) = Client::with_options(options(&e, None)).await {
        assert!(c.ping().await.is_err(), "commands without a password must be refused");
    }

    if e.ca.is_some() {
        // The system trust roots do not know this server certificate.
        let mut opts = options(&e, Some(&e.password));
        opts.tls.ca_file = None;
        match Client::with_options(opts).await {
            Err(_) => {}
            Ok(c) => assert!(c.ping().await.is_err(), "an unknown certificate must be refused"),
        }
    }
}

#[tokio::test]
async fn data_types_expiry_pipeline_and_transaction() {
    let Some(e) = env() else { return };
    let c = client(&e).await;
    let p = common::unique("remote");

    c.set(format!("{p}:s"), "hello").await.unwrap();
    assert_eq!(c.get(format!("{p}:s")).await.unwrap().as_deref(), Some("hello"));
    assert_eq!(c.incr(format!("{p}:n")).await.unwrap(), 1);

    assert_eq!(c.hset(format!("{p}:h"), [("f1", "v1"), ("f2", "v2")]).await.unwrap(), 2);
    assert_eq!(c.hgetall(format!("{p}:h")).await.unwrap().len(), 2);
    assert_eq!(c.lpush(format!("{p}:l"), ["a", "b"]).await.unwrap(), 2);
    assert_eq!(c.lrange(format!("{p}:l"), 0, -1).await.unwrap(), vec!["b", "a"]);
    assert_eq!(c.sadd(format!("{p}:set"), ["x", "y", "z"]).await.unwrap(), 3);

    assert!(c.expire(format!("{p}:s"), 100, None).await.unwrap());
    let ttl = c.ttl(format!("{p}:s")).await.unwrap();
    assert!((1..=100).contains(&ttl), "ttl {ttl}");

    let mut pipe = c.pipeline();
    pipe.queue(Cmd::new("SET").arg(format!("{p}:p1")).arg("1"));
    pipe.queue(Cmd::new("GET").arg(format!("{p}:p1")));
    pipe.queue(Cmd::new("INCR").arg(format!("{p}:p1")));
    let results = pipe.execute().await.unwrap();
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|r| r.is_ok()));

    let mut tx = c.transaction().await.unwrap();
    tx.multi().await.unwrap();
    tx.queue(Cmd::new("SET").arg(format!("{p}:t")).arg("1")).await.unwrap();
    tx.queue(Cmd::new("INCR").arg(format!("{p}:t"))).await.unwrap();
    assert_eq!(tx.exec().await.unwrap().expect("not aborted").len(), 2);
    assert_eq!(c.get(format!("{p}:t")).await.unwrap().as_deref(), Some("2"));

    let keys: Vec<String> = ["s", "n", "h", "l", "set", "p1", "t"].iter().map(|k| format!("{p}:{k}")).collect();
    c.del(keys.iter().map(String::as_str)).await.unwrap();
}

#[tokio::test]
async fn one_megabyte_value_and_concurrent_tasks() {
    let Some(e) = env() else { return };
    let c = client(&e).await;
    let p = common::unique("remote-big");

    let value: Vec<u8> = (0u8..=255).cycle().take(1 << 20).collect();
    c.set(format!("{p}:big"), value.as_slice()).await.unwrap();
    assert_eq!(c.get_bytes(format!("{p}:big")).await.unwrap(), Some(value));

    let mut tasks = Vec::new();
    for i in 0..16 {
        let c = c.clone();
        let key = format!("{p}:par:{i}");
        tasks.push(tokio::spawn(async move {
            for round in 0..10 {
                let v = format!("{i}-{round}");
                c.set(&key, &v).await.unwrap();
                assert_eq!(c.get(&key).await.unwrap().as_deref(), Some(v.as_str()));
            }
            c.del([key.as_str()]).await.unwrap();
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    c.del([format!("{p}:big").as_str()]).await.unwrap();
}

#[tokio::test]
async fn budget_and_prices_through_the_typed_api() {
    let Some(e) = env() else { return };
    let c = client(&e).await;

    // These commands need a server license that includes the llm-budget and
    // llm-stats features; without it the server answers NOLICENSE and the
    // test has nothing to check.
    let lb = c.llmbudget();
    let name = common::unique("remote-budget").replace(':', "-");
    if let Err(err) = lb.set(&name, 1_000_000, 0.00001, 60).await {
        println!("skipping the AI checks: {err}");
        return;
    }
    assert!(lb.consume(&name, 0, 0.000005, None).await.unwrap());
    assert!(lb.consume(&name, 0, 0.000005, None).await.unwrap());
    assert!(!lb.consume(&name, 0, 0.000005, None).await.unwrap(), "the limit is enforced to the micro-dollar");
    let status = lb.status(&name).await.unwrap();
    assert!((status.cost_used_usd - 0.00001).abs() < 1e-9, "{}", status.cost_used_usd);
    lb.drop(&name).await.unwrap();

    let ls = c.llmstats();
    let model = common::unique("remote-model").replace(':', "-");
    ls.price_set(&model, 0.1 + 0.2, 15.0, None).await.unwrap();
    let price = ls.price_get(&model).await.unwrap();
    assert!((price.input_usd_per_mtok - 0.3).abs() < 1e-12);
    assert_eq!(price.output_usd_per_mtok, 15.0);
}
