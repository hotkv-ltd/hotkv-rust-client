// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Number formatting for command arguments.
//!
//! Every floating point argument this crate sends goes through one of the
//! functions below, so the wire form is always a plain decimal: digits, an
//! optional `.`, an optional leading `-`. Never an exponent (`1e-5`), never a
//! leading `+`, never locale dependent, and always the shortest digit string
//! that parses back to exactly the same value.
//!
//! Rust's `Display` for `f64`/`f32` already has those properties (it never
//! prints an exponent and prints the shortest round trip digits), so the
//! work here is the part `Display` does not do: rejecting values a HotKV
//! parser would refuse (NaN, infinities where they make no sense, negative
//! money), normalising `-0.0`, and rounding USD amounts to the six
//! fractional digits `LLMBUDGET.SET` and `LLMSTATS.PRICE SET` accept.
//!
//! The server side parsers these forms feed (HotKV 0.3.0):
//!
//! - scores, weights and score range bounds (`ZADD`, `ZINCRBY`, `ZRANGEBYSCORE`,
//!   ...) use `strtod` semantics: an exponent and `inf`/`-inf` are accepted,
//!   NaN is not. See [`score`].
//! - `GEOADD`, `GEOSEARCH`, `INCRBYFLOAT`, `HINCRBYFLOAT`, `TS.ADD`, rate
//!   limit rates and `FEATURE` values want finite numbers. See [`finite`].
//! - `PCACHE.CREATE ... COST_PER_HIT` and `LLMBUDGET.CONSUME` / `LLMSTATS.RECORD`
//!   costs want finite, non-negative numbers (`from_chars`: no leading `+`).
//!   See [`non_negative`].
//! - vector components and similarity thresholds are read as 32 bit floats and
//!   must be finite and in `f32` range. See [`f32_component`].
//! - `LLMBUDGET.SET` limits and `LLMSTATS.PRICE SET` prices are fixed point USD:
//!   no sign, no exponent, at most six fractional digits, and the whole value
//!   must fit in an unsigned 64 bit count of millionths. See [`usd`].

use crate::error::{Error, Result};

fn invalid(what: &str, v: f64) -> Error {
    Error::InvalidArgument(format!("{what}: {v} is not allowed"))
}

/// Plain decimal text of a finite `f64`, with `-0.0` written as `0`.
fn plain(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    // `Display` for floats never uses an exponent and prints the shortest
    // digit string that round trips.
    format!("{}", v)
}

/// Formats a finite `f64` as a plain decimal. Rejects NaN and infinities.
///
/// ```
/// use hotkv::numfmt;
/// assert_eq!(numfmt::finite(1e-5).unwrap(), "0.00001");
/// assert_eq!(numfmt::finite(1e21).unwrap(), "1000000000000000000000");
/// assert_eq!(numfmt::finite(-0.0).unwrap(), "0");
/// assert!(numfmt::finite(f64::NAN).is_err());
/// ```
pub fn finite(v: f64) -> Result<String> {
    if !v.is_finite() {
        return Err(invalid("number", v));
    }
    Ok(plain(v))
}

/// Formats a sorted set score, weight or score range bound: a finite plain
/// decimal, or `inf` / `-inf` (which `ZADD` and the `BYSCORE` ranges accept).
/// Rejects NaN.
pub fn score(v: f64) -> Result<String> {
    if v.is_nan() {
        return Err(invalid("score", v));
    }
    if v == f64::INFINITY {
        return Ok("inf".to_string());
    }
    if v == f64::NEG_INFINITY {
        return Ok("-inf".to_string());
    }
    Ok(plain(v))
}

/// Formats a finite, non-negative `f64` (a cost, for example) as a plain
/// decimal. Rejects NaN, infinities and negative numbers; `-0.0` becomes `0`.
pub fn non_negative(v: f64) -> Result<String> {
    if !v.is_finite() || v < 0.0 {
        return Err(invalid("non-negative number", v));
    }
    Ok(plain(v))
}

/// Formats a vector component or threshold the way the server reads it: as a
/// 32 bit float. The value is narrowed to `f32` first and printed with the
/// shortest digits that round trip as an `f32`, so `0.1` stays `0.1` and a
/// value outside the `f32` range (about 3.4e38) is rejected instead of being
/// refused by the server. Rejects NaN and infinities.
pub fn f32_component(v: f64) -> Result<String> {
    if !v.is_finite() {
        return Err(invalid("vector component", v));
    }
    let narrowed = v as f32;
    if !narrowed.is_finite() {
        return Err(Error::InvalidArgument(format!("vector component: {v} is outside the f32 range")));
    }
    if narrowed == 0.0 {
        return Ok("0".to_string());
    }
    Ok(format!("{}", narrowed))
}

/// Largest number of millionths a server accepts (`u64::MAX`).
const MAX_MICROS: u128 = u64::MAX as u128;

/// Formats a non-negative USD amount for `LLMBUDGET.SET` and
/// `LLMSTATS.PRICE SET`: a plain decimal rounded to at most six fractional
/// digits (trailing zeros dropped), within the range the server's fixed
/// point parser accepts.
///
/// ```
/// use hotkv::numfmt;
/// assert_eq!(numfmt::usd(25.0).unwrap(), "25");
/// assert_eq!(numfmt::usd(0.1 + 0.2).unwrap(), "0.3");
/// assert_eq!(numfmt::usd(0.0000015).unwrap(), "0.000002");
/// assert!(numfmt::usd(-1.0).is_err());
/// ```
///
/// Rejects NaN, infinities, negative amounts and amounts too large for
/// `u64` millionths. An amount that rounds to zero is `0`.
pub fn usd(v: f64) -> Result<String> {
    if !v.is_finite() || v < 0.0 {
        return Err(invalid("USD amount", v));
    }
    if v == 0.0 {
        return Ok("0".to_string());
    }
    // `{:.6}` rounds the exact binary value correctly and never uses an exponent.
    let fixed = format!("{:.6}", v);
    let (whole, frac) = fixed.split_once('.').unwrap_or((fixed.as_str(), ""));
    let whole_n: u128 = whole
        .parse()
        .map_err(|_| Error::InvalidArgument(format!("USD amount: {v} is too large")))?;
    let frac_n: u128 = frac.parse().unwrap_or(0);
    let micros = whole_n.checked_mul(1_000_000).and_then(|m| m.checked_add(frac_n));
    match micros {
        Some(m) if m <= MAX_MICROS => {}
        _ => return Err(Error::InvalidArgument(format!("USD amount: {v} is too large"))),
    }
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        Ok(whole.to_string())
    } else {
        Ok(format!("{whole}.{frac}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: f64) {
        let s = finite(v).unwrap();
        assert!(!s.contains('e') && !s.contains('E') && !s.contains('+'), "{s}");
        let back: f64 = s.parse().unwrap();
        assert_eq!(back, v, "{s}");
        let s2 = score(v).unwrap();
        assert_eq!(s, s2);
    }

    #[test]
    fn tiny_huge_negative_integral_round_trip() {
        for v in [
            1e-5,
            1e-300,
            5e-324,
            1.5e-10,
            1e21,
            1e22,
            1.7976931348623157e308,
            f64::MIN_POSITIVE,
            -1e-5,
            -1e21,
            -f64::MAX,
            0.1,
            0.1 + 0.2,
            1.0,
            -3.0,
            42.0,
            123456789012345680.0,
            0.30000000000000004,
        ] {
            roundtrip(v);
        }
    }

    #[test]
    fn plain_decimal_shapes() {
        assert_eq!(finite(1e-5).unwrap(), "0.00001");
        assert_eq!(finite(1.0).unwrap(), "1");
        assert_eq!(finite(-3.0).unwrap(), "-3");
        assert_eq!(finite(1e21).unwrap(), "1000000000000000000000");
        assert_eq!(finite(1e-300).unwrap().len(), 302); // "0." + 299 zeros + "1"
        assert!(finite(f64::MAX).unwrap().len() == 309);
        assert_eq!(finite(2.5).unwrap(), "2.5");
    }

    #[test]
    fn negative_zero_is_zero() {
        assert_eq!(finite(-0.0).unwrap(), "0");
        assert_eq!(score(-0.0).unwrap(), "0");
        assert_eq!(non_negative(-0.0).unwrap(), "0");
        assert_eq!(usd(-0.0).unwrap(), "0");
    }

    #[test]
    fn nan_and_inf_rejected_by_finite() {
        assert!(finite(f64::NAN).is_err());
        assert!(finite(f64::INFINITY).is_err());
        assert!(finite(f64::NEG_INFINITY).is_err());
        assert!(non_negative(f64::NAN).is_err());
        assert!(non_negative(f64::INFINITY).is_err());
        assert!(non_negative(-1e-9).is_err());
        assert_eq!(non_negative(1e-9).unwrap(), "0.000000001");
    }

    #[test]
    fn score_allows_infinities_but_not_nan() {
        assert_eq!(score(f64::INFINITY).unwrap(), "inf");
        assert_eq!(score(f64::NEG_INFINITY).unwrap(), "-inf");
        assert!(score(f64::NAN).is_err());
        assert_eq!(score(-1.5).unwrap(), "-1.5");
    }

    #[test]
    fn f32_components() {
        assert_eq!(f32_component(0.1).unwrap(), "0.1");
        assert_eq!(f32_component(1.0).unwrap(), "1");
        assert_eq!(f32_component(-0.25).unwrap(), "-0.25");
        assert_eq!(f32_component(1e-5).unwrap(), "0.00001");
        assert!(f32_component(1e39).is_err());
        assert!(f32_component(f64::NAN).is_err());
        assert!(f32_component(f64::INFINITY).is_err());
        // Underflows to zero when narrowed (the server stores an f32 anyway).
        assert_eq!(f32_component(1e-300).unwrap(), "0");
        let s = f32_component(3.4028234e38).unwrap();
        assert!(!s.contains('e'));
        assert_eq!(s.parse::<f32>().unwrap(), f32::MAX);
    }

    #[test]
    fn f32_round_trips() {
        for v in [0.1f32, 0.3, 1e-5, 123456.78, 2.718_281_7, f32::MIN_POSITIVE, 1e-40] {
            let s = f32_component(v as f64).unwrap();
            assert_eq!(s.parse::<f32>().unwrap(), v, "{s}");
            assert!(!s.contains('e'));
        }
    }

    #[test]
    fn usd_rounds_to_six_digits_without_exponent() {
        assert_eq!(usd(25.0).unwrap(), "25");
        assert_eq!(usd(0.5).unwrap(), "0.5");
        assert_eq!(usd(0.1 + 0.2).unwrap(), "0.3");
        assert_eq!(usd(1e-5).unwrap(), "0.00001");
        assert_eq!(usd(1e-7).unwrap(), "0");
        assert_eq!(usd(0.0000015).unwrap(), "0.000002");
        assert_eq!(usd(1234.567891234).unwrap(), "1234.567891");
        assert_eq!(usd(3.0).unwrap(), "3");
        assert_eq!(usd(0.000001).unwrap(), "0.000001");
    }

    #[test]
    fn usd_range_and_sign() {
        assert!(usd(-0.5).is_err());
        assert!(usd(f64::NAN).is_err());
        assert!(usd(f64::INFINITY).is_err());
        assert!(usd(1e300).is_err());
        assert!(usd(1e21).is_err());
        // 18446744073709.551615 USD is exactly u64::MAX micros; f64 cannot hit
        // it, but the whole part 18446744073709 is fine and 18446744073710 is not.
        assert_eq!(usd(18446744073709.0).unwrap(), "18446744073709");
        assert!(usd(18446744073710.0).is_err());
        assert!(usd(1e13).is_ok());
    }

    #[test]
    fn usd_output_matches_server_grammar() {
        // digits, optional '.', at most 6 fractional digits, digit before '.',
        // no trailing '.', no sign, no exponent.
        for v in [0.0, 1e-6, 0.5, 7.0, 99.999999, 1e-9, 123456.7, 0.1 + 0.2, 1e12] {
            let s = usd(v).unwrap();
            assert!(!s.starts_with('.') && !s.ends_with('.'), "{s}");
            assert!(s.bytes().all(|b| b.is_ascii_digit() || b == b'.'), "{s}");
            if let Some((_, f)) = s.split_once('.') {
                assert!(f.len() <= 6 && !f.is_empty(), "{s}");
            }
        }
    }
}
