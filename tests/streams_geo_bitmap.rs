// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

mod common;

use hotkv::commands::bitmap::BitOp;
use hotkv::commands::geo::{GeoBy, GeoFrom, GeoSearchOptions};
use hotkv::commands::streams::{Trim, XAddOptions};

#[tokio::test]
async fn streams() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("stream");

    let id1 = client.xadd(&k, "*", [("field", "v1")], &XAddOptions::default()).await.unwrap();
    assert!(!id1.is_empty());
    let id2 = client.xadd(&k, "*", [("field", "v2")], &XAddOptions::default()).await.unwrap();

    assert_eq!(client.xlen(&k).await.unwrap(), 2);

    let range = client.xrange(&k, "-", "+", None).await.unwrap();
    assert_eq!(range.len(), 2);
    assert_eq!(range[0].id, id1);
    assert_eq!(range[0].fields, vec![("field".to_string(), "v1".to_string())]);

    let rev = client.xrevrange(&k, "+", "-", None).await.unwrap();
    assert_eq!(rev[0].id, id2);

    let group = "g1";
    client.xgroup_create(&k, group, "0", false).await.unwrap();
    let read = client.xreadgroup(group, "consumer1", [(k.as_str(), ">")], None, None, false).await.unwrap();
    let messages = read.expect("data available");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].entries.len(), 2);

    assert_eq!(client.xack(&k, group, [id1.as_str()]).await.unwrap(), 1);

    let pending = client.xpending(&k, group).await.unwrap();
    assert_eq!(pending.count, 1);

    let claimed = client.xclaim(&k, group, "consumer2", 0, [id2.as_str()]).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, id2);

    let (_cursor, auto_claimed, _deleted) = client.xautoclaim(&k, group, "consumer3", 0, "0", None).await.unwrap();
    assert!(auto_claimed.iter().any(|e| e.id == id2));

    assert_eq!(client.xdel(&k, [id1.as_str()]).await.unwrap(), 1);
    assert_eq!(client.xlen(&k).await.unwrap(), 1);

    let trimmed = client.xtrim(&k, Trim::MaxLen(0), false, None).await.unwrap();
    assert_eq!(trimmed, 1);

    let info = client.xinfo_stream(&k).await.unwrap().into_pairs().unwrap();
    assert!(
        info.iter().any(|(name, _)| name.clone().into_bytes().ok().as_deref() == Some(&b"length"[..])),
        "XINFO STREAM reports a length field"
    );

    assert!(client.xgroup_destroy(&k, group).await.unwrap());
}

#[tokio::test]
async fn xread_without_group() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("stream-plain");
    client.xadd(&k, "*", [("a", "1")], &XAddOptions::default()).await.unwrap();

    let read = client.xread([(k.as_str(), "0")], None, None).await.unwrap();
    let messages = read.expect("data available");
    assert_eq!(messages[0].stream, k);
    assert_eq!(messages[0].entries.len(), 1);

    // Nothing new since the last (highest) ID: expect no data.
    let k2 = common::unique("stream-empty");
    client.xadd(&k2, "*", [("a", "1")], &XAddOptions::default()).await.unwrap();
    let read2 = client.xread([(k2.as_str(), "$")], None, None).await.unwrap();
    assert!(read2.is_none() || read2.unwrap().iter().all(|m| m.entries.is_empty()));
}

#[tokio::test]
async fn geo() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("geo");

    assert_eq!(
        client
            .geoadd(&k, [(13.361389, 38.115556, "Palermo"), (15.087269, 37.502669, "Catania")])
            .await
            .unwrap(),
        2
    );

    let positions = client.geopos(&k, ["Palermo", "missing"]).await.unwrap();
    assert!(positions[0].is_some());
    assert!(positions[1].is_none());

    let dist = client.geodist(&k, "Palermo", "Catania", Some("km")).await.unwrap();
    let dist = dist.expect("distance");
    assert!((166.0..167.0).contains(&dist));

    let results = client
        .geosearch(
            &k,
            GeoFrom::LonLat(15.0, 37.0),
            GeoBy::Radius(200.0, "km"),
            &GeoSearchOptions { withdist: true, withcoord: true, asc: true, ..Default::default() },
        )
        .await
        .unwrap();
    assert!(!results.is_empty());
    assert!(results.iter().any(|r| r.member == "Catania"));
    assert!(results[0].distance.is_some());
    assert!(results[0].coord.is_some());
}

#[tokio::test]
async fn bitmaps_and_hyperloglog() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("bits");

    assert_eq!(client.setbit(&k, 7, true).await.unwrap(), 0);
    assert!(client.getbit(&k, 7).await.unwrap() == 1);
    assert_eq!(client.bitcount(&k, None).await.unwrap(), 1);
    assert!(client.bitpos(&k, true, None, None, false).await.unwrap() >= 0);

    let k2 = common::unique("bits2");
    client.setbit(&k2, 0, true).await.unwrap();
    let dest = common::unique("bitsdest");
    let len = client.bitop(BitOp::Or, &dest, [k.as_str(), k2.as_str()]).await.unwrap();
    assert!(len >= 1);

    let hll = common::unique("hll");
    assert!(client.pfadd(&hll, ["a", "b", "c"]).await.unwrap());
    let count = client.pfcount([hll.as_str()]).await.unwrap();
    assert!((1..=5).contains(&count));

    let hll2 = common::unique("hll2");
    client.pfadd(&hll2, ["c", "d", "e"]).await.unwrap();
    let merged = common::unique("hllmerged");
    client.pfmerge(&merged, [hll.as_str(), hll2.as_str()]).await.unwrap();
    let merged_count = client.pfcount([merged.as_str()]).await.unwrap();
    assert!((3..=7).contains(&merged_count));
}
