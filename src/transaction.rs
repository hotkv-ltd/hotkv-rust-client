// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! `MULTI`/`EXEC`/`DISCARD` transactions and `WATCH`/`UNWATCH`.

use crate::args::Cmd;
use crate::error::{Error, Result};
use crate::pool::PooledConnection;
use crate::value::Value;

/// A `MULTI`/`EXEC` transaction, holding one dedicated connection for its
/// whole lifetime. Build it with [`crate::Client::transaction`]:
///
/// ```no_run
/// # use hotkv::{Client, args::Cmd};
/// # async fn run(client: Client) -> hotkv::Result<()> {
/// let mut tx = client.transaction().await?;
/// tx.watch(["balance"]).await?;
/// tx.multi().await?;
/// tx.queue(Cmd::new("DECRBY").arg("balance").arg(10)).await?;
/// tx.queue(Cmd::new("INCRBY").arg("spent").arg(10)).await?;
/// match tx.exec().await? {
///     Some(_results) => { /* committed */ }
///     None => { /* a watched key changed: aborted */ }
/// }
/// # Ok(())
/// # }
/// ```
pub struct Transaction {
    conn: Option<PooledConnection>,
    /// Set once `WATCH` or `MULTI` has been sent: leaving the connection
    /// in this state unfinished (dropped before `EXEC`/`DISCARD`) is
    /// unsafe to hand back to the shared pool.
    dirty: bool,
    finished: bool,
}

impl Transaction {
    pub(crate) async fn new(conn: PooledConnection) -> Result<Self> {
        Ok(Transaction {
            conn: Some(conn),
            dirty: false,
            finished: false,
        })
    }

    fn conn_mut(&mut self) -> &mut PooledConnection {
        self.conn.as_mut().expect("Transaction used after exec/discard")
    }

    /// `WATCH key [key ...]`: aborts the transaction (a `None` from
    /// [`Transaction::exec`]) if any watched key changes before `EXEC`.
    /// Must be called before [`Transaction::multi`].
    pub async fn watch<I, S>(&mut self, keys: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.dirty = true;
        let cmd = Cmd::new("WATCH").args(keys.into_iter().map(|k| k.as_ref().to_string()));
        let v = self.conn_mut().command(&cmd).await?;
        v.into_result().map(|_| ())
    }

    /// `UNWATCH`: clears all watched keys.
    pub async fn unwatch(&mut self) -> Result<()> {
        let v = self.conn_mut().command(&Cmd::new("UNWATCH")).await?;
        v.into_result().map(|_| ())
    }

    /// Starts the transaction body with `MULTI`.
    pub async fn multi(&mut self) -> Result<()> {
        self.dirty = true;
        let v = self.conn_mut().command(&Cmd::new("MULTI")).await?;
        v.into_result().map(|_| ())
    }

    /// Queues one command inside the transaction body. The server replies
    /// `QUEUED` immediately (or an error immediately, e.g. for a malformed
    /// command); the command itself only runs at [`Transaction::exec`].
    pub async fn queue(&mut self, cmd: Cmd) -> Result<()> {
        let v = self.conn_mut().command(&cmd).await?;
        v.into_result().map(|_| ())
    }

    /// `EXEC`: runs every queued command. Returns `None` if the
    /// transaction was aborted (a watched key changed), otherwise one
    /// `Result` per queued command, in order.
    pub async fn exec(mut self) -> Result<Option<Vec<Result<Value>>>> {
        let v = self.conn_mut().command(&Cmd::new("EXEC")).await?;
        self.finished = true;
        match v {
            Value::Nil => Ok(None),
            Value::Array(items) => Ok(Some(items.into_iter().map(Value::into_result).collect())),
            Value::Error(e) => Err(e.into()),
            other => Err(Error::Protocol(format!("unexpected EXEC reply: {}", other.kind()))),
        }
    }

    /// `DISCARD`: cancels the transaction body without running it.
    pub async fn discard(mut self) -> Result<()> {
        let v = self.conn_mut().command(&Cmd::new("DISCARD")).await?;
        self.finished = true;
        v.into_result().map(|_| ())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            if self.dirty && !self.finished {
                // WATCH/MULTI state was left dangling: never hand this
                // connection back to the shared pool.
                conn.discard();
            }
        }
    }
}
