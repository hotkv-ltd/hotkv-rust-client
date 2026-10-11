// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Pipelining: batch several commands into one network round trip.

use crate::args::Cmd;
use crate::client::Client;
use crate::error::Result;
use crate::value::Value;

/// A batch of commands sent together in one round trip. Build it with
/// [`Client::pipeline`], queue commands with [`Pipeline::queue`], then call
/// [`Pipeline::execute`]. Replies come back in the order the commands were
/// queued; one command failing does not stop the others from running, so
/// each reply is its own `Result`.
pub struct Pipeline {
    client: Client,
    cmds: Vec<Cmd>,
}

impl Pipeline {
    pub(crate) fn new(client: Client) -> Self {
        Pipeline { client, cmds: Vec::new() }
    }

    /// Queues one command. Returns `&mut Self` so calls can be chained.
    pub fn queue(&mut self, cmd: Cmd) -> &mut Self {
        self.cmds.push(cmd);
        self
    }

    /// Number of commands queued so far.
    pub fn len(&self) -> usize {
        self.cmds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    /// Sends every queued command in one write and reads all replies back,
    /// in order. An empty pipeline returns an empty Vec without touching
    /// the network. The outer `Result` only reports a connection level
    /// failure (the whole batch could not complete); a per-command server
    /// error is reported inline as `Err` in that command's slot.
    pub async fn execute(&self) -> Result<Vec<Result<Value>>> {
        if self.cmds.is_empty() {
            return Ok(Vec::new());
        }
        // A refused argument (NaN, ...) fails the whole batch before any
        // connection is borrowed and before any command is sent.
        for cmd in &self.cmds {
            cmd.check()?;
        }
        let mut conn = self.client.borrow_conn().await?;
        match conn.pipeline(&self.cmds).await {
            Ok(values) => Ok(values.into_iter().map(Value::into_result).collect()),
            Err(e) => {
                conn.discard();
                Err(e)
            }
        }
    }
}
