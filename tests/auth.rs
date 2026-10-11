// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Authentication against a server started with `requirepass` and one ACL
//! user. Gated on environment variables so a plain `cargo test` skips it:
//!
//! - `HOTKV_TEST_AUTH_ADDR`      `host:port` of the server (plain TCP)
//! - `HOTKV_TEST_AUTH_PASSWORD`  its `requirepass` (the default user's password)
//! - `HOTKV_TEST_AUTH_USER`, `HOTKV_TEST_AUTH_USER_PASSWORD`  an ACL user that
//!   may use all commands except the `@dangerous` category on keys `sdk:*`
//!
//! All of these are throwaway test values for a server started just for this
//! run; they are never read from anywhere else.

use hotkv::{Client, ClientOptions};

struct Env {
    host: String,
    port: u16,
    password: String,
    user: Option<(String, String)>,
}

fn env() -> Option<Env> {
    let Ok(addr) = std::env::var("HOTKV_TEST_AUTH_ADDR") else {
        println!("skipping: HOTKV_TEST_AUTH_ADDR is not set");
        return None;
    };
    let (host, port) = addr.rsplit_once(':').expect("host:port");
    let user = match (std::env::var("HOTKV_TEST_AUTH_USER"), std::env::var("HOTKV_TEST_AUTH_USER_PASSWORD")) {
        (Ok(u), Ok(p)) => Some((u, p)),
        _ => None,
    };
    Some(Env {
        host: host.to_string(),
        port: port.parse().unwrap(),
        password: std::env::var("HOTKV_TEST_AUTH_PASSWORD").expect("HOTKV_TEST_AUTH_PASSWORD"),
        user,
    })
}

fn opts(e: &Env) -> ClientOptions {
    ClientOptions { host: e.host.clone(), port: e.port, ..Default::default() }
}

#[tokio::test]
async fn the_server_refuses_commands_without_a_password() {
    let Some(e) = env() else { return };
    let client = Client::with_options(opts(&e)).await.unwrap();
    let err = client.ping().await.unwrap_err();
    let se = err.server_error().unwrap_or_else(|| panic!("expected a server error, got {err}"));
    assert!(se.is_code("NOAUTH"), "{se}");
    let err = client.set("sdk:x", "v").await.unwrap_err();
    assert!(err.server_error().is_some_and(|s| s.is_code("NOAUTH")), "{err}");
}

#[tokio::test]
async fn password_authenticates_over_resp3_and_resp2() {
    let Some(e) = env() else { return };
    for resp3 in [true, false] {
        let mut o = opts(&e);
        o.password = Some(e.password.clone());
        o.resp3 = resp3;
        let client = Client::with_options(o).await.unwrap();
        assert_eq!(client.ping().await.unwrap(), "PONG");
        assert_eq!(client.server_protocol_version().await.unwrap(), if resp3 { 3 } else { 2 });
        let k = format!("sdk:auth:{resp3}");
        client.set(&k, "v").await.unwrap();
        assert_eq!(client.get(&k).await.unwrap().as_deref(), Some("v"));
        client.del([k.as_str()]).await.unwrap();
    }

    // The same through a URL, with a database and a client name.
    let url = format!("hotkv://:{}@{}:{}/3?client_name=sdk-auth-test", e.password, e.host, e.port);
    let client = Client::connect(&url).await.unwrap();
    assert_eq!(client.client_getname().await.unwrap().as_deref(), Some("sdk-auth-test"));
    client.set("sdk:in-db-3", "x").await.unwrap();
    let db0 = Client::with_options(ClientOptions { password: Some(e.password.clone()), ..opts(&e) }).await.unwrap();
    assert_eq!(db0.get("sdk:in-db-3").await.unwrap(), None, "the URL selected database 3");
    client.del(["sdk:in-db-3"]).await.unwrap();
}

#[tokio::test]
async fn a_wrong_password_is_rejected_while_connecting() {
    let Some(e) = env() else { return };
    for resp3 in [true, false] {
        let mut o = opts(&e);
        o.password = Some("definitely-not-the-password".to_string());
        o.resp3 = resp3;
        let client = Client::with_options(o).await.unwrap();
        let err = client.ping().await.unwrap_err();
        let se = err.server_error().unwrap_or_else(|| panic!("resp3={resp3}: expected a server error, got {err}"));
        assert!(se.is_code("WRONGPASS") || se.is_code("ERR"), "resp3={resp3}: {se}");
    }
}

#[tokio::test]
async fn acl_user_is_limited_to_its_keys_and_commands() {
    let Some(e) = env() else { return };
    let Some((user, pass)) = e.user.clone() else {
        println!("skipping: HOTKV_TEST_AUTH_USER is not set");
        return;
    };
    for resp3 in [true, false] {
        let mut o = opts(&e);
        o.username = Some(user.clone());
        o.password = Some(pass.clone());
        o.resp3 = resp3;
        let client = Client::with_options(o).await.unwrap();
        client.set("sdk:acl", "allowed").await.unwrap();
        assert_eq!(client.get("sdk:acl").await.unwrap().as_deref(), Some("allowed"));

        let err = client.set("other:key", "nope").await.unwrap_err();
        assert!(err.server_error().is_some_and(|s| s.is_code("NOPERM")), "resp3={resp3}: {err}");
        let err = client.keys("*").await.unwrap_err();
        assert!(err.server_error().is_some_and(|s| s.is_code("NOPERM")), "resp3={resp3}: {err}");
        client.del(["sdk:acl"]).await.unwrap();
    }
}
