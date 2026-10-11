// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Shared helpers for integration tests. Not itself a test binary (Cargo
//! only auto-discovers `tests/*.rs`, not `tests/common/*`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hotkv::Client;

/// Connects to the server named by `HOTKV_TEST_URL`, or prints a skip
/// reason and returns `None` if that variable is unset.
#[allow(dead_code)] // not every test binary that includes this module calls it directly
pub async fn connect() -> Option<Client> {
    let Ok(url) = std::env::var("HOTKV_TEST_URL") else {
        println!("skipping: HOTKV_TEST_URL is not set");
        return None;
    };
    match Client::connect(&url).await {
        Ok(c) => Some(c),
        Err(e) => panic!("failed to connect to HOTKV_TEST_URL: {e}"),
    }
}

/// A key/namespace/name unlikely to collide with anything else, prefixed
/// so it is easy to recognize (and bulk delete) if a test run leaves
/// something behind.
pub fn unique(prefix: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("hotkv-rust-test:{prefix}:{nanos}:{n}")
}

/// True when the environment says the test server carries an enterprise
/// license (`HOTKV_TEST_LICENSED` set to anything but empty or `0`). Then a
/// `NOLICENSE` reply is a failure, not a reason to skip.
#[allow(dead_code)]
pub fn licensed() -> bool {
    std::env::var("HOTKV_TEST_LICENSED").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

/// Called when a command answered `NOLICENSE`: panics if the server is
/// declared licensed, otherwise prints why the test is skipping.
#[allow(dead_code)]
pub fn unlicensed(what: &str, err: &dyn std::fmt::Display) {
    if licensed() {
        panic!("{what}: NOLICENSE ({err}) but HOTKV_TEST_LICENSED says the server is licensed");
    }
    println!("skipping {what}: the test server has no license for it ({err})");
}

/// Skips the calling test (with a printed reason) if `result` failed with
/// a `NOLICENSE` error and the server is not declared licensed; fails the
/// test on `NOLICENSE` when `HOTKV_TEST_LICENSED` is set. Otherwise unwraps.
#[macro_export]
macro_rules! skip_if_not_licensed {
    ($result:expr, $what:expr) => {
        match $result {
            Ok(v) => v,
            Err(e) if e.is_not_licensed() => {
                common::unlicensed($what, &e);
                return;
            }
            Err(e) => panic!("{}: unexpected error: {}", $what, e),
        }
    };
}
