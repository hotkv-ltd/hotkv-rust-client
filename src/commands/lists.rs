// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! List commands.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::Result;

impl Client {
    /// `LPUSH key value [value ...]` -> the list's new length.
    pub async fn lpush<I, V>(&self, key: impl ToArg, values: I) -> Result<i64>
    where
        I: IntoIterator<Item = V>,
        V: ToArg,
    {
        self.call(Cmd::new("LPUSH").arg(key).args(values)).await
    }

    /// `RPUSH key value [value ...]` -> the list's new length.
    pub async fn rpush<I, V>(&self, key: impl ToArg, values: I) -> Result<i64>
    where
        I: IntoIterator<Item = V>,
        V: ToArg,
    {
        self.call(Cmd::new("RPUSH").arg(key).args(values)).await
    }

    /// `LPUSHX key value [value ...]`: push only if `key` already exists.
    pub async fn lpushx<I, V>(&self, key: impl ToArg, values: I) -> Result<i64>
    where
        I: IntoIterator<Item = V>,
        V: ToArg,
    {
        self.call(Cmd::new("LPUSHX").arg(key).args(values)).await
    }

    /// `RPUSHX key value [value ...]`.
    pub async fn rpushx<I, V>(&self, key: impl ToArg, values: I) -> Result<i64>
    where
        I: IntoIterator<Item = V>,
        V: ToArg,
    {
        self.call(Cmd::new("RPUSHX").arg(key).args(values)).await
    }

    /// `LPOP key`.
    pub async fn lpop(&self, key: impl ToArg) -> Result<Option<String>> {
        self.call(Cmd::new("LPOP").arg(key)).await
    }

    /// `LPOP key count`.
    pub async fn lpop_count(&self, key: impl ToArg, count: i64) -> Result<Option<Vec<String>>> {
        self.call(Cmd::new("LPOP").arg(key).arg(count)).await
    }

    /// `RPOP key`.
    pub async fn rpop(&self, key: impl ToArg) -> Result<Option<String>> {
        self.call(Cmd::new("RPOP").arg(key)).await
    }

    /// `RPOP key count`.
    pub async fn rpop_count(&self, key: impl ToArg, count: i64) -> Result<Option<Vec<String>>> {
        self.call(Cmd::new("RPOP").arg(key).arg(count)).await
    }

    /// `LRANGE key start stop`.
    pub async fn lrange(&self, key: impl ToArg, start: i64, stop: i64) -> Result<Vec<String>> {
        self.call_ro(Cmd::new("LRANGE").arg(key).arg(start).arg(stop)).await
    }

    /// `LLEN key`.
    pub async fn llen(&self, key: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("LLEN").arg(key)).await
    }

    /// `LINDEX key index`.
    pub async fn lindex(&self, key: impl ToArg, index: i64) -> Result<Option<String>> {
        self.call_ro(Cmd::new("LINDEX").arg(key).arg(index)).await
    }

    /// `LSET key index value`.
    pub async fn lset(&self, key: impl ToArg, index: i64, value: impl ToArg) -> Result<()> {
        self.call(Cmd::new("LSET").arg(key).arg(index).arg(value)).await
    }

    /// `LINSERT key BEFORE|AFTER pivot value` -> the list's new length, or
    /// `-1` if `pivot` was not found.
    pub async fn linsert(&self, key: impl ToArg, before: bool, pivot: impl ToArg, value: impl ToArg) -> Result<i64> {
        let where_ = if before { "BEFORE" } else { "AFTER" };
        self.call(Cmd::new("LINSERT").arg(key).arg(where_).arg(pivot).arg(value)).await
    }

    /// `LREM key count value` -> number of elements removed.
    pub async fn lrem(&self, key: impl ToArg, count: i64, value: impl ToArg) -> Result<i64> {
        self.call(Cmd::new("LREM").arg(key).arg(count).arg(value)).await
    }

    /// `LTRIM key start stop`.
    pub async fn ltrim(&self, key: impl ToArg, start: i64, stop: i64) -> Result<()> {
        self.call(Cmd::new("LTRIM").arg(key).arg(start).arg(stop)).await
    }

    /// `RPOPLPUSH source destination`.
    pub async fn rpoplpush(&self, source: impl ToArg, destination: impl ToArg) -> Result<Option<String>> {
        self.call(Cmd::new("RPOPLPUSH").arg(source).arg(destination)).await
    }

    /// `LMOVE source destination LEFT|RIGHT LEFT|RIGHT`.
    pub async fn lmove(
        &self,
        source: impl ToArg,
        destination: impl ToArg,
        from_left: bool,
        to_left: bool,
    ) -> Result<Option<String>> {
        let from = if from_left { "LEFT" } else { "RIGHT" };
        let to = if to_left { "LEFT" } else { "RIGHT" };
        self.call(Cmd::new("LMOVE").arg(source).arg(destination).arg(from).arg(to)).await
    }
}
