// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Bitmap and HyperLogLog commands.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::Result;

/// `AND`/`OR`/`XOR`/`NOT` for [`Client::bitop`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOp {
    And,
    Or,
    Xor,
    Not,
}

impl BitOp {
    fn as_str(self) -> &'static str {
        match self {
            BitOp::And => "AND",
            BitOp::Or => "OR",
            BitOp::Xor => "XOR",
            BitOp::Not => "NOT",
        }
    }
}

impl Client {
    /// `SETBIT key offset value` -> the bit's previous value.
    pub async fn setbit(&self, key: impl ToArg, offset: i64, value: bool) -> Result<i64> {
        self.call(Cmd::new("SETBIT").arg(key).arg(offset).arg(if value { 1 } else { 0 }))
            .await
    }

    /// `GETBIT key offset`.
    pub async fn getbit(&self, key: impl ToArg, offset: i64) -> Result<i64> {
        self.call_ro(Cmd::new("GETBIT").arg(key).arg(offset)).await
    }

    /// `BITCOUNT key [start end [BYTE|BIT]]`.
    pub async fn bitcount(&self, key: impl ToArg, range: Option<(i64, i64, bool)>) -> Result<i64> {
        let mut cmd = Cmd::new("BITCOUNT").arg(key);
        if let Some((start, end, by_bit)) = range {
            cmd = cmd.arg(start).arg(end);
            cmd = cmd.arg_if(by_bit, "BIT");
        }
        self.call_ro(cmd).await
    }

    /// `BITPOS key bit [start [end [BYTE|BIT]]]`.
    pub async fn bitpos(&self, key: impl ToArg, bit: bool, start: Option<i64>, end: Option<i64>, by_bit: bool) -> Result<i64> {
        let mut cmd = Cmd::new("BITPOS").arg(key).arg(if bit { 1 } else { 0 });
        if let Some(s) = start {
            cmd = cmd.arg(s);
            if let Some(e) = end {
                cmd = cmd.arg(e);
                cmd = cmd.arg_if(by_bit, "BIT");
            }
        }
        self.call_ro(cmd).await
    }

    /// `BITOP AND|OR|XOR|NOT destkey key [key ...]` -> the result string's
    /// length.
    pub async fn bitop<I, K>(&self, op: BitOp, destkey: impl ToArg, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call(Cmd::new("BITOP").arg(op.as_str()).arg(destkey).args(keys)).await
    }

    /// `PFADD key element [element ...]` -> whether the register changed.
    pub async fn pfadd<I, E>(&self, key: impl ToArg, elements: I) -> Result<bool>
    where
        I: IntoIterator<Item = E>,
        E: ToArg,
    {
        self.call(Cmd::new("PFADD").arg(key).args(elements)).await
    }

    /// `PFCOUNT key [key ...]` -> the approximate cardinality.
    pub async fn pfcount<I, K>(&self, keys: I) -> Result<i64>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call_ro(Cmd::new("PFCOUNT").args(keys)).await
    }

    /// `PFMERGE destkey sourcekey [sourcekey ...]`.
    pub async fn pfmerge<I, K>(&self, destkey: impl ToArg, sourcekeys: I) -> Result<()>
    where
        I: IntoIterator<Item = K>,
        K: ToArg,
    {
        self.call(Cmd::new("PFMERGE").arg(destkey).args(sourcekeys)).await
    }
}
