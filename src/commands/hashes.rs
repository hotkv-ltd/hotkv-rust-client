// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Hash commands, including the per-field TTL family
//! (`HEXPIRE`/`HTTL`/...).

use std::collections::HashMap;

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::commands::keys::ExpireCondition;
use crate::error::Result;

impl Client {
    /// `HSET key field value [field value ...]` -> number of new fields.
    pub async fn hset<I, F, V>(&self, key: impl ToArg, fields: I) -> Result<i64>
    where
        I: IntoIterator<Item = (F, V)>,
        F: ToArg,
        V: ToArg,
    {
        let mut cmd = Cmd::new("HSET").arg(key);
        for (f, v) in fields {
            cmd = cmd.arg(f).arg(v);
        }
        self.call(cmd).await
    }

    /// `HGET key field`.
    pub async fn hget(&self, key: impl ToArg, field: impl ToArg) -> Result<Option<String>> {
        self.call_ro(Cmd::new("HGET").arg(key).arg(field)).await
    }

    /// `HDEL key field [field ...]` -> number of fields removed.
    pub async fn hdel<I, F>(&self, key: impl ToArg, fields: I) -> Result<i64>
    where
        I: IntoIterator<Item = F>,
        F: ToArg,
    {
        self.call(Cmd::new("HDEL").arg(key).args(fields)).await
    }

    /// `HGETALL key`. Accepts a RESP3 map, an array of pairs, or a flat
    /// alternating array reply, whichever the server sends.
    pub async fn hgetall(&self, key: impl ToArg) -> Result<HashMap<String, String>> {
        self.call_ro(Cmd::new("HGETALL").arg(key)).await
    }

    /// `HMGET key field [field ...]`.
    pub async fn hmget<I, F>(&self, key: impl ToArg, fields: I) -> Result<Vec<Option<String>>>
    where
        I: IntoIterator<Item = F>,
        F: ToArg,
    {
        self.call_ro(Cmd::new("HMGET").arg(key).args(fields)).await
    }

    /// `HMSET key field value [field value ...]` (legacy alias of `HSET`).
    pub async fn hmset<I, F, V>(&self, key: impl ToArg, fields: I) -> Result<()>
    where
        I: IntoIterator<Item = (F, V)>,
        F: ToArg,
        V: ToArg,
    {
        let mut cmd = Cmd::new("HMSET").arg(key);
        for (f, v) in fields {
            cmd = cmd.arg(f).arg(v);
        }
        self.call(cmd).await
    }

    /// `HEXISTS key field`.
    pub async fn hexists(&self, key: impl ToArg, field: impl ToArg) -> Result<bool> {
        self.call_ro(Cmd::new("HEXISTS").arg(key).arg(field)).await
    }

    /// `HLEN key`.
    pub async fn hlen(&self, key: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("HLEN").arg(key)).await
    }

    /// `HKEYS key`.
    pub async fn hkeys(&self, key: impl ToArg) -> Result<Vec<String>> {
        self.call_ro(Cmd::new("HKEYS").arg(key)).await
    }

    /// `HVALS key`.
    pub async fn hvals(&self, key: impl ToArg) -> Result<Vec<String>> {
        self.call_ro(Cmd::new("HVALS").arg(key)).await
    }

    /// `HINCRBY key field delta`.
    pub async fn hincrby(&self, key: impl ToArg, field: impl ToArg, delta: i64) -> Result<i64> {
        self.call(Cmd::new("HINCRBY").arg(key).arg(field).arg(delta)).await
    }

    /// `HINCRBYFLOAT key field delta`.
    pub async fn hincrbyfloat(&self, key: impl ToArg, field: impl ToArg, delta: f64) -> Result<f64> {
        self.call(Cmd::new("HINCRBYFLOAT").arg(key).arg(field).arg(delta)).await
    }

    /// `HSETNX key field value`.
    pub async fn hsetnx(&self, key: impl ToArg, field: impl ToArg, value: impl ToArg) -> Result<bool> {
        self.call(Cmd::new("HSETNX").arg(key).arg(field).arg(value)).await
    }

    /// `HRANDFIELD key`.
    pub async fn hrandfield(&self, key: impl ToArg) -> Result<Option<String>> {
        self.call_ro(Cmd::new("HRANDFIELD").arg(key)).await
    }

    /// `HRANDFIELD key count`. A negative count allows repeats.
    pub async fn hrandfield_count(&self, key: impl ToArg, count: i64) -> Result<Vec<String>> {
        self.call_ro(Cmd::new("HRANDFIELD").arg(key).arg(count)).await
    }

    /// `HRANDFIELD key count WITHVALUES`.
    pub async fn hrandfield_with_values(&self, key: impl ToArg, count: i64) -> Result<Vec<(String, String)>> {
        let cmd = Cmd::new("HRANDFIELD").arg(key).arg(count).arg("WITHVALUES");
        crate::value::pairs_vec(self.command_idempotent(cmd).await?)
    }

    /// `HEXPIRE key seconds [condition] FIELDS numfields field ...` ->
    /// one status per field (`1` set, `0` condition not met, `-2` no such
    /// field).
    pub async fn hexpire(
        &self,
        key: impl ToArg,
        seconds: i64,
        condition: Option<ExpireCondition>,
        fields: &[&str],
    ) -> Result<Vec<i64>> {
        self.call(hash_ttl_cmd("HEXPIRE", key, seconds, condition, fields)).await
    }

    /// `HPEXPIRE key milliseconds [condition] FIELDS numfields field ...`.
    pub async fn hpexpire(
        &self,
        key: impl ToArg,
        millis: i64,
        condition: Option<ExpireCondition>,
        fields: &[&str],
    ) -> Result<Vec<i64>> {
        self.call(hash_ttl_cmd("HPEXPIRE", key, millis, condition, fields)).await
    }

    /// `HEXPIREAT key unix-seconds [condition] FIELDS numfields field ...`.
    pub async fn hexpireat(
        &self,
        key: impl ToArg,
        unix_seconds: i64,
        condition: Option<ExpireCondition>,
        fields: &[&str],
    ) -> Result<Vec<i64>> {
        self.call(hash_ttl_cmd("HEXPIREAT", key, unix_seconds, condition, fields)).await
    }

    /// `HPEXPIREAT key unix-millis [condition] FIELDS numfields field ...`.
    pub async fn hpexpireat(
        &self,
        key: impl ToArg,
        unix_millis: i64,
        condition: Option<ExpireCondition>,
        fields: &[&str],
    ) -> Result<Vec<i64>> {
        self.call(hash_ttl_cmd("HPEXPIREAT", key, unix_millis, condition, fields)).await
    }

    /// `HPERSIST key FIELDS numfields field ...` -> one status per field
    /// (`1` removed, `-1` had no TTL, `-2` no such field).
    pub async fn hpersist(&self, key: impl ToArg, fields: &[&str]) -> Result<Vec<i64>> {
        self.call(hash_fields_cmd("HPERSIST", key, fields)).await
    }

    /// `HTTL key FIELDS numfields field ...` -> remaining TTL in seconds
    /// per field.
    pub async fn httl(&self, key: impl ToArg, fields: &[&str]) -> Result<Vec<i64>> {
        self.call_ro(hash_fields_cmd("HTTL", key, fields)).await
    }

    /// `HPTTL key FIELDS numfields field ...` -> remaining TTL in
    /// milliseconds per field.
    pub async fn hpttl(&self, key: impl ToArg, fields: &[&str]) -> Result<Vec<i64>> {
        self.call_ro(hash_fields_cmd("HPTTL", key, fields)).await
    }

    /// `HEXPIRETIME key FIELDS numfields field ...` -> absolute expiry as
    /// a Unix timestamp in seconds, per field.
    pub async fn hexpiretime(&self, key: impl ToArg, fields: &[&str]) -> Result<Vec<i64>> {
        self.call_ro(hash_fields_cmd("HEXPIRETIME", key, fields)).await
    }

    /// `HPEXPIRETIME key FIELDS numfields field ...`, in milliseconds.
    pub async fn hpexpiretime(&self, key: impl ToArg, fields: &[&str]) -> Result<Vec<i64>> {
        self.call_ro(hash_fields_cmd("HPEXPIRETIME", key, fields)).await
    }
}

fn hash_ttl_cmd(name: &str, key: impl ToArg, ttl: i64, condition: Option<ExpireCondition>, fields: &[&str]) -> Cmd {
    let mut cmd = Cmd::new(name).arg(key).arg(ttl);
    if let Some(c) = condition {
        cmd = cmd.arg(match c {
            ExpireCondition::Nx => "NX",
            ExpireCondition::Xx => "XX",
            ExpireCondition::Gt => "GT",
            ExpireCondition::Lt => "LT",
        });
    }
    cmd.arg("FIELDS").arg(fields.len() as i64).args(fields.iter().copied())
}

fn hash_fields_cmd(name: &str, key: impl ToArg, fields: &[&str]) -> Cmd {
    Cmd::new(name)
        .arg(key)
        .arg("FIELDS")
        .arg(fields.len() as i64)
        .args(fields.iter().copied())
}
