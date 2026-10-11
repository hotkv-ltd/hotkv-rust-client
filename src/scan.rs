// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Cursor based iteration: `SCAN`, `HSCAN`, `SSCAN`, `ZSCAN`.

use std::collections::VecDeque;

use crate::args::Cmd;
use crate::client::Client;
use crate::error::{Error, Result};
use crate::value::{FromValue, Value};

/// A cursor based iterator over the results of `SCAN`, `HSCAN`, `SSCAN` or
/// `ZSCAN`. Build one with [`Client::scan`], [`Client::hscan`],
/// [`Client::sscan`] or [`Client::zscan`], configure it with
/// [`Scan::match_pattern`]/[`Scan::count`]/[`Scan::type_filter`], then pull
/// items with [`Scan::next`] (or drain it with [`Scan::collect_all`]).
/// Each call to `next` may issue a new `SCAN`-family round trip once the
/// current batch is exhausted.
pub struct Scan<T: FromValue> {
    client: Client,
    cmd_name: &'static str,
    key: Option<String>,
    match_pattern: Option<String>,
    count: Option<i64>,
    type_filter: Option<String>,
    cursor: String,
    started: bool,
    pairs: bool,
    buffer: VecDeque<T>,
}

impl<T: FromValue> Scan<T> {
    pub(crate) fn new(client: Client, cmd_name: &'static str, key: Option<String>, pairs: bool) -> Self {
        Scan {
            client,
            cmd_name,
            key,
            match_pattern: None,
            count: None,
            type_filter: None,
            cursor: "0".to_string(),
            started: false,
            pairs,
            buffer: VecDeque::new(),
        }
    }

    /// `MATCH pattern`: only return keys/fields/members matching a glob
    /// style pattern.
    pub fn match_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.match_pattern = Some(pattern.into());
        self
    }

    /// `COUNT n`: a hint for how many entries to examine per round trip.
    pub fn count(mut self, n: i64) -> Self {
        self.count = Some(n);
        self
    }

    /// `TYPE name`: only meaningful for the top level `SCAN` command.
    pub fn type_filter(mut self, type_name: impl Into<String>) -> Self {
        self.type_filter = Some(type_name.into());
        self
    }

    fn build_cmd(&self) -> Cmd {
        let mut cmd = Cmd::new(self.cmd_name);
        if let Some(k) = &self.key {
            cmd = cmd.arg(k);
        }
        cmd = cmd.arg(&self.cursor);
        cmd = cmd.kw_opt("MATCH", self.match_pattern.clone());
        cmd = cmd.kw_opt("COUNT", self.count);
        cmd = cmd.kw_opt("TYPE", self.type_filter.clone());
        cmd
    }

    async fn fill(&mut self) -> Result<()> {
        while self.buffer.is_empty() {
            if self.started && self.cursor == "0" {
                return Ok(()); // exhausted
            }
            let reply = self.client.command_idempotent(self.build_cmd()).await?;
            let (cursor, data) = parse_scan_reply(reply)?;
            self.cursor = cursor;
            self.started = true;

            let batch: Vec<T> = if self.pairs {
                data.into_pairs()?
                    .into_iter()
                    .map(|(k, v)| T::from_value(Value::Array(vec![k, v])))
                    .collect::<Result<_>>()?
            } else {
                data.into_items()?.into_iter().map(T::from_value).collect::<Result<_>>()?
            };
            self.buffer.extend(batch);
        }
        Ok(())
    }

    /// Fetches and returns the next item, or `None` once every key/field
    /// has been visited once.
    pub async fn next(&mut self) -> Option<Result<T>> {
        if let Err(e) = self.fill().await {
            return Some(Err(e));
        }
        self.buffer.pop_front().map(Ok)
    }

    /// Drains the rest of the iteration into a `Vec`. Convenient for small
    /// keyspaces; for large ones prefer [`Scan::next`] in a loop.
    pub async fn collect_all(mut self) -> Result<Vec<T>> {
        let mut out = Vec::new();
        while let Some(item) = self.next().await {
            out.push(item?);
        }
        Ok(out)
    }
}

fn parse_scan_reply(v: Value) -> Result<(String, Value)> {
    let mut items = v.into_items()?;
    if items.len() != 2 {
        return Err(Error::Protocol(format!(
            "expected a 2 element SCAN reply, got {} elements",
            items.len()
        )));
    }
    let data = items.pop().unwrap();
    let cursor = String::from_value(items.pop().unwrap())?;
    Ok((cursor, data))
}

impl Client {
    /// `SCAN cursor ...`: iterates every key in the keyspace.
    pub fn scan(&self) -> Scan<String> {
        Scan::new(self.clone(), "SCAN", None, false)
    }

    /// `HSCAN key cursor ...`: iterates a hash's field/value pairs.
    pub fn hscan(&self, key: impl Into<String>) -> Scan<(String, String)> {
        Scan::new(self.clone(), "HSCAN", Some(key.into()), true)
    }

    /// `SSCAN key cursor ...`: iterates a set's members.
    pub fn sscan(&self, key: impl Into<String>) -> Scan<String> {
        Scan::new(self.clone(), "SSCAN", Some(key.into()), false)
    }

    /// `ZSCAN key cursor ...`: iterates a sorted set's member/score pairs.
    pub fn zscan(&self, key: impl Into<String>) -> Scan<(String, f64)> {
        Scan::new(self.clone(), "ZSCAN", Some(key.into()), true)
    }
}
