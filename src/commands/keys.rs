// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Generic key commands: existence, expiry, renaming, copying,
//! serialization.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::Result;
use crate::value::Value;

/// A conditional flag for `EXPIRE`/`PEXPIRE`/`EXPIREAT`/`PEXPIREAT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpireCondition {
    /// Only set the expiry if `key` has none.
    Nx,
    /// Only set the expiry if `key` already has one.
    Xx,
    /// Only set the expiry if it is later than the current one.
    Gt,
    /// Only set the expiry if it is earlier than the current one.
    Lt,
}

impl ExpireCondition {
    fn as_str(self) -> &'static str {
        match self {
            ExpireCondition::Nx => "NX",
            ExpireCondition::Xx => "XX",
            ExpireCondition::Gt => "GT",
            ExpireCondition::Lt => "LT",
        }
    }
}

impl Client {
    /// `DEL key [key ...]` -> number of keys removed.
    pub async fn del<I, K>(&self, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call(Cmd::new("DEL").args(keys)).await
    }

    /// `UNLINK key [key ...]`: like `DEL`, reclaiming memory in the
    /// background.
    pub async fn unlink<I, K>(&self, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call(Cmd::new("UNLINK").args(keys)).await
    }

    /// `EXISTS key [key ...]` -> count of keys that exist (a repeated key
    /// counts once per occurrence).
    pub async fn exists<I, K>(&self, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call_ro(Cmd::new("EXISTS").args(keys)).await
    }

    /// `EXPIRE key seconds [condition]` -> whether the TTL was set.
    pub async fn expire(&self, key: impl ToArg, seconds: i64, condition: Option<ExpireCondition>) -> Result<bool> {
        let mut cmd = Cmd::new("EXPIRE").arg(key).arg(seconds);
        if let Some(c) = condition {
            cmd = cmd.arg(c.as_str());
        }
        self.call(cmd).await
    }

    /// `PEXPIRE key milliseconds [condition]`.
    pub async fn pexpire(&self, key: impl ToArg, millis: i64, condition: Option<ExpireCondition>) -> Result<bool> {
        let mut cmd = Cmd::new("PEXPIRE").arg(key).arg(millis);
        if let Some(c) = condition {
            cmd = cmd.arg(c.as_str());
        }
        self.call(cmd).await
    }

    /// `EXPIREAT key unix-seconds [condition]`.
    pub async fn expireat(&self, key: impl ToArg, unix_seconds: i64, condition: Option<ExpireCondition>) -> Result<bool> {
        let mut cmd = Cmd::new("EXPIREAT").arg(key).arg(unix_seconds);
        if let Some(c) = condition {
            cmd = cmd.arg(c.as_str());
        }
        self.call(cmd).await
    }

    /// `PEXPIREAT key unix-millis [condition]`.
    pub async fn pexpireat(&self, key: impl ToArg, unix_millis: i64, condition: Option<ExpireCondition>) -> Result<bool> {
        let mut cmd = Cmd::new("PEXPIREAT").arg(key).arg(unix_millis);
        if let Some(c) = condition {
            cmd = cmd.arg(c.as_str());
        }
        self.call(cmd).await
    }

    /// `TTL key` -> seconds remaining, `-1` if no expiry, `-2` if missing.
    pub async fn ttl(&self, key: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("TTL").arg(key)).await
    }

    /// `PTTL key` -> milliseconds remaining, `-1`/`-2` as with `TTL`.
    pub async fn pttl(&self, key: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("PTTL").arg(key)).await
    }

    /// `PERSIST key` -> whether a TTL was removed.
    pub async fn persist(&self, key: impl ToArg) -> Result<bool> {
        self.call(Cmd::new("PERSIST").arg(key)).await
    }

    /// `TYPE key`.
    pub async fn key_type(&self, key: impl ToArg) -> Result<String> {
        self.call_ro(Cmd::new("TYPE").arg(key)).await
    }

    /// `RENAME key newkey`.
    pub async fn rename(&self, key: impl ToArg, newkey: impl ToArg) -> Result<()> {
        self.call(Cmd::new("RENAME").arg(key).arg(newkey)).await
    }

    /// `RENAMENX key newkey` -> whether the rename happened.
    pub async fn renamenx(&self, key: impl ToArg, newkey: impl ToArg) -> Result<bool> {
        self.call(Cmd::new("RENAMENX").arg(key).arg(newkey)).await
    }

    /// `KEYS pattern`. Scans the whole keyspace in one round trip; prefer
    /// [`Client::scan`] outside of small/test databases.
    pub async fn keys(&self, pattern: impl ToArg) -> Result<Vec<String>> {
        self.call_ro(Cmd::new("KEYS").arg(pattern)).await
    }

    /// `RANDOMKEY`.
    pub async fn randomkey(&self) -> Result<Option<String>> {
        self.call_ro(Cmd::new("RANDOMKEY")).await
    }

    /// `DUMP key`: a serialized representation, or `None` if `key` is
    /// missing.
    pub async fn dump(&self, key: impl ToArg) -> Result<Option<Vec<u8>>> {
        match self.command_idempotent(Cmd::new("DUMP").arg(key)).await? {
            Value::Nil => Ok(None),
            other => Ok(Some(other.into_bytes()?)),
        }
    }

    /// `RESTORE key ttl serialized [REPLACE]`. `ttl` is milliseconds, `0`
    /// for no expiry.
    pub async fn restore(&self, key: impl ToArg, ttl_millis: i64, serialized: &[u8], replace: bool) -> Result<()> {
        let mut cmd = Cmd::new("RESTORE").arg(key).arg(ttl_millis).arg(serialized);
        cmd = cmd.arg_if(replace, "REPLACE");
        self.call(cmd).await
    }

    /// `COPY source destination [DB index] [REPLACE]` -> whether the copy
    /// happened.
    pub async fn copy(&self, source: impl ToArg, destination: impl ToArg, db: Option<i64>, replace: bool) -> Result<bool> {
        let mut cmd = Cmd::new("COPY").arg(source).arg(destination);
        cmd = cmd.kw_opt("DB", db);
        cmd = cmd.arg_if(replace, "REPLACE");
        self.call(cmd).await
    }

    /// `TOUCH key [key ...]` -> number of keys that exist.
    pub async fn touch<I, K>(&self, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call_ro(Cmd::new("TOUCH").args(keys)).await
    }

    /// `OBJECT ENCODING key`.
    pub async fn object_encoding(&self, key: impl ToArg) -> Result<Option<String>> {
        self.call_ro(Cmd::new("OBJECT").arg("ENCODING").arg(key)).await
    }
}
