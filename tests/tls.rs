// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! TLS connections, built only with the `tls` feature and gated on these
//! environment variables:
//!
//! - `HOTKV_TEST_TLS_DIR`: directory holding `ca.crt`, `client.crt`,
//!   `client.key` and `other-ca.crt` (a CA that did not sign the server
//!   certificate). The client certificate must be an X.509 v3 certificate.
//! - `HOTKV_TEST_TLS_MTLS_PORT`: port of a TLS-only server that requires
//!   client certificates (`tls-auth-clients yes`).
//! - `HOTKV_TEST_TLS_PW_PORT`: port of a TLS-only server with
//!   `tls-auth-clients no` and a `requirepass`.
//! - `HOTKV_TEST_TLS_PASSWORD`: that `requirepass`.
//!
//! The server certificate names `localhost` and `127.0.0.1`.
#![cfg(feature = "tls")]

use std::path::PathBuf;

use hotkv::{Client, ClientOptions, Error, TlsOptions};

struct Env {
    dir: PathBuf,
    mtls_port: u16,
    pw_port: u16,
    password: String,
}

fn env() -> Option<Env> {
    let Ok(dir) = std::env::var("HOTKV_TEST_TLS_DIR") else {
        println!("skipping: HOTKV_TEST_TLS_DIR is not set");
        return None;
    };
    Some(Env {
        dir: PathBuf::from(dir),
        mtls_port: std::env::var("HOTKV_TEST_TLS_MTLS_PORT").expect("HOTKV_TEST_TLS_MTLS_PORT").parse().unwrap(),
        pw_port: std::env::var("HOTKV_TEST_TLS_PW_PORT").expect("HOTKV_TEST_TLS_PW_PORT").parse().unwrap(),
        password: std::env::var("HOTKV_TEST_TLS_PASSWORD").expect("HOTKV_TEST_TLS_PASSWORD"),
    })
}

fn tls(e: &Env, ca: &str, client_cert: bool) -> TlsOptions {
    TlsOptions {
        enabled: true,
        ca_file: Some(e.dir.join(ca)),
        cert_file: client_cert.then(|| e.dir.join("client.crt")),
        key_file: client_cert.then(|| e.dir.join("client.key")),
        ..Default::default()
    }
}

fn mtls_opts(e: &Env) -> ClientOptions {
    ClientOptions { host: "127.0.0.1".into(), port: e.mtls_port, tls: tls(e, "ca.crt", true), ..Default::default() }
}

fn pw_opts(e: &Env) -> ClientOptions {
    ClientOptions { host: "127.0.0.1".into(), port: e.pw_port, password: Some(e.password.clone()), tls: tls(e, "ca.crt", false), ..Default::default() }
}

#[tokio::test]
async fn mutual_tls_with_a_private_ca() {
    let Some(e) = env() else { return };
    let client = Client::with_options(mtls_opts(&e)).await.unwrap();
    assert_eq!(client.ping().await.unwrap(), "PONG");
    let key = [&b"tls:bin:"[..], &[0u8, 1, 2, 255][..]].concat();
    let value: Vec<u8> = (0u8..=255).cycle().take(100_000).collect();
    client.set(key.as_slice(), value.as_slice()).await.unwrap();
    assert_eq!(client.get_bytes(key.as_slice()).await.unwrap(), Some(value));
    client.del([key.as_slice()]).await.unwrap();

    // Several tasks share the pool, each with its own TLS session.
    let mut tasks = Vec::new();
    for i in 0..8 {
        let c = client.clone();
        tasks.push(tokio::spawn(async move {
            let k = format!("tls:par:{i}");
            c.set(&k, i.to_string()).await.unwrap();
            assert_eq!(c.get(&k).await.unwrap().as_deref(), Some(i.to_string().as_str()));
            c.del([k.as_str()]).await.unwrap();
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
}

#[tokio::test]
async fn tls_pubsub_and_transactions() {
    let Some(e) = env() else { return };
    let client = Client::with_options(mtls_opts(&e)).await.unwrap();
    let ch = format!("tls:chan:{}", std::process::id());
    let mut sub = client.subscribe([ch.as_str()]).await.unwrap();
    assert!(matches!(sub.next().await.unwrap().unwrap(), hotkv::PubSubMessage::Subscribed { .. }));
    client.execute("PUBLISH", [ch.as_str(), "over-tls"]).await.unwrap();
    match sub.next().await.unwrap().unwrap() {
        hotkv::PubSubMessage::Message { payload, .. } => assert_eq!(payload, b"over-tls".to_vec()),
        other => panic!("expected a message, got {other:?}"),
    }

    let k = "tls:tx";
    let mut tx = client.transaction().await.unwrap();
    tx.multi().await.unwrap();
    tx.queue(hotkv::args::Cmd::new("SET").arg(k).arg("1")).await.unwrap();
    tx.queue(hotkv::args::Cmd::new("INCR").arg(k)).await.unwrap();
    assert_eq!(tx.exec().await.unwrap().expect("committed").len(), 2);
    assert_eq!(client.get(k).await.unwrap().as_deref(), Some("2"));
    client.del([k]).await.unwrap();
}

#[tokio::test]
async fn a_server_that_requires_a_client_certificate_rejects_a_client_without_one() {
    let Some(e) = env() else { return };
    let mut o = mtls_opts(&e);
    o.tls = tls(&e, "ca.crt", false);
    let client = Client::with_options(o).await.unwrap();
    let err = client.ping().await.unwrap_err();
    assert!(!matches!(err, Error::Server(_)), "the handshake must fail, not a command: {err}");
}

#[tokio::test]
async fn a_certificate_from_an_unknown_ca_is_rejected() {
    let Some(e) = env() else { return };
    let mut o = pw_opts(&e);
    o.tls = tls(&e, "other-ca.crt", false);
    let client = Client::with_options(o).await.unwrap();
    let err = client.ping().await.unwrap_err();
    assert!(matches!(err, Error::Tls(_) | Error::Connection(_) | Error::Io(_)), "{err}");
}

#[tokio::test]
async fn server_name_is_verified_and_can_be_overridden() {
    let Some(e) = env() else { return };
    // The certificate names localhost, so an explicit matching name works for an IP host...
    let mut o = pw_opts(&e);
    o.tls.server_name = Some("localhost".into());
    let client = Client::with_options(o).await.unwrap();
    assert_eq!(client.ping().await.unwrap(), "PONG");
    // ...and a name it does not carry is refused.
    let mut o = pw_opts(&e);
    o.tls.server_name = Some("not-the-server.example".into());
    let client = Client::with_options(o).await.unwrap();
    assert!(client.ping().await.is_err());
}

#[tokio::test]
async fn tls_with_a_password_and_the_url_forms() {
    let Some(e) = env() else { return };
    let client = Client::with_options(pw_opts(&e)).await.unwrap();
    assert_eq!(client.ping().await.unwrap(), "PONG");

    // hotkvs:// with insecure_skip_verify needs no CA file.
    let url = format!("hotkvs://:{}@127.0.0.1:{}?insecure_skip_verify=true", e.password, e.pw_port);
    let client = Client::connect(&url).await.unwrap();
    assert_eq!(client.ping().await.unwrap(), "PONG");
    client.set("tls:url", "ok").await.unwrap();
    assert_eq!(client.get("tls:url").await.unwrap().as_deref(), Some("ok"));
    client.del(["tls:url"]).await.unwrap();

    // Without verification disabled, the system roots do not know this CA.
    let strict = format!("hotkvs://:{}@127.0.0.1:{}", e.password, e.pw_port);
    let client = Client::connect(&strict).await.unwrap();
    assert!(client.ping().await.is_err());

    // A wrong password over TLS is still a server error.
    let mut o = pw_opts(&e);
    o.password = Some("wrong".into());
    let err = Client::with_options(o).await.unwrap().ping().await.unwrap_err();
    assert!(err.server_error().is_some(), "{err}");
}

#[tokio::test]
async fn plain_tcp_to_a_tls_port_fails_cleanly() {
    let Some(e) = env() else { return };
    let o = ClientOptions {
        host: "127.0.0.1".into(),
        port: e.pw_port,
        connect_timeout: std::time::Duration::from_secs(3),
        command_timeout: Some(std::time::Duration::from_secs(3)),
        ..Default::default()
    };
    let client = Client::with_options(o).await.unwrap();
    assert!(client.ping().await.is_err());
}
