// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Set commands.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::Result;

impl Client {
    /// `SADD key member [member ...]` -> number of members added.
    pub async fn sadd<I, M>(&self, key: impl ToArg, members: I) -> Result<i64>
    where
        I: IntoIterator<Item = M>,
        M: ToArg,
    {
        self.call(Cmd::new("SADD").arg(key).args(members)).await
    }

    /// `SREM key member [member ...]` -> number of members removed.
    pub async fn srem<I, M>(&self, key: impl ToArg, members: I) -> Result<i64>
    where
        I: IntoIterator<Item = M>,
        M: ToArg,
    {
        self.call(Cmd::new("SREM").arg(key).args(members)).await
    }

    /// `SMEMBERS key`.
    pub async fn smembers(&self, key: impl ToArg) -> Result<Vec<String>> {
        self.call_ro(Cmd::new("SMEMBERS").arg(key)).await
    }

    /// `SISMEMBER key member`.
    pub async fn sismember(&self, key: impl ToArg, member: impl ToArg) -> Result<bool> {
        self.call_ro(Cmd::new("SISMEMBER").arg(key).arg(member)).await
    }

    /// `SMISMEMBER key member [member ...]`.
    pub async fn smismember<I, M>(&self, key: impl ToArg, members: I) -> Result<Vec<bool>>
    where
        I: IntoIterator<Item = M>,
        M: ToArg,
    {
        self.call_ro(Cmd::new("SMISMEMBER").arg(key).args(members)).await
    }

    /// `SCARD key`.
    pub async fn scard(&self, key: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("SCARD").arg(key)).await
    }

    /// `SPOP key`.
    pub async fn spop(&self, key: impl ToArg) -> Result<Option<String>> {
        self.call(Cmd::new("SPOP").arg(key)).await
    }

    /// `SPOP key count`.
    pub async fn spop_count(&self, key: impl ToArg, count: i64) -> Result<Vec<String>> {
        self.call(Cmd::new("SPOP").arg(key).arg(count)).await
    }

    /// `SRANDMEMBER key`.
    pub async fn srandmember(&self, key: impl ToArg) -> Result<Option<String>> {
        self.call_ro(Cmd::new("SRANDMEMBER").arg(key)).await
    }

    /// `SRANDMEMBER key count`. A negative count allows repeats.
    pub async fn srandmember_count(&self, key: impl ToArg, count: i64) -> Result<Vec<String>> {
        self.call_ro(Cmd::new("SRANDMEMBER").arg(key).arg(count)).await
    }

    /// `SUNION key [key ...]`.
    pub async fn sunion<I, K>(&self, keys: I) -> Result<Vec<String>>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call_ro(Cmd::new("SUNION").args(keys)).await
    }

    /// `SINTER key [key ...]`.
    pub async fn sinter<I, K>(&self, keys: I) -> Result<Vec<String>>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call_ro(Cmd::new("SINTER").args(keys)).await
    }

    /// `SDIFF key [key ...]`.
    pub async fn sdiff<I, K>(&self, keys: I) -> Result<Vec<String>>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call_ro(Cmd::new("SDIFF").args(keys)).await
    }

    /// `SUNIONSTORE destination key [key ...]` -> the result set's size.
    pub async fn sunionstore<I, K>(&self, destination: impl ToArg, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call(Cmd::new("SUNIONSTORE").arg(destination).args(keys)).await
    }

    /// `SINTERSTORE destination key [key ...]`.
    pub async fn sinterstore<I, K>(&self, destination: impl ToArg, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call(Cmd::new("SINTERSTORE").arg(destination).args(keys)).await
    }

    /// `SDIFFSTORE destination key [key ...]`.
    pub async fn sdiffstore<I, K>(&self, destination: impl ToArg, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call(Cmd::new("SDIFFSTORE").arg(destination).args(keys)).await
    }

    /// `SMOVE source destination member` -> whether it was moved.
    pub async fn smove(&self, source: impl ToArg, destination: impl ToArg, member: impl ToArg) -> Result<bool> {
        self.call(Cmd::new("SMOVE").arg(source).arg(destination).arg(member)).await
    }
}
