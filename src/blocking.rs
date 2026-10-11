// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! A synchronous wrapper around [`crate::Client`], for callers who are not
//! already inside a Tokio runtime. Each [`BlockingClient`] owns a small,
//! current thread Tokio runtime and blocks the calling thread for the
//! duration of every call. Not safe to call concurrently from more than
//! one OS thread at a time; create one `BlockingClient` per thread (or
//! guard a shared one with a mutex).

use std::future::Future;

use tokio::runtime::{Builder, Runtime};

use crate::args::{Cmd, ToArg};
use crate::commands::{
    AlertsCommands, FeatureCommands, GcraCommands, ObserveCommands, RateLimitCommands, TagCommands, TsCommands, VsimCommands,
};
use crate::{ClientOptions, Error, PoolStats, Result, Value};

fn new_runtime() -> Result<Runtime> {
    Builder::new_current_thread().enable_all().build().map_err(Error::from)
}

/// The blocking counterpart of [`crate::Client`]. It wraps an async
/// `Client` one for one; [`BlockingClient::async_client`] and
/// [`BlockingClient::block_on`] give access to any typed async method
/// (including the HotKV and AI command group accessors) without leaving
/// blocking code, e.g. `client.block_on(client.async_client().scache().get(...))`.
pub struct BlockingClient {
    client: crate::Client,
    rt: Runtime,
}

impl BlockingClient {
    /// Connects using a `hotkv://`/`hotkvs://`/`redis://`/`rediss://` URL.
    pub fn connect(url: &str) -> Result<Self> {
        let rt = new_runtime()?;
        let client = rt.block_on(crate::Client::connect(url))?;
        Ok(BlockingClient { client, rt })
    }

    /// Creates a client from an explicit [`ClientOptions`].
    pub fn with_options(opts: ClientOptions) -> Result<Self> {
        let rt = new_runtime()?;
        let client = rt.block_on(crate::Client::with_options(opts))?;
        Ok(BlockingClient { client, rt })
    }

    /// The underlying async client, for use with [`BlockingClient::block_on`].
    pub fn async_client(&self) -> &crate::Client {
        &self.client
    }

    /// Blocks the current thread until `fut` completes. Use this to reach
    /// any async [`crate::Client`] method this wrapper does not have a
    /// dedicated method for.
    pub fn block_on<F: Future>(&self, fut: F) -> F::Output {
        self.rt.block_on(fut)
    }

    pub fn options(&self) -> &ClientOptions {
        self.client.options()
    }

    pub fn pool_stats(&self) -> PoolStats {
        self.client.pool_stats()
    }

    pub fn close(&self) {
        self.client.close()
    }

    /// The `TAG.*` command group.
    pub fn tag(&self) -> TagCommands {
        self.client.tag()
    }
    /// The `GCRA.*` command group.
    pub fn gcra(&self) -> GcraCommands {
        self.client.gcra()
    }
    /// The `VSIM.*` command group.
    pub fn vsim(&self) -> VsimCommands {
        self.client.vsim()
    }
    /// The `RATELIMIT.*` command group.
    pub fn ratelimit(&self) -> RateLimitCommands {
        self.client.ratelimit()
    }
    /// The `TS.*` command group.
    pub fn ts(&self) -> TsCommands {
        self.client.ts()
    }
    /// The `FEATURE.*` command group.
    pub fn feature(&self) -> FeatureCommands {
        self.client.feature()
    }
    /// The `OBSERVE` command group.
    pub fn observe(&self) -> ObserveCommands {
        self.client.observe()
    }
    /// The `ALERTS` command group.
    pub fn alerts(&self) -> AlertsCommands {
        self.client.alerts()
    }

    /// The generic `command(...)` escape hatch.
    pub fn command(&self, cmd: Cmd) -> Result<Value> {
        self.rt.block_on(self.client.command(cmd))
    }

    /// The generic `execute(name, args)` escape hatch.
    pub fn execute<I, A>(&self, name: &str, args: I) -> Result<Value>
    where
        I: IntoIterator<Item = A>,
        A: ToArg,
    {
        self.rt.block_on(self.client.execute(name, args))
    }

    /// `GET key`.
    pub fn get(&self, key: impl ToArg) -> Result<Option<String>> {
        self.rt.block_on(self.client.get(key))
    }

    /// `SET key value`.
    pub fn set(&self, key: impl ToArg, value: impl ToArg) -> Result<()> {
        self.rt.block_on(self.client.set(key, value))
    }

    /// `DEL key [key ...]`.
    pub fn del<I, K>(&self, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.rt.block_on(self.client.del(keys))
    }

    /// `EXISTS key [key ...]`.
    pub fn exists<I, K>(&self, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.rt.block_on(self.client.exists(keys))
    }

    /// `EXPIRE key seconds`.
    pub fn expire(&self, key: impl ToArg, seconds: i64) -> Result<bool> {
        self.rt.block_on(self.client.expire(key, seconds, None))
    }

    /// `TTL key`.
    pub fn ttl(&self, key: impl ToArg) -> Result<i64> {
        self.rt.block_on(self.client.ttl(key))
    }

    /// `INCR key`.
    pub fn incr(&self, key: impl ToArg) -> Result<i64> {
        self.rt.block_on(self.client.incr(key))
    }

    /// `PING`.
    pub fn ping(&self) -> Result<String> {
        self.rt.block_on(self.client.ping())
    }

    /// `HGET key field`.
    pub fn hget(&self, key: impl ToArg, field: impl ToArg) -> Result<Option<String>> {
        self.rt.block_on(self.client.hget(key, field))
    }

    /// `HSET key field value [field value ...]`.
    pub fn hset<I, F, V>(&self, key: impl ToArg, fields: I) -> Result<i64>
    where
        I: IntoIterator<Item = (F, V)>,
        F: ToArg,
        V: ToArg,
    {
        self.rt.block_on(self.client.hset(key, fields))
    }

    /// `LPUSH key value [value ...]`.
    pub fn lpush<I, V>(&self, key: impl ToArg, values: I) -> Result<i64>
    where
        I: IntoIterator<Item = V>,
        V: ToArg,
    {
        self.rt.block_on(self.client.lpush(key, values))
    }

    /// `RPUSH key value [value ...]`.
    pub fn rpush<I, V>(&self, key: impl ToArg, values: I) -> Result<i64>
    where
        I: IntoIterator<Item = V>,
        V: ToArg,
    {
        self.rt.block_on(self.client.rpush(key, values))
    }

    /// `SADD key member [member ...]`.
    pub fn sadd<I, M>(&self, key: impl ToArg, members: I) -> Result<i64>
    where
        I: IntoIterator<Item = M>,
        M: ToArg,
    {
        self.rt.block_on(self.client.sadd(key, members))
    }

    /// `ZADD key score member [score member ...]`.
    pub fn zadd<I, M>(&self, key: impl ToArg, members: I) -> Result<i64>
    where
        I: IntoIterator<Item = (f64, M)>,
        M: ToArg,
    {
        self.rt.block_on(self.client.zadd(key, members))
    }
}
