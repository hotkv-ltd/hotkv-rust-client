// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! A bounded connection pool with idle reuse and a health check on borrow.

use std::collections::VecDeque;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::args::Cmd;
use crate::conn::Connection;
use crate::error::{Error, Result};
use crate::options::ClientOptions;

/// A pooled connection is PING checked before being handed out once it has
/// sat idle for at least this long.
const HEALTH_CHECK_IDLE: Duration = Duration::from_secs(2);

/// A snapshot of pool usage, returned by [`crate::Client::pool_stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
    pub idle: usize,
    pub in_use: usize,
    pub max_size: usize,
}

/// The pool holds at most `pool_size` connections. A permit from the
/// semaphore is held for as long as a connection is borrowed (or being
/// dialed) and is released as soon as it is returned, so a task waiting for
/// a free connection wakes up the moment one comes back. Idle connections
/// hold no permit: a connection is only dialed when none is idle, and then
/// every other connection is borrowed, so the total never exceeds the size.
pub(crate) struct Pool {
    opts: ClientOptions,
    idle: Mutex<VecDeque<Connection>>,
    semaphore: Arc<Semaphore>,
    closed: AtomicBool,
}

impl Pool {
    pub(crate) fn new(opts: ClientOptions) -> Arc<Self> {
        let size = opts.pool_size.max(1);
        Arc::new(Pool {
            opts,
            idle: Mutex::new(VecDeque::new()),
            semaphore: Arc::new(Semaphore::new(size)),
            closed: AtomicBool::new(false),
        })
    }

    /// Borrows a connection: an idle, still fresh one when available
    /// (health checked with `PING` if it has been idle for a while),
    /// otherwise a newly dialed one, blocking only while the pool is at
    /// capacity.
    pub(crate) async fn get(self: &Arc<Self>) -> Result<PooledConnection> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }

        // Wait for a free slot first: at most `pool_size` connections are
        // borrowed or being dialed at any moment.
        let permit = tokio::time::timeout(self.opts.connect_timeout, Arc::clone(&self.semaphore).acquire_owned())
            .await
            .map_err(|_| Error::Pool("timed out waiting for a free connection".into()))?
            .map_err(|_| Error::Closed)?;

        loop {
            let popped = self.idle.lock().unwrap().pop_back();
            let Some(mut conn) = popped else { break };

            if conn.idle_for() > self.opts.idle_timeout {
                continue; // too old: drop it, try the next idle one
            }
            if conn.idle_for() >= HEALTH_CHECK_IDLE && conn.command(&Cmd::new("PING")).await.is_err() {
                continue; // stale connection, drop it and try the next
            }
            return Ok(PooledConnection {
                pool: Arc::clone(self),
                conn: Some(conn),
                permit,
                tainted: false,
            });
        }

        let conn = Connection::connect(&self.opts).await?;
        Ok(PooledConnection {
            pool: Arc::clone(self),
            conn: Some(conn),
            permit,
            tainted: false,
        })
    }

    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.semaphore.close();
        self.idle.lock().unwrap().clear();
    }

    pub(crate) fn stats(&self) -> PoolStats {
        let idle = self.idle.lock().unwrap().len();
        let available = self.semaphore.available_permits();
        PoolStats {
            idle,
            in_use: self.opts.pool_size.max(1).saturating_sub(available),
            max_size: self.opts.pool_size,
        }
    }
}

/// An owned, borrowed connection. Returned to the pool's idle list when
/// dropped, unless [`PooledConnection::discard`] was called first (used
/// after an I/O error, since the connection is no longer trustworthy).
pub(crate) struct PooledConnection {
    pool: Arc<Pool>,
    conn: Option<Connection>,
    // Held while the connection is borrowed; released after the connection
    // is back in the idle list (fields drop after `Drop::drop` has run).
    #[allow(dead_code)]
    permit: OwnedSemaphorePermit,
    /// Once true, this connection is never returned to the shared pool
    /// again (even if a future holding it, e.g. a Pub/Sub read, is
    /// dropped before completing). Used for connections handed off to
    /// Pub/Sub, which leave server side subscription state behind that
    /// would corrupt an unrelated later borrower.
    tainted: bool,
}

impl PooledConnection {
    pub(crate) fn discard(mut self) {
        self.conn = None;
    }

    pub(crate) fn mark_tainted(&mut self) {
        self.tainted = true;
    }
}

impl Deref for PooledConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("connection taken")
    }
}

impl DerefMut for PooledConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        self.conn.as_mut().expect("connection taken")
    }
}

impl Drop for PooledConnection {
    fn drop(&mut self) {
        if self.tainted {
            return; // the connection drops for real below: the socket closes, the permit is freed
        }
        if let Some(conn) = self.conn.take() {
            if !self.pool.closed.load(Ordering::Acquire) {
                self.pool.idle.lock().unwrap().push_back(conn);
            }
        }
    }
}
