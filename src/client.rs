// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! The async [`Client`]: a cloneable handle over a pooled set of
//! connections to one HotKV server.

use std::sync::Arc;

use crate::args::{Cmd, ToArg};
use crate::error::{Error, Result};
use crate::options::ClientOptions;
use crate::pipeline::Pipeline;
use crate::pool::{Pool, PoolStats, PooledConnection};
use crate::pubsub::PubSub;
use crate::transaction::Transaction;
use crate::value::{FromValue, Value};

struct Inner {
    opts: ClientOptions,
    pool: Arc<Pool>,
}

/// The main HotKV client: an async, cloneable handle backed by a bounded
/// connection pool. Cloning a `Client` is cheap (an `Arc` clone) and every
/// clone shares the same pool, so a single `Client` can be shared across
/// tasks freely.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl Client {
    /// Connects using a `hotkv://`, `hotkvs://`, `redis://` or `rediss://`
    /// URL. The connection pool is created immediately but individual
    /// TCP/TLS connections are dialed lazily, on first use.
    pub async fn connect(url: &str) -> Result<Self> {
        Self::with_options(ClientOptions::from_url(url)?).await
    }

    /// Creates a client from an explicit [`ClientOptions`].
    pub async fn with_options(opts: ClientOptions) -> Result<Self> {
        let pool = Pool::new(opts.clone());
        Ok(Client {
            inner: Arc::new(Inner { opts, pool }),
        })
    }

    /// The options this client was built with.
    pub fn options(&self) -> &ClientOptions {
        &self.inner.opts
    }

    /// A snapshot of connection pool usage.
    pub fn pool_stats(&self) -> PoolStats {
        self.inner.pool.stats()
    }

    /// Borrows a connection and returns the RESP protocol version it
    /// negotiated: `3` if `HELLO 3` succeeded, `2` if the server does not
    /// understand `HELLO` and this crate fell back to legacy RESP2
    /// `AUTH`/`SELECT`. Different pooled connections could in principle
    /// disagree if the server's capabilities changed mid-session; in
    /// practice every connection to the same server negotiates the same
    /// version.
    pub async fn server_protocol_version(&self) -> Result<u8> {
        Ok(self.borrow_conn().await?.protocol())
    }

    /// Closes every pooled connection. The client remains usable
    /// afterwards; new connections are dialed on demand as usual.
    pub fn close(&self) {
        self.inner.pool.close();
    }

    pub(crate) async fn borrow_conn(&self) -> Result<PooledConnection> {
        self.inner.pool.get().await
    }

    /// Sends a single, pre-built command and returns its raw reply. This
    /// never retries: it is meant for writes and for callers who want full
    /// control. See [`Client::command_idempotent`] for automatic retry of
    /// read-only commands after a connection failure.
    pub async fn command(&self, cmd: Cmd) -> Result<Value> {
        self.run(&cmd, false).await
    }

    /// Like [`Client::command`], but on a connection level failure (I/O
    /// error, timeout, dropped connection) it reconnects and retries the
    /// command, up to [`ClientOptions::max_retries`] times. Only use this
    /// for commands that are safe to run more than once (reads, or writes
    /// you know are idempotent); the typed read-only command wrappers in
    /// this crate already do so internally.
    pub async fn command_idempotent(&self, cmd: Cmd) -> Result<Value> {
        self.run(&cmd, true).await
    }

    /// Builds and sends a command from a name and a list of arguments, the
    /// generic escape hatch for anything this crate does not wrap.
    pub async fn execute<I, A>(&self, name: &str, args: I) -> Result<Value>
    where
        I: IntoIterator<Item = A>,
        A: ToArg,
    {
        self.command(Cmd::new(name).args(args)).await
    }

    async fn run(&self, cmd: &Cmd, idempotent: bool) -> Result<Value> {
        // An argument the client refused (NaN, negative USD, ...) fails here,
        // before a connection is borrowed, and never reaches the wire.
        cmd.check()?;
        let attempts = if idempotent { self.inner.opts.max_retries + 1 } else { 1 };
        let mut last_err = Error::Closed;
        for attempt in 0..attempts {
            let mut conn = match self.borrow_conn().await {
                Ok(c) => c,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            match conn.command(cmd).await {
                Ok(v) => return v.into_result(),
                Err(e) => {
                    conn.discard();
                    if idempotent && e.is_retryable() && attempt + 1 < attempts {
                        last_err = e;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(last_err)
    }

    pub(crate) async fn call<T: FromValue>(&self, cmd: Cmd) -> Result<T> {
        T::from_value(self.command(cmd).await?)
    }

    pub(crate) async fn call_ro<T: FromValue>(&self, cmd: Cmd) -> Result<T> {
        T::from_value(self.command_idempotent(cmd).await?)
    }

    /// Starts a new, empty pipeline of commands to run in one round trip.
    pub fn pipeline(&self) -> Pipeline {
        Pipeline::new(self.clone())
    }

    /// Borrows a dedicated connection and starts a `MULTI`/`EXEC`
    /// transaction on it (optionally preceded by `WATCH`).
    pub async fn transaction(&self) -> Result<Transaction> {
        Transaction::new(self.borrow_conn().await?).await
    }

    /// Opens a dedicated Pub/Sub connection with no subscriptions yet; use
    /// [`PubSub::subscribe`]/[`PubSub::psubscribe`]/[`PubSub::ssubscribe`]
    /// to add channels.
    pub async fn pubsub(&self) -> Result<PubSub> {
        PubSub::open(self).await
    }

    /// Opens a Pub/Sub connection and subscribes to `channels`.
    pub async fn subscribe<I, S>(&self, channels: I) -> Result<PubSub>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut ps = self.pubsub().await?;
        ps.subscribe(channels).await?;
        Ok(ps)
    }

    /// Opens a Pub/Sub connection and subscribes to `patterns` with
    /// `PSUBSCRIBE`.
    pub async fn psubscribe<I, S>(&self, patterns: I) -> Result<PubSub>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut ps = self.pubsub().await?;
        ps.psubscribe(patterns).await?;
        Ok(ps)
    }

    /// Opens a Pub/Sub connection and subscribes to `channels` with the
    /// cluster-aware `SSUBSCRIBE`.
    pub async fn ssubscribe<I, S>(&self, channels: I) -> Result<PubSub>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut ps = self.pubsub().await?;
        ps.ssubscribe(channels).await?;
        Ok(ps)
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("host", &self.inner.opts.host)
            .field("port", &self.inner.opts.port)
            .finish()
    }
}
