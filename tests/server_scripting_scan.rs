// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

mod common;

use std::collections::HashSet;

use hotkv::Value;

#[tokio::test]
async fn server_commands() {
    let Some(client) = common::connect().await else { return };

    assert_eq!(client.ping().await.unwrap(), "PONG");
    assert_eq!(client.ping_message("hi").await.unwrap(), "hi");
    assert_eq!(client.echo("hello").await.unwrap(), "hello");

    let info = client.info(None).await.unwrap();
    assert!(!info.is_empty());

    // DBSIZE on a database of its own (one pooled connection, db 11), so
    // tests running in parallel in db 0 cannot change the count.
    {
        let mut opts = hotkv::ClientOptions::from_url(&std::env::var("HOTKV_TEST_URL").unwrap()).unwrap();
        opts.pool_size = 1;
        opts.db = Some(11);
        let private = hotkv::Client::with_options(opts).await.unwrap();
        private.flushdb(false).await.unwrap();
        assert_eq!(private.dbsize().await.unwrap(), 0);
        private.set("a", "1").await.unwrap();
        private.set("b", "2").await.unwrap();
        assert_eq!(private.dbsize().await.unwrap(), 2);
        private.del(["a"]).await.unwrap();
        assert_eq!(private.dbsize().await.unwrap(), 1);
        private.flushdb(false).await.unwrap();
    }
    let k = common::unique("dbsize");
    client.set(&k, "v").await.unwrap();
    assert!(client.dbsize().await.unwrap() >= 1);

    let (secs, micros) = client.time().await.unwrap();
    assert!(secs > 0);
    assert!(micros < 1_000_000);

    client.client_setname("hotkv-rust-tests").await.unwrap();
    assert_eq!(client.client_getname().await.unwrap(), Some("hotkv-rust-tests".to_string()));
    assert!(client.client_id().await.unwrap() > 0);

    let cfg = client.config_get("maxmemory").await.unwrap();
    assert!(cfg.contains_key("maxmemory"), "{cfg:?}");
    assert!(cfg["maxmemory"].parse::<u64>().is_ok(), "{cfg:?}");

    client.del([k.as_str()]).await.unwrap();
}

#[tokio::test]
async fn generic_command_escape_hatch() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("escape");

    let v = client.command(hotkv::args::Cmd::new("SET").arg(&k).arg("via-command")).await.unwrap();
    assert!(matches!(v, Value::SimpleString(ref s) if s == "OK"));

    let v = client.execute("GET", [k.as_str()]).await.unwrap();
    assert_eq!(v.into_bytes().unwrap(), b"via-command".to_vec());

    let protocol = client.server_protocol_version().await.unwrap();
    assert!(protocol == 2 || protocol == 3);

    client.del([k.as_str()]).await.unwrap();
}

#[tokio::test]
async fn scripting() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("script");

    let sha = client.script_load("return redis.call('SET', KEYS[1], ARGV[1])").await.unwrap();
    assert_eq!(sha.len(), 40);
    assert_eq!(client.script_exists([sha.as_str()]).await.unwrap(), vec![true]);

    client.evalsha(&sha, [k.as_str()], ["value1"]).await.unwrap();
    assert_eq!(client.get(&k).await.unwrap(), Some("value1".to_string()));

    // eval_sha_or_load with a bogus sha falls back to EVAL and still
    // succeeds.
    client
        .eval_sha_or_load(
            "0000000000000000000000000000000000000000",
            "return redis.call('SET', KEYS[1], ARGV[1])",
            [k.as_str()],
            ["value2"],
        )
        .await
        .unwrap();
    assert_eq!(client.get(&k).await.unwrap(), Some("value2".to_string()));

    let direct = client.eval("return ARGV[1]", Vec::<String>::new(), ["direct"]).await.unwrap();
    assert_eq!(direct.into_bytes().unwrap(), b"direct".to_vec());
}

#[tokio::test]
async fn scan_family() {
    let Some(client) = common::connect().await else { return };
    let prefix = common::unique("scan");

    for i in 0..5 {
        client.set(format!("{prefix}:{i}"), i.to_string()).await.unwrap();
    }

    let mut seen = HashSet::new();
    let mut scan = client.scan().match_pattern(format!("{prefix}:*")).count(2);
    while let Some(item) = scan.next().await {
        seen.insert(item.unwrap());
    }
    assert_eq!(seen.len(), 5);

    let hkey = common::unique("hscan");
    client.hset(&hkey, (0..5).map(|i| (format!("f{i}"), i.to_string()))).await.unwrap();
    let mut hseen = HashSet::new();
    let mut hscan = client.hscan(&hkey);
    while let Some(item) = hscan.next().await {
        let (f, _v) = item.unwrap();
        hseen.insert(f);
    }
    assert_eq!(hseen.len(), 5);

    let skey = common::unique("sscan");
    client.sadd(&skey, (0..5).map(|i| i.to_string())).await.unwrap();
    let smembers = client.sscan(&skey).collect_all().await.unwrap();
    assert_eq!(smembers.len(), 5);

    let zkey = common::unique("zscan");
    client.zadd(&zkey, (0..5).map(|i| (i as f64, i.to_string()))).await.unwrap();
    let zmembers = client.zscan(&zkey).collect_all().await.unwrap();
    assert_eq!(zmembers.len(), 5);

    for i in 0..5 {
        client.del([format!("{prefix}:{i}")]).await.unwrap();
    }
    client.del([hkey.as_str(), skey.as_str(), zkey.as_str()]).await.unwrap();
}
