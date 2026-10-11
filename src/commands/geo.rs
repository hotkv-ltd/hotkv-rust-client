// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Geospatial commands.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::value::{FromValue, Value};

/// The search origin for [`Client::geosearch`].
pub enum GeoFrom<'a> {
    Member(&'a str),
    LonLat(f64, f64),
}

/// The search area for [`Client::geosearch`]. `unit` is one of `"m"`,
/// `"km"`, `"mi"`, `"ft"`.
pub enum GeoBy {
    Radius(f64, &'static str),
    Box(f64, f64, &'static str),
}

/// Optional trailing arguments for [`Client::geosearch`].
#[derive(Debug, Clone, Copy, Default)]
pub struct GeoSearchOptions {
    pub asc: bool,
    pub desc: bool,
    pub count: Option<i64>,
    pub any: bool,
    pub withcoord: bool,
    pub withdist: bool,
    pub withhash: bool,
}

/// One `GEOSEARCH` result; fields beyond `member` are populated only when
/// the matching `WITH*` option was requested.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GeoSearchResult {
    pub member: String,
    pub distance: Option<f64>,
    pub hash: Option<i64>,
    pub coord: Option<(f64, f64)>,
}

fn parse_geosearch_item(item: Value, withdist: bool, withhash: bool, withcoord: bool) -> Result<GeoSearchResult> {
    if !withdist && !withhash && !withcoord {
        return Ok(GeoSearchResult {
            member: String::from_value(item)?,
            ..Default::default()
        });
    }
    let items = item.into_items()?;
    let mut it = items.into_iter();
    let mut next = || it.next().ok_or_else(|| Error::Protocol("malformed GEOSEARCH result".into()));
    let member = String::from_value(next()?)?;
    let distance = if withdist { Some(f64::from_value(next()?)?) } else { None };
    let hash = if withhash { Some(i64::from_value(next()?)?) } else { None };
    let coord = if withcoord { Some(<(f64, f64)>::from_value(next()?)?) } else { None };
    Ok(GeoSearchResult { member, distance, hash, coord })
}

impl Client {
    /// `GEOADD key longitude latitude member [longitude latitude member ...]`
    /// -> number of new members added.
    pub async fn geoadd<I, M>(&self, key: impl ToArg, members: I) -> Result<i64>
    where
        I: IntoIterator<Item = (f64, f64, M)>,
        M: ToArg,
    {
        let mut cmd = Cmd::new("GEOADD").arg(key);
        for (lon, lat, member) in members {
            cmd = cmd.arg(lon).arg(lat).arg(member);
        }
        self.call(cmd).await
    }

    /// `GEOPOS key member [member ...]` -> `(longitude, latitude)` per
    /// member, `None` for members that do not exist.
    pub async fn geopos<I, M>(&self, key: impl ToArg, members: I) -> Result<Vec<Option<(f64, f64)>>>
    where
        I: IntoIterator<Item = M>,
        M: ToArg,
    {
        self.call_ro(Cmd::new("GEOPOS").arg(key).args(members)).await
    }

    /// `GEODIST key member1 member2 [unit]` (`unit` one of `"m"`, `"km"`,
    /// `"mi"`, `"ft"`; `None` uses the server default, meters).
    pub async fn geodist(&self, key: impl ToArg, member1: impl ToArg, member2: impl ToArg, unit: Option<&str>) -> Result<Option<f64>> {
        let mut cmd = Cmd::new("GEODIST").arg(key).arg(member1).arg(member2);
        if let Some(u) = unit {
            cmd = cmd.arg(u);
        }
        self.call_ro(cmd).await
    }

    /// `GEOSEARCH key FROMMEMBER|FROMLONLAT ... BYRADIUS|BYBOX ... [ASC|DESC]
    /// [COUNT n [ANY]] [WITHCOORD] [WITHDIST] [WITHHASH]`.
    pub async fn geosearch(&self, key: impl ToArg, from: GeoFrom<'_>, by: GeoBy, opts: &GeoSearchOptions) -> Result<Vec<GeoSearchResult>> {
        let mut cmd = Cmd::new("GEOSEARCH").arg(key);
        cmd = match from {
            GeoFrom::Member(m) => cmd.arg("FROMMEMBER").arg(m),
            GeoFrom::LonLat(lon, lat) => cmd.arg("FROMLONLAT").arg(lon).arg(lat),
        };
        cmd = match by {
            GeoBy::Radius(radius, unit) => cmd.arg("BYRADIUS").arg(radius).arg(unit),
            GeoBy::Box(width, height, unit) => cmd.arg("BYBOX").arg(width).arg(height).arg(unit),
        };
        cmd = cmd.arg_if(opts.asc, "ASC");
        cmd = cmd.arg_if(opts.desc, "DESC");
        if let Some(c) = opts.count {
            cmd = cmd.arg("COUNT").arg(c);
            cmd = cmd.arg_if(opts.any, "ANY");
        }
        cmd = cmd.arg_if(opts.withcoord, "WITHCOORD");
        cmd = cmd.arg_if(opts.withdist, "WITHDIST");
        cmd = cmd.arg_if(opts.withhash, "WITHHASH");

        let items = self.command_idempotent(cmd).await?.into_items()?;
        items
            .into_iter()
            .map(|item| parse_geosearch_item(item, opts.withdist, opts.withhash, opts.withcoord))
            .collect()
    }
}
