// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! The synchronous `BlockingClient`, from plain (non-async) test functions.

mod common;

use hotkv::blocking::BlockingClient;
use hotkv::Value;

fn client() -> Option<BlockingClient> {
    let Ok(url) = std::env::var("HOTKV_TEST_URL") else {
        println!("skipping: HOTKV_TEST_URL is not set");
        return None;
    };
    Some(BlockingClient::connect(&url).unwrap())
}

#[test]
fn blocking_wrappers_return_the_server_values() {
    let Some(c) = client() else { return };
    assert_eq!(c.ping().unwrap(), "PONG");

    let k = common::unique("blk");
    assert_eq!(c.get(&k).unwrap(), None);
    c.set(&k, "v").unwrap();
    assert_eq!(c.get(&k).unwrap().as_deref(), Some("v"));
    assert_eq!(c.exists([k.as_str()]).unwrap(), 1);
    assert!(c.expire(&k, 100).unwrap());
    assert!((1..=100).contains(&c.ttl(&k).unwrap()));
    assert_eq!(c.del([k.as_str()]).unwrap(), 1);
    assert_eq!(c.exists([k.as_str()]).unwrap(), 0);

    let n = common::unique("blk-n");
    assert_eq!(c.incr(&n).unwrap(), 1);
    assert_eq!(c.incr(&n).unwrap(), 2);

    let h = common::unique("blk-h");
    assert_eq!(c.hset(&h, [("a", "1"), ("b", "2")]).unwrap(), 2);
    assert_eq!(c.hget(&h, "b").unwrap().as_deref(), Some("2"));
    assert_eq!(c.hget(&h, "zz").unwrap(), None);

    let l = common::unique("blk-l");
    assert_eq!(c.rpush(&l, ["b", "c"]).unwrap(), 2);
    assert_eq!(c.lpush(&l, ["a"]).unwrap(), 3);
    assert_eq!(c.execute("LRANGE", [l.as_str(), "0", "-1"]).unwrap(), Value::Array(vec![Value::Bulk(b"a".to_vec()), Value::Bulk(b"b".to_vec()), Value::Bulk(b"c".to_vec())]));

    let s = common::unique("blk-s");
    assert_eq!(c.sadd(&s, ["x", "y", "x"]).unwrap(), 2);
    let z = common::unique("blk-z");
    assert_eq!(c.zadd(&z, [(1.5, "m"), (2.5, "n")]).unwrap(), 2);
    assert_eq!(c.execute("ZSCORE", [z.as_str(), "n"]).unwrap(), Value::Double(2.5) /* RESP3 reply */);

    // The typed accessors and the async client are reachable from here too.
    let tag_key = common::unique("blk-tag");
    c.set(&tag_key, "v").unwrap();
    let tag = common::unique("blk-tagname");
    assert_eq!(c.block_on(c.tag().set(&tag_key, [tag.as_str()])).unwrap(), 1);
    assert_eq!(c.block_on(c.async_client().tag().count(&tag_key)).unwrap(), 1);

    // A server error is an Err, a refused float never reaches the server.
    assert!(c.incr(&h).is_err());
    assert!(c.zadd(&z, [(f64::NAN, "bad")]).is_err());

    c.del([n.as_str(), h.as_str(), l.as_str(), s.as_str(), z.as_str(), tag_key.as_str()]).unwrap();
    c.close();
}
