// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Float arguments: what the SDK puts on the wire, and that the server
//! accepts it. The first group needs no server; the rest run against
//! `HOTKV_TEST_URL` and use only core commands (the enterprise ones answer
//! `NOLICENSE` on an unlicensed server and are covered by `wire_args.rs`).

mod common;

use hotkv::args::{Cmd, Score};
use hotkv::commands::geo::{GeoBy, GeoFrom, GeoSearchOptions};
use hotkv::commands::zsets::ZRangeOptions;
use hotkv::{Client, Error};

// ------------------------------------------------------------ no server --

#[tokio::test]
async fn nan_and_infinity_are_refused_before_anything_is_sent() {
    // Nothing listens on this port: if the SDK tried to send, the error
    // would be a connection error, not InvalidArgument.
    let client = Client::connect("hotkv://127.0.0.1:1").await.unwrap();
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let err = client.incrbyfloat("k", bad).await.unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "incrbyfloat({bad}): {err}");
        let err = client.hincrbyfloat("k", "f", bad).await.unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "hincrbyfloat({bad}): {err}");
        let err = client.geoadd("k", [(bad, 0.0, "m")]).await.unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "geoadd({bad}): {err}");
        let err = client.vsim().set("idx", "k", &[0.5, bad]).await.unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "vsim().set({bad}): {err}");
        let err = client.ts().add("k", "*", bad).await.unwrap_err();
        assert!(matches!(err, Error::InvalidArgument(_)), "ts().add({bad}): {err}");
    }
    // NaN is never a score; infinity is.
    let err = client.zadd("k", [(f64::NAN, "m")]).await.unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
    let err = client.zincrby("k", f64::NAN, "m").await.unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
    let err = client.execute("ZCOUNT", [Score(f64::NAN)]).await.unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
    // A pipeline holding a refused command is refused as a whole.
    let mut pipe = client.pipeline();
    pipe.queue(Cmd::new("SET").arg("a").arg("1"));
    pipe.queue(Cmd::new("INCRBYFLOAT").arg("a").arg(f64::NAN));
    let err = pipe.execute().await.unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
}

// ----------------------------------------------------------- live server --

#[tokio::test]
async fn zadd_scores_survive_tiny_huge_and_infinite_values() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("fz");
    let scores: [(f64, &str); 9] = [
        (f64::NEG_INFINITY, "ninf"),
        (-1e21, "neg-huge"),
        (-1e-5, "neg-tiny"),
        (1e-300, "tiny"),
        (0.1 + 0.2, "sum"),
        (1.0, "one"),
        (1e21, "huge"),
        (1.7976931348623157e308, "max"),
        (f64::INFINITY, "pinf"),
    ];
    assert_eq!(client.zadd(&k, scores).await.unwrap(), 9);
    for (score, member) in scores {
        let got = client.zscore(&k, member).await.unwrap().unwrap();
        assert_eq!(got, score, "{member}");
    }
    let order = client.zrange(&k, 0, -1, &ZRangeOptions::default()).await.unwrap();
    assert_eq!(order, vec!["ninf", "neg-huge", "neg-tiny", "tiny", "sum", "one", "huge", "max", "pinf"]);

    // Score range bounds: typed infinities, exclusive and exponent-free finite bounds.
    let tiny_range = client.zrangebyscore(&k, Score(1e-300), Score(1.0), None).await.unwrap();
    assert_eq!(tiny_range, vec!["tiny", "sum", "one"]);
    let upto = client.zcount(&k, Score(f64::NEG_INFINITY), Score(-1e-5)).await.unwrap();
    assert_eq!(upto, 3);
    let above = client.zcount(&k, Score(1e21), Score(f64::INFINITY)).await.unwrap();
    assert_eq!(above, 3);
    let none = client.zcount(&k, "(1e21", "(1.7976931348623157e308").await.unwrap();
    assert_eq!(none, 0);

    // INCR on a score keeps full precision.
    let v = client.zincrby(&k, 1e-5, "tiny").await.unwrap();
    assert_eq!(v, 1e-300 + 1e-5);
    let v = client.zincrby(&k, f64::INFINITY, "one").await.unwrap();
    assert_eq!(v, f64::INFINITY);
    let v = client.zadd_incr(&k, Default::default(), "neg-tiny", -1e-5).await.unwrap();
    assert_eq!(v, Some(-2e-5));
}

#[tokio::test]
async fn incrbyfloat_and_hincrbyfloat_accept_plain_decimals() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("fi");
    client.set(&k, "0").await.unwrap();

    let v = client.incrbyfloat(&k, 1e-5).await.unwrap();
    assert!((v - 1e-5).abs() < 1e-15, "{v}");
    let v = client.incrbyfloat(&k, 1e21).await.unwrap();
    assert!((v - 1e21).abs() / 1e21 < 1e-12, "{v}");
    client.set(&k, "10").await.unwrap();
    assert_eq!(client.incrbyfloat(&k, -2.5).await.unwrap(), 7.5);
    assert_eq!(client.incrbyfloat(&k, 3.0).await.unwrap(), 10.5);
    // -0.0 is sent as 0.
    assert_eq!(client.incrbyfloat(&k, -0.0).await.unwrap(), 10.5);

    let h = common::unique("fh");
    let v = client.hincrbyfloat(&h, "f", 1e-5).await.unwrap();
    assert!((v - 1e-5).abs() < 1e-15, "{v}");
    let v = client.hincrbyfloat(&h, "f", 1e21).await.unwrap();
    assert!((v - 1e21).abs() / 1e21 < 1e-12, "{v}");
    assert_eq!(client.hincrbyfloat(&h, "g", 0.25).await.unwrap(), 0.25);
    assert_eq!(client.hincrbyfloat(&h, "g", -0.5).await.unwrap(), -0.25);

    // After a refused (NaN) call nothing changed on the server.
    assert!(client.incrbyfloat(&k, f64::NAN).await.is_err());
    assert_eq!(client.get(&k).await.unwrap().as_deref(), Some("10.5"));
}

#[tokio::test]
async fn geo_accepts_tiny_and_negative_coordinates() {
    let Some(client) = common::connect().await else { return };
    let k = common::unique("fg");
    let added = client
        .geoadd(&k, [(1e-5, -1e-7, "origin"), (-122.27652, 37.805186, "oakland"), (179.99999, -85.0, "edge")])
        .await
        .unwrap();
    assert_eq!(added, 3);

    let pos = client.geopos(&k, ["origin", "oakland"]).await.unwrap();
    let (lon, lat) = pos[0].unwrap();
    assert!(lon.abs() < 1e-4 && lat.abs() < 1e-4, "{lon} {lat}");
    let (lon, lat) = pos[1].unwrap();
    assert!((lon + 122.27652).abs() < 1e-4 && (lat - 37.805186).abs() < 1e-4);

    // Radius and box sizes go through the same formatter.
    let near = client
        .geosearch(&k, GeoFrom::LonLat(0.0, 0.0), GeoBy::Radius(0.5, "km"), &GeoSearchOptions::default())
        .await
        .unwrap();
    assert_eq!(near.iter().map(|r| r.member.as_str()).collect::<Vec<_>>(), vec!["origin"]);
    let boxed = client
        .geosearch(&k, GeoFrom::Member("oakland"), GeoBy::Box(1e-3, 1e-3, "km"), &GeoSearchOptions::default())
        .await
        .unwrap();
    assert_eq!(boxed.len(), 1);
    let huge = client
        .geosearch(&k, GeoFrom::LonLat(0.0, 0.0), GeoBy::Radius(1e7, "km"), &GeoSearchOptions { asc: true, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(huge.len(), 3);
}

#[tokio::test]
async fn vsim_vectors_use_f32_range_decimals() {
    let Some(client) = common::connect().await else { return };
    let idx = common::unique("fv");
    client.vsim().create_index(&idx, 4, Some("l2"), None, None, None).await.unwrap();

    // Components the server must read as 32 bit floats: tiny, huge, negative,
    // integral, and a value that is not exact in binary.
    let a = [1e-30, 3.0e38, -2.5, 0.1];
    assert!(client.vsim().set(&idx, "a", &a).await.unwrap());
    let back = client.vsim().get(&idx, "a").await.unwrap().unwrap();
    assert_eq!(back.len(), 4);
    for (got, want) in back.iter().zip(a) {
        assert_eq!(*got as f32, want as f32, "{back:?}");
    }
    assert!(client.vsim().set(&idx, "b", &[0.0, 0.0, 0.0, 0.0]).await.unwrap());
    let hits = client.vsim().search(&idx, 2, &[0.0, 0.0, -2.5, 0.1], &[], None).await.unwrap();
    assert_eq!(hits.len(), 2);

    // A value outside the f32 range is refused by the SDK instead of the server.
    let err = client.vsim().set(&idx, "c", &[1e39, 0.0, 0.0, 0.0]).await.unwrap_err();
    assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
    assert!(client.vsim().get(&idx, "c").await.unwrap().is_none());

    client.vsim().drop_index(&idx).await.unwrap();
}
