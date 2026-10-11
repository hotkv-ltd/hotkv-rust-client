// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Sorted set commands.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::Result;
use crate::value::{pairs_vec, FromValue, Value};

/// Optional flags for [`Client::zadd_options`]/[`Client::zadd_incr`].
/// `nx`/`xx` and `gt`/`lt` are each mutually exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ZAddOptions {
    pub nx: bool,
    pub xx: bool,
    pub gt: bool,
    pub lt: bool,
    /// `CH`: count changed elements (added or updated), not just added.
    pub ch: bool,
}

impl ZAddOptions {
    fn apply(self, mut cmd: Cmd) -> Cmd {
        cmd = cmd.arg_if(self.nx, "NX");
        cmd = cmd.arg_if(self.xx, "XX");
        cmd = cmd.arg_if(self.gt, "GT");
        cmd = cmd.arg_if(self.lt, "LT");
        cmd.arg_if(self.ch, "CH")
    }
}

/// `BYSCORE`/`BYLEX` selector for [`Client::zrange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZRangeBy {
    Score,
    Lex,
}

/// Optional trailing arguments for [`Client::zrange`]/
/// [`Client::zrange_withscores`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ZRangeOptions {
    pub by: Option<ZRangeBy>,
    pub rev: bool,
    /// `LIMIT offset count`, only valid together with `by`.
    pub limit: Option<(i64, i64)>,
}

/// `AGGREGATE` mode for [`Client::zunionstore`]/[`Client::zinterstore`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aggregate {
    Sum,
    Min,
    Max,
}

impl Aggregate {
    fn as_str(self) -> &'static str {
        match self {
            Aggregate::Sum => "SUM",
            Aggregate::Min => "MIN",
            Aggregate::Max => "MAX",
        }
    }
}

fn append_range_opts(mut cmd: Cmd, opts: &ZRangeOptions) -> Cmd {
    match opts.by {
        Some(ZRangeBy::Score) => cmd = cmd.arg("BYSCORE"),
        Some(ZRangeBy::Lex) => cmd = cmd.arg("BYLEX"),
        None => {}
    }
    cmd = cmd.arg_if(opts.rev, "REV");
    if let Some((offset, count)) = opts.limit {
        cmd = cmd.arg("LIMIT").arg(offset).arg(count);
    }
    cmd
}

fn zrange_cmd(key: impl ToArg, min: impl ToArg, max: impl ToArg, opts: &ZRangeOptions) -> Cmd {
    append_range_opts(Cmd::new("ZRANGE").arg(key).arg(min).arg(max), opts)
}

async fn pop_one(client: &Client, cmd: Cmd) -> Result<Option<(String, f64)>> {
    let items = client.command(cmd).await?.into_items()?;
    if items.is_empty() {
        return Ok(None);
    }
    <(String, f64)>::from_value(Value::Array(items)).map(Some)
}

impl Client {
    /// `ZADD key score member [score member ...]` -> number of members
    /// added (not counting updates).
    pub async fn zadd<I, M>(&self, key: impl ToArg, members: I) -> Result<i64>
    where
        I: IntoIterator<Item = (f64, M)>,
        M: ToArg,
    {
        let mut cmd = Cmd::new("ZADD").arg(key);
        for (score, member) in members {
            cmd = cmd.arg_score(score).arg(member);
        }
        self.call(cmd).await
    }

    /// `ZADD key [NX|XX] [GT|LT] [CH] score member [score member ...]`,
    /// returned as the raw reply (an integer count).
    pub async fn zadd_options<I, M>(&self, key: impl ToArg, opts: ZAddOptions, members: I) -> Result<i64>
    where
        I: IntoIterator<Item = (f64, M)>,
        M: ToArg,
    {
        let mut cmd = opts.apply(Cmd::new("ZADD").arg(key));
        for (score, member) in members {
            cmd = cmd.arg_score(score).arg(member);
        }
        self.call(cmd).await
    }

    /// `ZADD key [NX|XX] [GT|LT] INCR delta member` -> the member's new
    /// score, or `None` if an `NX`/`XX`/`GT`/`LT` condition blocked it.
    pub async fn zadd_incr(&self, key: impl ToArg, opts: ZAddOptions, member: impl ToArg, delta: f64) -> Result<Option<f64>> {
        let cmd = opts.apply(Cmd::new("ZADD").arg(key)).arg("INCR").arg_score(delta).arg(member);
        self.call(cmd).await
    }

    /// `ZSCORE key member`.
    pub async fn zscore(&self, key: impl ToArg, member: impl ToArg) -> Result<Option<f64>> {
        self.call_ro(Cmd::new("ZSCORE").arg(key).arg(member)).await
    }

    /// `ZMSCORE key member [member ...]`.
    pub async fn zmscore<I, M>(&self, key: impl ToArg, members: I) -> Result<Vec<Option<f64>>>
    where
        I: IntoIterator<Item = M>,
        M: ToArg,
    {
        self.call_ro(Cmd::new("ZMSCORE").arg(key).args(members)).await
    }

    /// `ZRANK key member`.
    pub async fn zrank(&self, key: impl ToArg, member: impl ToArg) -> Result<Option<i64>> {
        self.call_ro(Cmd::new("ZRANK").arg(key).arg(member)).await
    }

    /// `ZRANK key member WITHSCORE` -> `(rank, score)`.
    pub async fn zrank_withscore(&self, key: impl ToArg, member: impl ToArg) -> Result<Option<(i64, f64)>> {
        let v = self
            .command_idempotent(Cmd::new("ZRANK").arg(key).arg(member).arg("WITHSCORE"))
            .await?;
        if v.is_nil() {
            return Ok(None);
        }
        <(i64, f64)>::from_value(v).map(Some)
    }

    /// `ZREVRANK key member`.
    pub async fn zrevrank(&self, key: impl ToArg, member: impl ToArg) -> Result<Option<i64>> {
        self.call_ro(Cmd::new("ZREVRANK").arg(key).arg(member)).await
    }

    /// `ZREVRANK key member WITHSCORE` -> `(rank, score)`.
    pub async fn zrevrank_withscore(&self, key: impl ToArg, member: impl ToArg) -> Result<Option<(i64, f64)>> {
        let v = self
            .command_idempotent(Cmd::new("ZREVRANK").arg(key).arg(member).arg("WITHSCORE"))
            .await?;
        if v.is_nil() {
            return Ok(None);
        }
        <(i64, f64)>::from_value(v).map(Some)
    }

    /// `ZREM key member [member ...]` -> number removed.
    pub async fn zrem<I, M>(&self, key: impl ToArg, members: I) -> Result<i64>
    where
        I: IntoIterator<Item = M>,
        M: ToArg,
    {
        self.call(Cmd::new("ZREM").arg(key).args(members)).await
    }

    /// `ZINCRBY key delta member`.
    pub async fn zincrby(&self, key: impl ToArg, delta: f64, member: impl ToArg) -> Result<f64> {
        self.call(Cmd::new("ZINCRBY").arg(key).arg_score(delta).arg(member)).await
    }

    /// `ZCARD key`.
    pub async fn zcard(&self, key: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("ZCARD").arg(key)).await
    }

    /// `ZCOUNT key min max`.
    pub async fn zcount(&self, key: impl ToArg, min: impl ToArg, max: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("ZCOUNT").arg(key).arg(min).arg(max)).await
    }

    /// `ZLEXCOUNT key min max`.
    pub async fn zlexcount(&self, key: impl ToArg, min: impl ToArg, max: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("ZLEXCOUNT").arg(key).arg(min).arg(max)).await
    }

    /// `ZPOPMIN key`.
    pub async fn zpopmin(&self, key: impl ToArg) -> Result<Option<(String, f64)>> {
        pop_one(self, Cmd::new("ZPOPMIN").arg(key)).await
    }

    /// `ZPOPMIN key count`.
    pub async fn zpopmin_count(&self, key: impl ToArg, count: i64) -> Result<Vec<(String, f64)>> {
        pairs_vec(self.command(Cmd::new("ZPOPMIN").arg(key).arg(count)).await?)
    }

    /// `ZPOPMAX key`.
    pub async fn zpopmax(&self, key: impl ToArg) -> Result<Option<(String, f64)>> {
        pop_one(self, Cmd::new("ZPOPMAX").arg(key)).await
    }

    /// `ZPOPMAX key count`.
    pub async fn zpopmax_count(&self, key: impl ToArg, count: i64) -> Result<Vec<(String, f64)>> {
        pairs_vec(self.command(Cmd::new("ZPOPMAX").arg(key).arg(count)).await?)
    }

    /// `ZRANDMEMBER key`.
    pub async fn zrandmember(&self, key: impl ToArg) -> Result<Option<String>> {
        self.call_ro(Cmd::new("ZRANDMEMBER").arg(key)).await
    }

    /// `ZRANDMEMBER key count`. A negative count allows repeats.
    pub async fn zrandmember_count(&self, key: impl ToArg, count: i64) -> Result<Vec<String>> {
        self.call_ro(Cmd::new("ZRANDMEMBER").arg(key).arg(count)).await
    }

    /// `ZRANDMEMBER key count WITHSCORES`.
    pub async fn zrandmember_with_scores(&self, key: impl ToArg, count: i64) -> Result<Vec<(String, f64)>> {
        let cmd = Cmd::new("ZRANDMEMBER").arg(key).arg(count).arg("WITHSCORES");
        pairs_vec(self.command_idempotent(cmd).await?)
    }

    /// `ZRANGE key min max [BYSCORE|BYLEX] [REV] [LIMIT offset count]`.
    /// `min`/`max` are plain indexes for the default (by index) mode,
    /// score bounds (`"-inf"`, `"(5"`, ...) for `BYSCORE`, or lex bounds
    /// (`"[a"`, `"-"`, `"+"`, ...) for `BYLEX`. With `opts.rev` and
    /// `BYSCORE`/`BYLEX`, pass the higher bound as `min` and the lower
    /// bound as `max`: the server reads them positionally, not by name.
    pub async fn zrange(&self, key: impl ToArg, min: impl ToArg, max: impl ToArg, opts: &ZRangeOptions) -> Result<Vec<String>> {
        self.call_ro(zrange_cmd(key, min, max, opts)).await
    }

    /// `ZRANGE key min max ... WITHSCORES`.
    pub async fn zrange_withscores(
        &self,
        key: impl ToArg,
        min: impl ToArg,
        max: impl ToArg,
        opts: &ZRangeOptions,
    ) -> Result<Vec<(String, f64)>> {
        let cmd = zrange_cmd(key, min, max, opts).arg("WITHSCORES");
        pairs_vec(self.command_idempotent(cmd).await?)
    }

    /// `ZRANGESTORE destination source min max [BYSCORE|BYLEX] [REV]
    /// [LIMIT offset count]` -> size of the stored result. See [`Client::zrange`]
    /// for how `min`/`max` interact with `opts.rev`.
    pub async fn zrangestore(
        &self,
        destination: impl ToArg,
        source: impl ToArg,
        min: impl ToArg,
        max: impl ToArg,
        opts: &ZRangeOptions,
    ) -> Result<i64> {
        let cmd = append_range_opts(Cmd::new("ZRANGESTORE").arg(destination).arg(source).arg(min).arg(max), opts);
        self.call(cmd).await
    }

    /// `ZREVRANGE key start stop` (legacy index based reverse range).
    pub async fn zrevrange(&self, key: impl ToArg, start: i64, stop: i64) -> Result<Vec<String>> {
        self.call_ro(Cmd::new("ZREVRANGE").arg(key).arg(start).arg(stop)).await
    }

    /// `ZREVRANGE key start stop WITHSCORES`.
    pub async fn zrevrange_withscores(&self, key: impl ToArg, start: i64, stop: i64) -> Result<Vec<(String, f64)>> {
        let cmd = Cmd::new("ZREVRANGE").arg(key).arg(start).arg(stop).arg("WITHSCORES");
        pairs_vec(self.command_idempotent(cmd).await?)
    }

    /// `ZRANGEBYSCORE key min max [LIMIT offset count]` (legacy form).
    pub async fn zrangebyscore(&self, key: impl ToArg, min: impl ToArg, max: impl ToArg, limit: Option<(i64, i64)>) -> Result<Vec<String>> {
        let mut cmd = Cmd::new("ZRANGEBYSCORE").arg(key).arg(min).arg(max);
        if let Some((o, c)) = limit {
            cmd = cmd.arg("LIMIT").arg(o).arg(c);
        }
        self.call_ro(cmd).await
    }

    /// `ZRANGEBYSCORE key min max WITHSCORES [LIMIT offset count]`.
    pub async fn zrangebyscore_withscores(
        &self,
        key: impl ToArg,
        min: impl ToArg,
        max: impl ToArg,
        limit: Option<(i64, i64)>,
    ) -> Result<Vec<(String, f64)>> {
        let mut cmd = Cmd::new("ZRANGEBYSCORE").arg(key).arg(min).arg(max).arg("WITHSCORES");
        if let Some((o, c)) = limit {
            cmd = cmd.arg("LIMIT").arg(o).arg(c);
        }
        pairs_vec(self.command_idempotent(cmd).await?)
    }

    /// `ZREVRANGEBYSCORE key max min [LIMIT offset count]` (legacy form;
    /// note the max/min argument order).
    pub async fn zrevrangebyscore(&self, key: impl ToArg, max: impl ToArg, min: impl ToArg, limit: Option<(i64, i64)>) -> Result<Vec<String>> {
        let mut cmd = Cmd::new("ZREVRANGEBYSCORE").arg(key).arg(max).arg(min);
        if let Some((o, c)) = limit {
            cmd = cmd.arg("LIMIT").arg(o).arg(c);
        }
        self.call_ro(cmd).await
    }

    /// `ZRANGEBYLEX key min max [LIMIT offset count]`.
    pub async fn zrangebylex(&self, key: impl ToArg, min: impl ToArg, max: impl ToArg, limit: Option<(i64, i64)>) -> Result<Vec<String>> {
        let mut cmd = Cmd::new("ZRANGEBYLEX").arg(key).arg(min).arg(max);
        if let Some((o, c)) = limit {
            cmd = cmd.arg("LIMIT").arg(o).arg(c);
        }
        self.call_ro(cmd).await
    }

    /// `ZREVRANGEBYLEX key max min [LIMIT offset count]`.
    pub async fn zrevrangebylex(&self, key: impl ToArg, max: impl ToArg, min: impl ToArg, limit: Option<(i64, i64)>) -> Result<Vec<String>> {
        let mut cmd = Cmd::new("ZREVRANGEBYLEX").arg(key).arg(max).arg(min);
        if let Some((o, c)) = limit {
            cmd = cmd.arg("LIMIT").arg(o).arg(c);
        }
        self.call_ro(cmd).await
    }

    /// `ZREMRANGEBYRANK key start stop` -> number removed.
    pub async fn zremrangebyrank(&self, key: impl ToArg, start: i64, stop: i64) -> Result<i64> {
        self.call(Cmd::new("ZREMRANGEBYRANK").arg(key).arg(start).arg(stop)).await
    }

    /// `ZREMRANGEBYSCORE key min max`.
    pub async fn zremrangebyscore(&self, key: impl ToArg, min: impl ToArg, max: impl ToArg) -> Result<i64> {
        self.call(Cmd::new("ZREMRANGEBYSCORE").arg(key).arg(min).arg(max)).await
    }

    /// `ZREMRANGEBYLEX key min max`.
    pub async fn zremrangebylex(&self, key: impl ToArg, min: impl ToArg, max: impl ToArg) -> Result<i64> {
        self.call(Cmd::new("ZREMRANGEBYLEX").arg(key).arg(min).arg(max)).await
    }

    /// `ZUNIONSTORE destination numkeys key [key ...] [WEIGHTS w ...]
    /// [AGGREGATE SUM|MIN|MAX]` -> the result set's size.
    pub async fn zunionstore<I, K>(
        &self,
        destination: impl ToArg,
        keys: I,
        weights: Option<&[f64]>,
        aggregate: Option<Aggregate>,
    ) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call(zstore_cmd("ZUNIONSTORE", destination, keys, weights, aggregate)).await
    }

    /// `ZINTERSTORE destination numkeys key [key ...] [WEIGHTS w ...]
    /// [AGGREGATE SUM|MIN|MAX]`.
    pub async fn zinterstore<I, K>(
        &self,
        destination: impl ToArg,
        keys: I,
        weights: Option<&[f64]>,
        aggregate: Option<Aggregate>,
    ) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call(zstore_cmd("ZINTERSTORE", destination, keys, weights, aggregate)).await
    }
}

fn zstore_cmd<I, K>(name: &str, destination: impl ToArg, keys: I, weights: Option<&[f64]>, aggregate: Option<Aggregate>) -> Cmd
where
    I: IntoIterator<Item = K>,
    K: ToArg,
{
    let keys: Vec<K> = keys.into_iter().collect();
    let mut cmd = Cmd::new(name).arg(destination).arg(keys.len() as i64).args(keys);
    if let Some(w) = weights {
        cmd = cmd.arg("WEIGHTS");
        for weight in w {
            cmd = cmd.arg_score(*weight);
        }
    }
    if let Some(a) = aggregate {
        cmd = cmd.arg("AGGREGATE").arg(a.as_str());
    }
    cmd
}
