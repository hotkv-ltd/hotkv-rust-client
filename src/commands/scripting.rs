// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Lua scripting: `EVAL`, `EVALSHA`, `SCRIPT`.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::value::Value;

fn eval_cmd<I, K, A>(name: &str, script_or_sha: impl ToArg, keys: I, args: A) -> Cmd
where
    I: IntoIterator<Item = K>,
    K: ToArg,
    A: IntoIterator,
    A::Item: ToArg,
{
    let keys: Vec<K> = keys.into_iter().collect();
    Cmd::new(name)
        .arg(script_or_sha)
        .arg(keys.len() as i64)
        .args(keys)
        .args(args)
}

impl Client {
    /// `EVAL script numkeys key [key ...] arg [arg ...]`.
    pub async fn eval<I, K, A>(&self, script: &str, keys: I, args: A) -> Result<Value>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
        A: IntoIterator,
        A::Item: ToArg,
    {
        self.command(eval_cmd("EVAL", script, keys, args)).await
    }

    /// `EVALSHA sha1 numkeys key [key ...] arg [arg ...]`.
    pub async fn evalsha<I, K, A>(&self, sha1: &str, keys: I, args: A) -> Result<Value>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
        A: IntoIterator,
        A::Item: ToArg,
    {
        self.command(eval_cmd("EVALSHA", sha1, keys, args)).await
    }

    /// `EVALSHA`, transparently falling back to `EVAL script` (which also
    /// primes the script cache for next time) when the server replies
    /// `NOSCRIPT`.
    pub async fn eval_sha_or_load<I, K, A>(&self, sha1: &str, script: &str, keys: I, args: A) -> Result<Value>
    where
        I: IntoIterator<Item = K> + Clone,
        K: ToArg,
        A: IntoIterator + Clone,
        A::Item: ToArg,
    {
        match self.evalsha(sha1, keys.clone(), args.clone()).await {
            Err(Error::Server(e)) if e.is_code("NOSCRIPT") => self.eval(script, keys, args).await,
            other => other,
        }
    }

    /// `SCRIPT LOAD script` -> its SHA1 digest.
    pub async fn script_load(&self, script: &str) -> Result<String> {
        self.call(Cmd::new("SCRIPT").arg("LOAD").arg(script)).await
    }

    /// `SCRIPT EXISTS sha1 [sha1 ...]`.
    pub async fn script_exists<I, S>(&self, sha1s: I) -> Result<Vec<bool>>
    where
        I: IntoIterator<Item = S>,
        S: ToArg,
    {
        self.call_ro(Cmd::new("SCRIPT").arg("EXISTS").args(sha1s)).await
    }

    /// `SCRIPT FLUSH`.
    pub async fn script_flush(&self) -> Result<()> {
        self.call(Cmd::new("SCRIPT").arg("FLUSH")).await
    }
}
