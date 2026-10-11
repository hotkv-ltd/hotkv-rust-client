// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Stream commands (`XADD`, `XRANGE`, `XREAD`, consumer groups, ...).

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::value::{pairs_vec, FromValue, Value};

/// One stream entry: its ID plus its field/value pairs.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamEntry {
    pub id: String,
    pub fields: Vec<(String, String)>,
}

impl FromValue for StreamEntry {
    fn from_value(value: Value) -> Result<Self> {
        let mut items = value.into_items()?;
        if items.len() != 2 {
            return Err(Error::Protocol("malformed stream entry".into()));
        }
        let fields_v = items.pop().unwrap();
        let id = String::from_value(items.pop().unwrap())?;
        let fields = if fields_v.is_nil() {
            Vec::new() // a tombstoned entry inside XCLAIM/XRANGE output
        } else {
            pairs_vec(fields_v)?
        };
        Ok(StreamEntry { id, fields })
    }
}

/// The entries returned for one stream by `XREAD`/`XREADGROUP`.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamMessages {
    pub stream: String,
    pub entries: Vec<StreamEntry>,
}

impl FromValue for StreamMessages {
    fn from_value(value: Value) -> Result<Self> {
        let mut items = value.into_items()?;
        if items.len() != 2 {
            return Err(Error::Protocol("malformed XREAD stream entry".into()));
        }
        let entries_v = items.pop().unwrap();
        let stream = String::from_value(items.pop().unwrap())?;
        let entries = Vec::<StreamEntry>::from_value(entries_v)?;
        Ok(StreamMessages { stream, entries })
    }
}

/// One consumer's pending count, from the `XPENDING key group` summary
/// form.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingSummary {
    pub count: i64,
    pub min_id: Option<String>,
    pub max_id: Option<String>,
    pub consumers: Vec<(String, i64)>,
}

/// One pending entry, from the extended `XPENDING` form.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingEntry {
    pub id: String,
    pub consumer: String,
    pub idle_ms: i64,
    pub deliveries: i64,
}

impl FromValue for PendingEntry {
    fn from_value(value: Value) -> Result<Self> {
        let (id, consumer, idle_ms, deliveries) = FromValue::from_value(value)?;
        Ok(PendingEntry { id, consumer, idle_ms, deliveries })
    }
}

/// Trimming strategy shared by [`Client::xadd`] and [`Client::xtrim`].
#[derive(Debug, Clone, Copy)]
pub enum Trim {
    MaxLen(i64),
    MinId(i64),
}

/// Optional trailing arguments for [`Client::xadd`].
#[derive(Debug, Clone, Copy, Default)]
pub struct XAddOptions {
    pub nomkstream: bool,
    pub trim: Option<Trim>,
    /// Approximate trimming (`~`) instead of exact (`=`).
    pub approx: bool,
    pub limit: Option<i64>,
}

fn apply_trim(mut cmd: Cmd, trim: Option<Trim>, approx: bool, limit: Option<i64>) -> Cmd {
    match trim {
        Some(Trim::MaxLen(n)) => {
            cmd = cmd.arg("MAXLEN");
            cmd = cmd.arg_if(approx, "~");
            cmd = cmd.arg(n);
        }
        Some(Trim::MinId(id)) => {
            cmd = cmd.arg("MINID");
            cmd = cmd.arg_if(approx, "~");
            cmd = cmd.arg(id);
        }
        None => return cmd,
    }
    cmd.kw_opt("LIMIT", limit)
}

impl Client {
    /// `XADD key [NOMKSTREAM] [MAXLEN|MINID [~] threshold [LIMIT n]] id
    /// field value [field value ...]`. Pass `"*"` for `id` to auto
    /// generate one. Returns the ID used.
    pub async fn xadd<I, F, V>(&self, key: impl ToArg, id: impl ToArg, fields: I, opts: &XAddOptions) -> Result<String>
    where
        I: IntoIterator<Item = (F, V)>,
        F: ToArg,
        V: ToArg,
    {
        let mut cmd = Cmd::new("XADD").arg(key);
        cmd = cmd.arg_if(opts.nomkstream, "NOMKSTREAM");
        cmd = apply_trim(cmd, opts.trim, opts.approx, opts.limit);
        cmd = cmd.arg(id);
        for (f, v) in fields {
            cmd = cmd.arg(f).arg(v);
        }
        self.call(cmd).await
    }

    /// `XLEN key`.
    pub async fn xlen(&self, key: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("XLEN").arg(key)).await
    }

    /// `XDEL key id [id ...]` -> number of entries removed.
    pub async fn xdel<I, Id>(&self, key: impl ToArg, ids: I) -> Result<i64>
    where
        I: IntoIterator<Item = Id>,
        Id: ToArg,
    {
        self.call(Cmd::new("XDEL").arg(key).args(ids)).await
    }

    /// `XTRIM key MAXLEN|MINID [~] threshold [LIMIT n]` -> number of
    /// entries removed.
    pub async fn xtrim(&self, key: impl ToArg, trim: Trim, approx: bool, limit: Option<i64>) -> Result<i64> {
        let cmd = apply_trim(Cmd::new("XTRIM").arg(key), Some(trim), approx, limit);
        self.call(cmd).await
    }

    /// `XRANGE key start end [COUNT n]`.
    pub async fn xrange(&self, key: impl ToArg, start: impl ToArg, end: impl ToArg, count: Option<i64>) -> Result<Vec<StreamEntry>> {
        let cmd = Cmd::new("XRANGE").arg(key).arg(start).arg(end).kw_opt("COUNT", count);
        self.call_ro(cmd).await
    }

    /// `XREVRANGE key end start [COUNT n]`.
    pub async fn xrevrange(&self, key: impl ToArg, end: impl ToArg, start: impl ToArg, count: Option<i64>) -> Result<Vec<StreamEntry>> {
        let cmd = Cmd::new("XREVRANGE").arg(key).arg(end).arg(start).kw_opt("COUNT", count);
        self.call_ro(cmd).await
    }

    /// `XREAD [COUNT n] [BLOCK ms] STREAMS key [key ...] id [id ...]`.
    /// `None` when the read timed out (`BLOCK`) or nothing new was
    /// available.
    pub async fn xread<I, K, Id>(&self, streams: I, count: Option<i64>, block_ms: Option<i64>) -> Result<Option<Vec<StreamMessages>>>
    where
        I: IntoIterator<Item = (K, Id)>,
        K: ToArg,
        Id: ToArg,
    {
        let cmd = xread_cmd("XREAD", None, count, block_ms, false, streams);
        let v = self.command(cmd).await?;
        if v.is_nil() {
            return Ok(None);
        }
        parse_xread_reply(v).map(Some)
    }

    /// `XREADGROUP GROUP group consumer [COUNT n] [BLOCK ms] [NOACK]
    /// STREAMS key [key ...] id [id ...]`.
    pub async fn xreadgroup<I, K, Id>(
        &self,
        group: &str,
        consumer: &str,
        streams: I,
        count: Option<i64>,
        block_ms: Option<i64>,
        noack: bool,
    ) -> Result<Option<Vec<StreamMessages>>>
    where
        I: IntoIterator<Item = (K, Id)>,
        K: ToArg,
        Id: ToArg,
    {
        let cmd = xread_cmd("XREADGROUP", Some((group, consumer)), count, block_ms, noack, streams);
        let v = self.command(cmd).await?;
        if v.is_nil() {
            return Ok(None);
        }
        parse_xread_reply(v).map(Some)
    }

    /// `XACK key group id [id ...]` -> number acknowledged.
    pub async fn xack<I, Id>(&self, key: impl ToArg, group: impl ToArg, ids: I) -> Result<i64>
    where
        I: IntoIterator<Item = Id>,
        Id: ToArg,
    {
        self.call(Cmd::new("XACK").arg(key).arg(group).args(ids)).await
    }

    /// `XGROUP CREATE key group id_or_dollar [MKSTREAM]`.
    pub async fn xgroup_create(&self, key: impl ToArg, group: impl ToArg, id: impl ToArg, mkstream: bool) -> Result<()> {
        let cmd = Cmd::new("XGROUP")
            .arg("CREATE")
            .arg(key)
            .arg(group)
            .arg(id)
            .arg_if(mkstream, "MKSTREAM");
        self.call(cmd).await
    }

    /// `XGROUP DESTROY key group` -> whether it existed.
    pub async fn xgroup_destroy(&self, key: impl ToArg, group: impl ToArg) -> Result<bool> {
        self.call(Cmd::new("XGROUP").arg("DESTROY").arg(key).arg(group)).await
    }

    /// `XGROUP SETID key group id_or_dollar`.
    pub async fn xgroup_setid(&self, key: impl ToArg, group: impl ToArg, id: impl ToArg) -> Result<()> {
        self.call(Cmd::new("XGROUP").arg("SETID").arg(key).arg(group).arg(id)).await
    }

    /// `XGROUP CREATECONSUMER key group consumer` -> whether it was
    /// created.
    pub async fn xgroup_createconsumer(&self, key: impl ToArg, group: impl ToArg, consumer: impl ToArg) -> Result<bool> {
        self.call(Cmd::new("XGROUP").arg("CREATECONSUMER").arg(key).arg(group).arg(consumer))
            .await
    }

    /// `XGROUP DELCONSUMER key group consumer` -> number of pending
    /// entries that consumer had.
    pub async fn xgroup_delconsumer(&self, key: impl ToArg, group: impl ToArg, consumer: impl ToArg) -> Result<i64> {
        self.call(Cmd::new("XGROUP").arg("DELCONSUMER").arg(key).arg(group).arg(consumer))
            .await
    }

    /// `XPENDING key group` (summary form).
    pub async fn xpending(&self, key: impl ToArg, group: impl ToArg) -> Result<PendingSummary> {
        let mut items = self.command_idempotent(Cmd::new("XPENDING").arg(key).arg(group)).await?.into_items()?;
        if items.len() != 4 {
            return Err(Error::Protocol("malformed XPENDING summary reply".into()));
        }
        let consumers_v = items.pop().unwrap();
        let max_id = Option::<String>::from_value(items.pop().unwrap())?;
        let min_id = Option::<String>::from_value(items.pop().unwrap())?;
        let count = i64::from_value(items.pop().unwrap())?;
        let consumers = if consumers_v.is_nil() {
            Vec::new()
        } else {
            consumers_v
                .into_items()?
                .into_iter()
                .map(<(String, i64)>::from_value)
                .collect::<Result<_>>()?
        };
        Ok(PendingSummary { count, min_id, max_id, consumers })
    }

    /// `XPENDING key group start end count [consumer]` (extended form).
    pub async fn xpending_extended(
        &self,
        key: impl ToArg,
        group: impl ToArg,
        start: impl ToArg,
        end: impl ToArg,
        count: i64,
        consumer: Option<&str>,
    ) -> Result<Vec<PendingEntry>> {
        let mut cmd = Cmd::new("XPENDING").arg(key).arg(group).arg(start).arg(end).arg(count);
        if let Some(c) = consumer {
            cmd = cmd.arg(c);
        }
        self.call_ro(cmd).await
    }

    /// `XCLAIM key group consumer min_idle_ms id [id ...]`.
    pub async fn xclaim<I, Id>(&self, key: impl ToArg, group: impl ToArg, consumer: impl ToArg, min_idle_ms: i64, ids: I) -> Result<Vec<StreamEntry>>
    where
        I: IntoIterator<Item = Id>,
        Id: ToArg,
    {
        let cmd = Cmd::new("XCLAIM").arg(key).arg(group).arg(consumer).arg(min_idle_ms).args(ids);
        self.call(cmd).await
    }

    /// `XCLAIM key group consumer min_idle_ms id [id ...] JUSTID`.
    pub async fn xclaim_justid<I, Id>(
        &self,
        key: impl ToArg,
        group: impl ToArg,
        consumer: impl ToArg,
        min_idle_ms: i64,
        ids: I,
    ) -> Result<Vec<String>>
    where
        I: IntoIterator<Item = Id>,
        Id: ToArg,
    {
        let cmd = Cmd::new("XCLAIM")
            .arg(key)
            .arg(group)
            .arg(consumer)
            .arg(min_idle_ms)
            .args(ids)
            .arg("JUSTID");
        self.call(cmd).await
    }

    /// `XAUTOCLAIM key group consumer min_idle_ms start [COUNT n]` ->
    /// `(next_cursor, claimed_entries, deleted_ids)`.
    pub async fn xautoclaim(
        &self,
        key: impl ToArg,
        group: impl ToArg,
        consumer: impl ToArg,
        min_idle_ms: i64,
        start: impl ToArg,
        count: Option<i64>,
    ) -> Result<(String, Vec<StreamEntry>, Vec<String>)> {
        let cmd = Cmd::new("XAUTOCLAIM")
            .arg(key)
            .arg(group)
            .arg(consumer)
            .arg(min_idle_ms)
            .arg(start)
            .kw_opt("COUNT", count);
        let mut items = self.command(cmd).await?.into_items()?;
        match items.len() {
            3 => {
                let deleted = Vec::<String>::from_value(items.remove(2))?;
                let entries = Vec::<StreamEntry>::from_value(items.remove(1))?;
                let cursor = String::from_value(items.remove(0))?;
                Ok((cursor, entries, deleted))
            }
            2 => {
                let entries = Vec::<StreamEntry>::from_value(items.remove(1))?;
                let cursor = String::from_value(items.remove(0))?;
                Ok((cursor, entries, Vec::new()))
            }
            other => Err(Error::Protocol(format!("malformed XAUTOCLAIM reply with {} elements", other))),
        }
    }

    /// `XINFO STREAM key`, as the raw reply (a flat field/value list on
    /// RESP2, or a map on RESP3): decode it with
    /// [`crate::Value::into_pairs`] for the fields you need.
    pub async fn xinfo_stream(&self, key: impl ToArg) -> Result<Value> {
        self.command_idempotent(Cmd::new("XINFO").arg("STREAM").arg(key)).await
    }

    /// `XINFO GROUPS key`, as the raw per-group reply list.
    pub async fn xinfo_groups(&self, key: impl ToArg) -> Result<Vec<Value>> {
        self.call_ro(Cmd::new("XINFO").arg("GROUPS").arg(key)).await
    }

    /// `XINFO CONSUMERS key group`, as the raw per-consumer reply list.
    pub async fn xinfo_consumers(&self, key: impl ToArg, group: impl ToArg) -> Result<Vec<Value>> {
        self.call_ro(Cmd::new("XINFO").arg("CONSUMERS").arg(key).arg(group)).await
    }
}

/// `XREAD`/`XREADGROUP` reply parsing: the server may send this as an
/// array of `[stream, entries]` pairs (RESP2) or a native RESP3 map of
/// `stream -> entries`, so decode through [`Value::into_pairs`] rather
/// than assuming either shape.
fn parse_xread_reply(value: Value) -> Result<Vec<StreamMessages>> {
    value
        .into_pairs()?
        .into_iter()
        .map(|(stream, entries)| {
            Ok(StreamMessages {
                stream: String::from_value(stream)?,
                entries: Vec::<StreamEntry>::from_value(entries)?,
            })
        })
        .collect()
}

fn xread_cmd<I, K, Id>(name: &str, group_consumer: Option<(&str, &str)>, count: Option<i64>, block_ms: Option<i64>, noack: bool, streams: I) -> Cmd
where
    I: IntoIterator<Item = (K, Id)>,
    K: ToArg,
    Id: ToArg,
{
    let mut cmd = Cmd::new(name);
    if let Some((group, consumer)) = group_consumer {
        cmd = cmd.arg("GROUP").arg(group).arg(consumer);
    }
    cmd = cmd.kw_opt("COUNT", count);
    cmd = cmd.kw_opt("BLOCK", block_ms);
    cmd = cmd.arg_if(noack, "NOACK");

    let mut keys = Vec::new();
    let mut ids = Vec::new();
    for (k, id) in streams {
        keys.push(k.to_arg());
        ids.push(id.to_arg());
    }
    cmd.arg("STREAMS").args(keys).args(ids)
}
