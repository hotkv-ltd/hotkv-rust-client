// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Server and connection commands: `PING`, `INFO`, `CONFIG`, `CLIENT`, ...

use std::collections::HashMap;

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::Result;

impl Client {
    /// `PING`.
    pub async fn ping(&self) -> Result<String> {
        self.call_ro(Cmd::new("PING")).await
    }

    /// `PING message` -> the same message, echoed back.
    pub async fn ping_message(&self, message: impl ToArg) -> Result<String> {
        self.call_ro(Cmd::new("PING").arg(message)).await
    }

    /// `ECHO message`.
    pub async fn echo(&self, message: impl ToArg) -> Result<String> {
        self.call_ro(Cmd::new("ECHO").arg(message)).await
    }

    /// `INFO [section]`.
    pub async fn info(&self, section: Option<&str>) -> Result<String> {
        let mut cmd = Cmd::new("INFO");
        if let Some(s) = section {
            cmd = cmd.arg(s);
        }
        self.call_ro(cmd).await
    }

    /// `DBSIZE`.
    pub async fn dbsize(&self) -> Result<i64> {
        self.call_ro(Cmd::new("DBSIZE")).await
    }

    /// `FLUSHDB [ASYNC|SYNC]`.
    pub async fn flushdb(&self, asynchronous: bool) -> Result<()> {
        let mut cmd = Cmd::new("FLUSHDB");
        if asynchronous {
            cmd = cmd.arg("ASYNC");
        }
        self.call(cmd).await
    }

    /// `TIME` -> `(unix seconds, microseconds)`.
    pub async fn time(&self) -> Result<(i64, i64)> {
        self.call_ro(Cmd::new("TIME")).await
    }

    /// `CONFIG GET parameter`. `parameter` may be a glob pattern; the
    /// server may then reply with several parameters.
    pub async fn config_get(&self, parameter: impl ToArg) -> Result<HashMap<String, String>> {
        self.call_ro(Cmd::new("CONFIG").arg("GET").arg(parameter)).await
    }

    /// `CONFIG SET parameter value`.
    pub async fn config_set(&self, parameter: impl ToArg, value: impl ToArg) -> Result<()> {
        self.call(Cmd::new("CONFIG").arg("SET").arg(parameter).arg(value)).await
    }

    /// `CLIENT SETNAME name`.
    pub async fn client_setname(&self, name: impl ToArg) -> Result<()> {
        self.call(Cmd::new("CLIENT").arg("SETNAME").arg(name)).await
    }

    /// `CLIENT GETNAME`.
    pub async fn client_getname(&self) -> Result<Option<String>> {
        self.call_ro(Cmd::new("CLIENT").arg("GETNAME")).await
    }

    /// `CLIENT ID`.
    pub async fn client_id(&self) -> Result<i64> {
        self.call_ro(Cmd::new("CLIENT").arg("ID")).await
    }

    /// `SELECT index`: changes the logical database for connections
    /// dialed after this call returns. Prefer setting
    /// [`crate::ClientOptions::db`] before connecting; because the
    /// connection pool may serve any command from any pooled connection,
    /// calling this mid-session affects only the one connection this
    /// particular call happens to run on.
    pub async fn select(&self, index: i64) -> Result<()> {
        self.call(Cmd::new("SELECT").arg(index)).await
    }
}
