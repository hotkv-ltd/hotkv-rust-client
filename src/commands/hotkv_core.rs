// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Core HotKV extensions that need no enterprise license: `TAG.*`,
//! `NEXPIRE`/`NTTL`/`NEXPIREAT`, `HOTKEYS`, `GCRA.*` and `VSIM.*`.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::value::{field_i64, field_string, named_fields, pairs_vec, FromValue, Value};

// ---------------------------------------------------------------- TAG.* --

/// `TAG.*` commands: tag keys and look them up by tag. Reach it with
/// [`Client::tag`].
pub struct TagCommands(pub(crate) Client);

impl TagCommands {
    /// `TAG.SET key tag [tag ...]` -> number of tags newly added.
    pub async fn set<I, T>(&self, key: impl ToArg, tags: I) -> Result<i64>
    where
        I: IntoIterator<Item = T>,
        T: ToArg,
    {
        self.0.call(Cmd::new("TAG.SET").arg(key).args(tags)).await
    }

    /// `TAG.REM key tag [tag ...]` -> number of tags removed.
    pub async fn rem<I, T>(&self, key: impl ToArg, tags: I) -> Result<i64>
    where
        I: IntoIterator<Item = T>,
        T: ToArg,
    {
        self.0.call(Cmd::new("TAG.REM").arg(key).args(tags)).await
    }

    /// `TAG.GET key` -> every tag on `key`.
    pub async fn get(&self, key: impl ToArg) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("TAG.GET").arg(key)).await
    }

    /// `TAG.MEMBERS tag` -> every key carrying `tag`.
    pub async fn members(&self, tag: impl ToArg) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("TAG.MEMBERS").arg(tag)).await
    }

    /// `TAG.INTER tag [tag ...]` -> keys carrying every given tag.
    pub async fn inter<I, T>(&self, tags: I) -> Result<Vec<String>>
    where
        I: IntoIterator<Item = T>,
        T: ToArg,
    {
        self.0.call_ro(Cmd::new("TAG.INTER").args(tags)).await
    }

    /// `TAG.COUNT key`.
    pub async fn count(&self, key: impl ToArg) -> Result<i64> {
        self.0.call_ro(Cmd::new("TAG.COUNT").arg(key)).await
    }

    /// `TAG.EXISTS key tag`.
    pub async fn exists(&self, key: impl ToArg, tag: impl ToArg) -> Result<bool> {
        self.0.call_ro(Cmd::new("TAG.EXISTS").arg(key).arg(tag)).await
    }
}

// -------------------------------------------- NEXPIRE / NTTL / HOTKEYS --

impl Client {
    /// The `TAG.*` command group.
    pub fn tag(&self) -> TagCommands {
        TagCommands(self.clone())
    }

    /// `NEXPIRE key nanoseconds` (a relative TTL) -> whether it was set.
    pub async fn nexpire(&self, key: impl ToArg, nanoseconds: i64) -> Result<bool> {
        self.call(Cmd::new("NEXPIRE").arg(key).arg(nanoseconds)).await
    }

    /// `NTTL key` -> remaining TTL in nanoseconds (`-2` missing, `-1` no
    /// expiry).
    pub async fn nttl(&self, key: impl ToArg) -> Result<i64> {
        self.call_ro(Cmd::new("NTTL").arg(key)).await
    }

    /// `NEXPIREAT key unix-nanoseconds` (an absolute TTL).
    pub async fn nexpireat(&self, key: impl ToArg, unix_nanoseconds: i64) -> Result<bool> {
        self.call(Cmd::new("NEXPIREAT").arg(key).arg(unix_nanoseconds)).await
    }

    /// `HOTKEYS TOP [count]` -> `(key, access count)`, most accessed
    /// first.
    pub async fn hotkeys_top(&self, count: Option<i64>) -> Result<Vec<(String, i64)>> {
        let mut cmd = Cmd::new("HOTKEYS").arg("TOP");
        if let Some(c) = count {
            cmd = cmd.arg(c);
        }
        self.call_ro(cmd).await
    }

    /// `HOTKEYS RESET`.
    pub async fn hotkeys_reset(&self) -> Result<()> {
        self.call(Cmd::new("HOTKEYS").arg("RESET")).await
    }

    /// The `GCRA.*` rate limiter primitive command group.
    pub fn gcra(&self) -> GcraCommands {
        GcraCommands(self.clone())
    }

    /// The `VSIM.*` vector similarity index command group.
    pub fn vsim(&self) -> VsimCommands {
        VsimCommands(self.clone())
    }
}

// ------------------------------------------------------------- GCRA.* --

/// One `GCRA.LIMIT` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcraDecision {
    pub denied: bool,
    pub limit: i64,
    pub remaining: i64,
    /// Milliseconds until the request could succeed; `-1` when allowed.
    pub retry_after_ms: i64,
    pub reset_after_ms: i64,
}

impl FromValue for GcraDecision {
    fn from_value(value: Value) -> Result<Self> {
        let items = value.into_items()?;
        if items.len() != 5 {
            return Err(Error::Protocol(format!("expected 5 elements in GCRA.LIMIT reply, got {}", items.len())));
        }
        let mut it = items.into_iter();
        let mut next = || i64::from_value(it.next().unwrap());
        Ok(GcraDecision {
            denied: next()? != 0,
            limit: next()?,
            remaining: next()?,
            retry_after_ms: next()?,
            reset_after_ms: next()?,
        })
    }
}

/// `GCRA.*` commands. Reach it with [`Client::gcra`].
pub struct GcraCommands(pub(crate) Client);

impl GcraCommands {
    /// `GCRA.LIMIT key period_ms burst [count]`.
    pub async fn limit(&self, key: impl ToArg, period_ms: i64, burst: i64, count: Option<i64>) -> Result<GcraDecision> {
        let cmd = Cmd::new("GCRA.LIMIT").arg(key).arg(period_ms).arg(burst).arg_opt(count);
        self.0.call(cmd).await
    }

    /// `GCRA.RESET key [key ...]` -> number of keys that had state.
    pub async fn reset<I, K>(&self, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.0.call(Cmd::new("GCRA.RESET").args(keys)).await
    }
}

// ------------------------------------------------------------- VSIM.* --

/// `VSIM.INFO index` fields.
#[derive(Debug, Clone, PartialEq)]
pub struct VsimIndexInfo {
    pub name: String,
    pub dimensions: i64,
    pub metric: String,
    pub vectors: i64,
    pub m: i64,
    pub ef_construction: i64,
    pub ef_search: i64,
    /// Tombstones not yet unlinked from the graph.
    pub deleted_vectors: i64,
    /// Unlinked slots waiting for reuse.
    pub free_slots: i64,
    /// Live vectors, tombstones and free slots together.
    pub total_slots: i64,
    /// The index's estimated memory footprint.
    pub memory_bytes: i64,
}

impl FromValue for VsimIndexInfo {
    /// Parsed by field name, so fields added by newer servers are ignored and
    /// fields an older server does not send read as zero.
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(VsimIndexInfo {
            name: field_string(&m, "name"),
            dimensions: field_i64(&m, "dimensions"),
            metric: field_string(&m, "metric"),
            vectors: field_i64(&m, "vectors"),
            m: field_i64(&m, "M"),
            ef_construction: field_i64(&m, "ef_construction"),
            ef_search: field_i64(&m, "ef_search"),
            deleted_vectors: field_i64(&m, "deleted_vectors"),
            free_slots: field_i64(&m, "free_slots"),
            total_slots: field_i64(&m, "total_slots"),
            memory_bytes: field_i64(&m, "memory_bytes"),
        })
    }
}

/// `VSIM.*` commands. Reach it with [`Client::vsim`].
pub struct VsimCommands(pub(crate) Client);

impl VsimCommands {
    /// `VSIM.CREATEINDEX name dims [METRIC m] [M n] [EF_CONSTRUCTION n]
    /// [EF_SEARCH n]`. `metric` is one of `"cosine"`, `"l2"`, `"ip"`.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_index(
        &self,
        name: impl ToArg,
        dims: i64,
        metric: Option<&str>,
        m: Option<i64>,
        ef_construction: Option<i64>,
        ef_search: Option<i64>,
    ) -> Result<()> {
        let cmd = Cmd::new("VSIM.CREATEINDEX")
            .arg(name)
            .arg(dims)
            .kw_opt("METRIC", metric)
            .kw_opt("M", m)
            .kw_opt("EF_CONSTRUCTION", ef_construction)
            .kw_opt("EF_SEARCH", ef_search);
        self.0.call(cmd).await
    }

    /// `VSIM.DROPINDEX name`.
    pub async fn drop_index(&self, name: impl ToArg) -> Result<()> {
        self.0.call(Cmd::new("VSIM.DROPINDEX").arg(name)).await
    }

    /// `VSIM.SET index key val1 val2 ... valN` -> whether `key` was newly
    /// inserted (`false` if it replaced an existing vector).
    pub async fn set(&self, index: impl ToArg, key: impl ToArg, vector: &[f64]) -> Result<bool> {
        let cmd = Cmd::new("VSIM.SET").arg(index).arg(key).args_f32(vector);
        self.0.call(cmd).await
    }

    /// `VSIM.GET index key`.
    pub async fn get(&self, index: impl ToArg, key: impl ToArg) -> Result<Option<Vec<f64>>> {
        self.0.call_ro(Cmd::new("VSIM.GET").arg(index).arg(key)).await
    }

    /// `VSIM.DEL index key`.
    pub async fn del(&self, index: impl ToArg, key: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("VSIM.DEL").arg(index).arg(key)).await
    }

    /// `VSIM.SEARCH index k val1 ... valN [FILTER tag ...] [EF ef]` ->
    /// `(key, distance)`, nearest first.
    pub async fn search(&self, index: impl ToArg, k: i64, vector: &[f64], filter_tags: &[&str], ef: Option<i64>) -> Result<Vec<(String, f64)>> {
        let mut cmd = Cmd::new("VSIM.SEARCH").arg(index).arg(k).args_f32(vector);
        if !filter_tags.is_empty() {
            cmd = cmd.arg("FILTER").args(filter_tags.iter().copied());
        }
        cmd = cmd.kw_opt("EF", ef);
        pairs_vec(self.0.command_idempotent(cmd).await?)
    }

    /// `VSIM.INFO` (no index) -> every index name.
    pub async fn list_indexes(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("VSIM.INFO")).await
    }

    /// `VSIM.INFO index`.
    pub async fn info(&self, index: impl ToArg) -> Result<VsimIndexInfo> {
        self.0.call_ro(Cmd::new("VSIM.INFO").arg(index)).await
    }

    /// `VSIM.SAVE snapshot_name blob` -> bytes written. Needs the
    /// `vsim-persist` license feature.
    pub async fn save(&self, snapshot_name: impl ToArg, blob: &[u8]) -> Result<i64> {
        self.0.call(Cmd::new("VSIM.SAVE").arg(snapshot_name).arg(blob)).await
    }

    /// `VSIM.LOAD snapshot_name`. Needs the `vsim-persist` license
    /// feature.
    pub async fn load(&self, snapshot_name: impl ToArg) -> Result<Option<Vec<u8>>> {
        match self.0.command(Cmd::new("VSIM.LOAD").arg(snapshot_name)).await? {
            Value::Nil => Ok(None),
            other => Ok(Some(other.into_bytes()?)),
        }
    }

    /// `VSIM.LIST`. Needs the `vsim-persist` license feature.
    pub async fn list_snapshots(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("VSIM.LIST")).await
    }

    /// `VSIM.DROP snapshot_name`. Needs the `vsim-persist` license
    /// feature.
    pub async fn drop_snapshot(&self, snapshot_name: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("VSIM.DROP").arg(snapshot_name)).await
    }
}
