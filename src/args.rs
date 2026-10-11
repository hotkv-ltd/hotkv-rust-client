// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Command argument building: the [`ToArg`] conversion trait and the
//! [`Cmd`] builder used both internally by every typed command wrapper and
//! publicly through [`crate::Client::command`].

use crate::error::{Error, Result};
use crate::numfmt;

/// Converts a Rust value into one RESP command argument (a single bulk
/// string). Implemented for common owned and borrowed scalar types; binary
/// data is passed through unchanged.
pub trait ToArg {
    fn to_arg(&self) -> Vec<u8>;

    /// Like [`ToArg::to_arg`], but may refuse a value that no server parser
    /// accepts (a NaN or infinite float). [`Cmd::arg`] and the other builder
    /// methods use this one and remember the error, so the command fails
    /// with [`crate::Error::InvalidArgument`] before anything is sent.
    fn try_to_arg(&self) -> Result<Vec<u8>> {
        Ok(self.to_arg())
    }
}

impl ToArg for str {
    fn to_arg(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl ToArg for String {
    fn to_arg(&self) -> Vec<u8> {
        self.as_bytes().to_vec()
    }
}

impl ToArg for [u8] {
    fn to_arg(&self) -> Vec<u8> {
        self.to_vec()
    }
}

impl ToArg for Vec<u8> {
    fn to_arg(&self) -> Vec<u8> {
        self.clone()
    }
}

impl<T: ToArg + ?Sized> ToArg for &T {
    fn to_arg(&self) -> Vec<u8> {
        (**self).to_arg()
    }

    fn try_to_arg(&self) -> Result<Vec<u8>> {
        (**self).try_to_arg()
    }
}

macro_rules! to_arg_display {
    ($($t:ty),+ $(,)?) => {
        $(impl ToArg for $t {
            fn to_arg(&self) -> Vec<u8> {
                self.to_string().into_bytes()
            }
        })+
    };
}
to_arg_display!(i64, i32, u64, u32, usize, isize, u16, i16, u8);

/// A finite `f64` is sent as a plain decimal (never an exponent, `-0.0` as
/// `0`); NaN and infinities are refused with [`Error::InvalidArgument`]. Use
/// [`Score`] where `inf`/`-inf` are valid (sorted set scores and bounds).
impl ToArg for f64 {
    fn to_arg(&self) -> Vec<u8> {
        match numfmt::finite(*self) {
            Ok(s) => s.into_bytes(),
            // Only reachable through `to_arg` directly; the builder methods
            // use `try_to_arg` and fail the command instead.
            Err(_) => self.to_string().into_bytes(),
        }
    }

    fn try_to_arg(&self) -> Result<Vec<u8>> {
        numfmt::finite(*self).map(String::into_bytes)
    }
}

/// An `f32` is sent with the shortest digits that parse back to the same
/// `f32`, as a plain decimal. NaN and infinities are refused.
impl ToArg for f32 {
    fn to_arg(&self) -> Vec<u8> {
        if *self == 0.0 {
            b"0".to_vec()
        } else {
            self.to_string().into_bytes()
        }
    }

    fn try_to_arg(&self) -> Result<Vec<u8>> {
        if !self.is_finite() {
            return Err(Error::InvalidArgument(format!("number: {} is not allowed", self)));
        }
        Ok(self.to_arg())
    }
}

/// A sorted set score, weight or score range bound: a finite number as a
/// plain decimal, or `inf` / `-inf`. NaN is refused.
///
/// ```
/// use hotkv::args::{Cmd, Score};
/// let cmd = Cmd::new("ZCOUNT").arg("k").arg(Score(f64::NEG_INFINITY)).arg(Score(1.5));
/// assert_eq!(cmd.len(), 4);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score(pub f64);

impl ToArg for Score {
    fn to_arg(&self) -> Vec<u8> {
        match numfmt::score(self.0) {
            Ok(s) => s.into_bytes(),
            Err(_) => self.0.to_string().into_bytes(),
        }
    }

    fn try_to_arg(&self) -> Result<Vec<u8>> {
        numfmt::score(self.0).map(String::into_bytes)
    }
}

impl ToArg for bool {
    fn to_arg(&self) -> Vec<u8> {
        if *self {
            b"1".to_vec()
        } else {
            b"0".to_vec()
        }
    }
}

/// A command being built up: the command name plus its arguments, each
/// stored as raw bytes so binary safe values (keys, blobs, vectors encoded
/// as bytes) pass through untouched.
#[derive(Debug, Clone, Default)]
pub struct Cmd {
    pub(crate) parts: Vec<Vec<u8>>,
    /// The first argument that could not be encoded (a NaN float, a
    /// negative USD amount, ...). A command carrying one is never sent.
    pub(crate) invalid: Option<String>,
}

impl Cmd {
    /// Starts a new command with the given name, e.g. `Cmd::new("GET")`.
    pub fn new(name: &str) -> Self {
        Cmd {
            parts: vec![name.as_bytes().to_vec()],
            invalid: None,
        }
    }

    fn fail(&mut self, e: Error) {
        if self.invalid.is_none() {
            self.invalid = Some(match e {
                Error::InvalidArgument(m) => m,
                other => other.to_string(),
            });
        }
    }

    fn push_result(mut self, r: Result<Vec<u8>>) -> Self {
        match r {
            Ok(b) => self.parts.push(b),
            Err(e) => self.fail(e),
        }
        self
    }

    /// `Ok` when every argument encoded, otherwise
    /// [`Error::InvalidArgument`] naming the first one that did not. The
    /// client checks this before sending a command.
    pub(crate) fn check(&self) -> Result<()> {
        match &self.invalid {
            None => Ok(()),
            Some(m) => Err(Error::InvalidArgument(format!(
                "{} (in {})",
                m,
                String::from_utf8_lossy(self.name())
            ))),
        }
    }

    /// Appends one argument.
    pub fn arg<T: ToArg>(self, a: T) -> Self {
        let r = a.try_to_arg();
        self.push_result(r)
    }

    /// Appends one argument only if `cond` is true.
    pub fn arg_if<T: ToArg>(self, cond: bool, a: T) -> Self {
        if cond {
            self.arg(a)
        } else {
            self
        }
    }

    /// Appends the argument only if `a` is `Some`.
    pub fn arg_opt<T: ToArg>(self, a: Option<T>) -> Self {
        match a {
            Some(v) => self.arg(v),
            None => self,
        }
    }

    /// Appends a keyword token followed by its value, only if `value` is
    /// `Some`. A convenience for the `KEYWORD value` optional trailing
    /// options most of the HotKV and AI commands use.
    pub fn kw_opt<T: ToArg>(mut self, keyword: &str, value: Option<T>) -> Self {
        if let Some(v) = value {
            self.parts.push(keyword.as_bytes().to_vec());
            return self.arg(v);
        }
        self
    }

    /// Appends every item from an iterator as one argument each.
    pub fn args<I, T>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: ToArg,
    {
        for a in it {
            self = self.arg(a);
        }
        self
    }

    /// Appends a sorted set score, weight or range bound: a finite number
    /// as a plain decimal, or `inf` / `-inf`. NaN fails the command.
    pub fn arg_score(self, v: f64) -> Self {
        self.push_result(numfmt::score(v).map(String::into_bytes))
    }

    /// Appends a non-negative USD amount rounded to at most six fractional
    /// digits (the form `LLMBUDGET.SET` and `LLMSTATS.PRICE SET` require).
    /// A negative, NaN, infinite or oversized amount fails the command.
    pub fn arg_usd(self, v: f64) -> Self {
        self.push_result(numfmt::usd(v).map(String::into_bytes))
    }

    /// Appends a finite, non-negative number as a plain decimal (a cost
    /// that the server reads with `from_chars`).
    pub fn arg_non_negative(self, v: f64) -> Self {
        self.push_result(numfmt::non_negative(v).map(String::into_bytes))
    }

    /// Appends one vector component or threshold, narrowed to the 32 bit
    /// float the server stores (see [`numfmt::f32_component`]).
    pub fn arg_f32(self, v: f64) -> Self {
        self.push_result(numfmt::f32_component(v).map(String::into_bytes))
    }

    /// Appends every component of a vector, one argument each, as
    /// [`Cmd::arg_f32`] does.
    pub fn args_f32(mut self, vector: &[f64]) -> Self {
        for v in vector {
            self = self.arg_f32(*v);
        }
        self
    }

    /// Appends `keyword` and a [`Cmd::arg_f32`] value, only if `value` is `Some`.
    pub fn kw_f32(mut self, keyword: &str, value: Option<f64>) -> Self {
        if let Some(v) = value {
            self.parts.push(keyword.as_bytes().to_vec());
            return self.arg_f32(v);
        }
        self
    }

    /// Appends `keyword` and a [`Cmd::arg_non_negative`] value, only if `value` is `Some`.
    pub fn kw_non_negative(mut self, keyword: &str, value: Option<f64>) -> Self {
        if let Some(v) = value {
            self.parts.push(keyword.as_bytes().to_vec());
            return self.arg_non_negative(v);
        }
        self
    }

    /// Total number of RESP elements (command name plus arguments).
    pub fn len(&self) -> usize {
        self.parts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// The command name, as sent on the wire (first array element).
    pub fn name(&self) -> &[u8] {
        &self.parts[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_args() {
        let cmd = Cmd::new("SET").arg("key").arg("value").kw_opt("EX", Some(60i64));
        assert_eq!(cmd.parts[0], b"SET");
        assert_eq!(cmd.parts[1], b"key");
        assert_eq!(cmd.parts[2], b"value");
        assert_eq!(cmd.parts[3], b"EX");
        assert_eq!(cmd.parts[4], b"60");
    }

    #[test]
    fn kw_opt_skips_none() {
        let cmd = Cmd::new("SET").arg("key").arg("value").kw_opt::<i64>("EX", None);
        assert_eq!(cmd.len(), 3);
    }
}
