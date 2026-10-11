// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! String commands: `GET`, `SET` and friends.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::Result;
use crate::value::Value;

/// Optional trailing arguments for [`Client::set_options`]. At most one of
/// `ex`/`px`/`exat`/`pxat`/`keepttl` should be set; `nx` and `xx` are
/// mutually exclusive.
#[derive(Debug, Clone, Default)]
pub struct SetOptions {
    pub nx: bool,
    pub xx: bool,
    /// `GET`: return the key's old value (or nil) instead of a plain OK.
    pub get: bool,
    pub ex: Option<i64>,
    pub px: Option<i64>,
    pub exat: Option<i64>,
    pub pxat: Option<i64>,
    pub keepttl: bool,
}

/// Optional trailing arguments for [`Client::getex`].
#[derive(Debug, Clone, Default)]
pub struct GetExOptions {
    pub ex: Option<i64>,
    pub px: Option<i64>,
    pub exat: Option<i64>,
    pub pxat: Option<i64>,
    pub persist: bool,
}

impl Client {
    /// `GET key`. For binary values that may not be valid UTF-8, use
    /// [`Client::get_bytes`] instead.
    pub async fn get(&self, key: impl ToArg) -> Result<Option<String>> {
        self.call_ro(Cmd::new("GET").arg(key)).await
    }

    /// `GET key`, returned as raw, binary safe bytes.
    pub async fn get_bytes(&self, key: impl ToArg) -> Result<Option<Vec<u8>>> {
        match self.command_idempotent(Cmd::new("GET").arg(key)).await? {
            Value::Nil => Ok(None),
            other => Ok(Some(other.into_bytes()?)),
        }
    }

    /// `SET key value`.
    pub async fn set(&self, key: impl ToArg, value: impl ToArg) -> Result<()> {
        self.call(Cmd::new("SET").arg(key).arg(value)).await
    }

    /// `SET key value NX`: only set if `key` does not exist. Returns
    /// whether it was set.
    pub async fn set_nx(&self, key: impl ToArg, value: impl ToArg) -> Result<bool> {
        let v = self.command(Cmd::new("SET").arg(key).arg(value).arg("NX")).await?;
        Ok(!v.is_nil())
    }

    /// `SET key value XX`: only set if `key` already exists. Returns
    /// whether it was set.
    pub async fn set_xx(&self, key: impl ToArg, value: impl ToArg) -> Result<bool> {
        let v = self.command(Cmd::new("SET").arg(key).arg(value).arg("XX")).await?;
        Ok(!v.is_nil())
    }

    /// `SET key value EX seconds`.
    pub async fn set_ex(&self, key: impl ToArg, value: impl ToArg, seconds: i64) -> Result<()> {
        self.call(Cmd::new("SET").arg(key).arg(value).arg("EX").arg(seconds)).await
    }

    /// `SET key value PX milliseconds`.
    pub async fn set_px(&self, key: impl ToArg, value: impl ToArg, millis: i64) -> Result<()> {
        self.call(Cmd::new("SET").arg(key).arg(value).arg("PX").arg(millis)).await
    }

    /// `SET key value [NX|XX] [GET] [EX s|PX ms|EXAT ts|PXAT ts|KEEPTTL]`,
    /// the full option set, returned as the raw reply: `Value::Nil` if a
    /// `NX`/`XX` condition was not met (and `GET` was not requested), the
    /// previous value if `GET` was requested, or `Value::SimpleString`
    /// (`OK`) otherwise.
    pub async fn set_options(&self, key: impl ToArg, value: impl ToArg, opts: &SetOptions) -> Result<Value> {
        let mut cmd = Cmd::new("SET").arg(key).arg(value);
        cmd = cmd.arg_if(opts.nx, "NX");
        cmd = cmd.arg_if(opts.xx, "XX");
        cmd = cmd.arg_if(opts.get, "GET");
        cmd = cmd.kw_opt("EX", opts.ex);
        cmd = cmd.kw_opt("PX", opts.px);
        cmd = cmd.kw_opt("EXAT", opts.exat);
        cmd = cmd.kw_opt("PXAT", opts.pxat);
        cmd = cmd.arg_if(opts.keepttl, "KEEPTTL");
        self.command(cmd).await
    }

    /// `SETNX key value`.
    pub async fn setnx(&self, key: impl ToArg, value: impl ToArg) -> Result<bool> {
        self.call(Cmd::new("SETNX").arg(key).arg(value)).await
    }

    /// `SETEX key seconds value`.
    pub async fn setex(&self, key: impl ToArg, seconds: i64, value: impl ToArg) -> Result<()> {
        self.call(Cmd::new("SETEX").arg(key).arg(seconds).arg(value)).await
    }

    /// `PSETEX key milliseconds value`.
    pub async fn psetex(&self, key: impl ToArg, millis: i64, value: impl ToArg) -> Result<()> {
        self.call(Cmd::new("PSETEX").arg(key).arg(millis).arg(value)).await
    }

    /// `GETSET key value`: set a new value, return the old one.
    pub async fn getset(&self, key: impl ToArg, value: impl ToArg) -> Result<Option<String>> {
        self.call(Cmd::new("GETSET").arg(key).arg(value)).await
    }

    /// `GETDEL key`: return the value and delete the key in one step.
    pub async fn getdel(&self, key: impl ToArg) -> Result<Option<String>> {
        self.call(Cmd::new("GETDEL").arg(key)).await
    }

    /// `GETEX key [EX s|PX ms|EXAT ts|PXAT ts|PERSIST]`: read a value and
    /// optionally change its TTL in the same round trip.
    pub async fn getex(&self, key: impl ToArg, opts: &GetExOptions) -> Result<Option<String>> {
        let mut cmd = Cmd::new("GETEX").arg(key);
        cmd = cmd.kw_opt("EX", opts.ex);
        cmd = cmd.kw_opt("PX", opts.px);
        cmd = cmd.kw_opt("EXAT", opts.exat);
        cmd = cmd.kw_opt("PXAT", opts.pxat);
        cmd = cmd.arg_if(opts.persist, "PERSIST");
        self.call(cmd).await
    }

    /// `APPEND key value` -> the string's new length.
    pub async fn append(&self, key: impl ToArg, value: impl ToArg) -> Result<i64> {
        self.call(Cmd::new("APPEND").arg(key).arg(value)).await
    }

    /// `STRLEN key`.
    pub async fn strlen(&self, key: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("STRLEN").arg(key)).await
    }

    /// `INCR key`.
    pub async fn incr(&self, key: impl ToArg) -> Result<i64> {
        self.call(Cmd::new("INCR").arg(key)).await
    }

    /// `DECR key`.
    pub async fn decr(&self, key: impl ToArg) -> Result<i64> {
        self.call(Cmd::new("DECR").arg(key)).await
    }

    /// `INCRBY key delta`.
    pub async fn incrby(&self, key: impl ToArg, delta: i64) -> Result<i64> {
        self.call(Cmd::new("INCRBY").arg(key).arg(delta)).await
    }

    /// `DECRBY key delta`.
    pub async fn decrby(&self, key: impl ToArg, delta: i64) -> Result<i64> {
        self.call(Cmd::new("DECRBY").arg(key).arg(delta)).await
    }

    /// `INCRBYFLOAT key delta`.
    pub async fn incrbyfloat(&self, key: impl ToArg, delta: f64) -> Result<f64> {
        self.call(Cmd::new("INCRBYFLOAT").arg(key).arg(delta)).await
    }

    /// `MGET key [key ...]`, one `Option<String>` per key, in order.
    pub async fn mget<I, K>(&self, keys: I) -> Result<Vec<Option<String>>>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call_ro(Cmd::new("MGET").args(keys)).await
    }

    /// `MSET key value [key value ...]`.
    pub async fn mset<I, K, V>(&self, pairs: I) -> Result<()>
    where
        I: IntoIterator<Item = (K, V)>,
        K: ToArg,
        V: ToArg,
    {
        let mut cmd = Cmd::new("MSET");
        for (k, v) in pairs {
            cmd = cmd.arg(k).arg(v);
        }
        self.call(cmd).await
    }

    /// `MSETNX key value [key value ...]`: sets nothing unless every key
    /// is new. Returns whether the keys were set.
    pub async fn msetnx<I, K, V>(&self, pairs: I) -> Result<bool>
    where
        I: IntoIterator<Item = (K, V)>,
        K: ToArg,
        V: ToArg,
    {
        let mut cmd = Cmd::new("MSETNX");
        for (k, v) in pairs {
            cmd = cmd.arg(k).arg(v);
        }
        self.call(cmd).await
    }

    /// `SETRANGE key offset value` -> the string's new length.
    pub async fn setrange(&self, key: impl ToArg, offset: i64, value: impl ToArg) -> Result<i64> {
        self.call(Cmd::new("SETRANGE").arg(key).arg(offset).arg(value)).await
    }

    /// `GETRANGE key start end`.
    pub async fn getrange(&self, key: impl ToArg, start: i64, end: i64) -> Result<String> {
        self.call_ro(Cmd::new("GETRANGE").arg(key).arg(start).arg(end)).await
    }
}
