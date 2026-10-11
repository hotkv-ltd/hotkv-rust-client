// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! The [`Value`] type: an in-memory representation of any RESP2 or RESP3
//! reply, and the [`FromValue`] trait used to convert it into typed Rust
//! results.

use std::collections::HashMap;

use crate::error::{Error, Result, ServerError};

/// A reply from the server, in either RESP2 or RESP3 shape. The RESP2
/// parser normalizes into the richer RESP3 variants where a natural
/// mapping exists (for example a RESP2 nil bulk string and a RESP3 null
/// both become [`Value::Nil`]); everything else round trips exactly as the
/// wire protocol sent it.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// RESP2 nil bulk/array (`$-1`, `*-1`) or RESP3 null (`_`).
    Nil,
    /// RESP2/RESP3 simple string (`+OK`).
    SimpleString(String),
    /// RESP2/RESP3 integer (`:123`).
    Integer(i64),
    /// RESP2/RESP3 bulk string (`$5\r\nhello`), binary safe.
    Bulk(Vec<u8>),
    /// RESP2/RESP3 array (`*3\r\n...`).
    Array(Vec<Value>),
    /// A server error reply (`-ERR ...` or the RESP3 blob error `!`).
    Error(ServerError),
    /// RESP3 double (`,3.14`). Parses `inf`, `-inf` and `nan` too.
    Double(f64),
    /// RESP3 boolean (`#t` / `#f`).
    Boolean(bool),
    /// RESP3 big number (`(`), kept as its decimal string form.
    BigNumber(String),
    /// RESP3 verbatim string (`=15\r\ntxt:Some string`), format plus text.
    VerbatimString { format: String, text: String },
    /// RESP3 map (`%2\r\n...`).
    Map(Vec<(Value, Value)>),
    /// RESP3 set (`~3\r\n...`).
    Set(Vec<Value>),
    /// RESP3 out of band push message (`>3\r\n...`), e.g. Pub/Sub.
    Push(Vec<Value>),
}

impl Value {
    /// True for [`Value::Nil`].
    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    /// True for [`Value::Error`].
    pub fn is_error(&self) -> bool {
        matches!(self, Value::Error(_))
    }

    /// Returns the elements of an [`Value::Array`], [`Value::Set`] or
    /// [`Value::Push`], or an empty slice for anything else.
    pub fn as_items(&self) -> &[Value] {
        match self {
            Value::Array(v) | Value::Set(v) | Value::Push(v) => v,
            _ => &[],
        }
    }

    /// Converts an ordered reply into a Vec of items, treating
    /// [`Value::Array`], [`Value::Set`] and [`Value::Push`] the same way.
    pub fn into_items(self) -> Result<Vec<Value>> {
        match self {
            Value::Array(v) | Value::Set(v) | Value::Push(v) => Ok(v),
            Value::Nil => Ok(Vec::new()),
            other => Err(Error::Type(format!("expected an array, got {}", other.kind()))),
        }
    }

    /// Converts a reply that is either a native RESP3 map, or an array
    /// shaped as alternating `name, value, name, value, ...` ("flat
    /// array"), or an array of two-element pair arrays/sets (the shape the
    /// server may use for RESP3 `WITHSCORES`-style replies), into a
    /// uniform list of pairs. This lets typed command wrappers accept
    /// whichever of the three shapes the connected server sends.
    pub fn into_pairs(self) -> Result<Vec<(Value, Value)>> {
        match self {
            Value::Map(pairs) => Ok(pairs),
            Value::Array(items) | Value::Set(items) => array_to_pairs(items),
            Value::Nil => Ok(Vec::new()),
            other => Err(Error::Type(format!("expected a map or array, got {}", other.kind()))),
        }
    }

    /// Extracts binary safe bytes from a bulk/simple/verbatim string
    /// reply. Kept as an inherent method rather than a [`FromValue`] impl
    /// because a blanket `Vec<T>` conversion already exists for element
    /// arrays and Rust cannot host both for `Vec<u8>` at once; reach for
    /// this whenever a value might not be valid UTF-8.
    pub fn into_bytes(self) -> Result<Vec<u8>> {
        match self {
            Value::Bulk(b) => Ok(b),
            Value::SimpleString(s) => Ok(s.into_bytes()),
            Value::VerbatimString { text, .. } => Ok(text.into_bytes()),
            other => Err(type_err("bytes", &other)),
        }
    }

    /// Turns a top level error reply into `Err`, passing everything else
    /// through unchanged. Nested errors (inside an array or pipeline
    /// batch) are left as [`Value::Error`] so callers can inspect them
    /// per element instead of failing the whole reply.
    pub fn into_result(self) -> Result<Value> {
        match self {
            Value::Error(e) => Err(e.into()),
            other => Ok(other),
        }
    }

    /// A short human readable name for the reply's RESP type, used in type
    /// conversion error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::SimpleString(_) => "simple string",
            Value::Integer(_) => "integer",
            Value::Bulk(_) => "bulk string",
            Value::Array(_) => "array",
            Value::Error(_) => "error",
            Value::Double(_) => "double",
            Value::Boolean(_) => "boolean",
            Value::BigNumber(_) => "big number",
            Value::VerbatimString { .. } => "verbatim string",
            Value::Map(_) => "map",
            Value::Set(_) => "set",
            Value::Push(_) => "push",
        }
    }
}

/// Splits an array's elements into pairs, accepting either the flat
/// `name, value, name, value, ...` shape or a list of two-element
/// sub-arrays/sub-sets.
fn array_to_pairs(items: Vec<Value>) -> Result<Vec<(Value, Value)>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let all_pairs = items
        .iter()
        .all(|v| matches!(v, Value::Array(a) | Value::Set(a) if a.len() == 2));
    if all_pairs {
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Value::Array(mut a) | Value::Set(mut a) => {
                    let b = a.pop().unwrap();
                    let a = a.pop().unwrap();
                    out.push((a, b));
                }
                _ => unreachable!(),
            }
        }
        Ok(out)
    } else {
        if items.len() % 2 != 0 {
            return Err(Error::Protocol(
                "expected an even number of elements for a flat name/value array".into(),
            ));
        }
        let mut out = Vec::with_capacity(items.len() / 2);
        let mut it = items.into_iter();
        while let (Some(a), Some(b)) = (it.next(), it.next()) {
            out.push((a, b));
        }
        Ok(out)
    }
}

/// Converts a [`Value`] reply into a typed Rust value.
pub trait FromValue: Sized {
    fn from_value(value: Value) -> Result<Self>;
}

impl FromValue for Value {
    fn from_value(value: Value) -> Result<Self> {
        Ok(value)
    }
}

/// Discards the reply, only checking it was not an error (errors already
/// short circuit before `FromValue::from_value` runs; this exists so
/// commands that only reply `OK` can be typed `Result<()>`).
impl FromValue for () {
    fn from_value(_value: Value) -> Result<Self> {
        Ok(())
    }
}

impl FromValue for bool {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Boolean(b) => Ok(b),
            Value::Integer(i) => Ok(i != 0),
            Value::SimpleString(s) => Ok(s == "OK" || s == "1"),
            Value::Bulk(b) => Ok(b.as_slice() == b"1"),
            Value::Nil => Ok(false),
            other => Err(type_err("bool", &other)),
        }
    }
}

macro_rules! from_value_int {
    ($($t:ty),+) => {
        $(impl FromValue for $t {
            fn from_value(value: Value) -> Result<Self> {
                match value {
                    Value::Integer(i) => <$t>::try_from(i)
                        .map_err(|_| Error::Type(format!("integer {} out of range for {}", i, stringify!($t)))),
                    Value::Bulk(b) => std::str::from_utf8(&b)
                        .ok()
                        .and_then(|s| s.parse::<$t>().ok())
                        .ok_or_else(|| Error::Type(format!("cannot parse {:?} as {}", String::from_utf8_lossy(&b), stringify!($t)))),
                    Value::SimpleString(s) => s.parse::<$t>()
                        .map_err(|_| Error::Type(format!("cannot parse {:?} as {}", s, stringify!($t)))),
                    Value::Double(d) => Ok(d as $t),
                    other => Err(type_err(stringify!($t), &other)),
                }
            }
        })+
    };
}
from_value_int!(i64, i32, u64, u32, usize, isize, u16, i16);

macro_rules! from_value_float {
    ($($t:ty),+) => {
        $(impl FromValue for $t {
            fn from_value(value: Value) -> Result<Self> {
                match value {
                    Value::Double(d) => Ok(d as $t),
                    Value::Integer(i) => Ok(i as $t),
                    Value::Bulk(b) => parse_float_bytes(&b).map(|d| d as $t),
                    Value::SimpleString(s) => parse_float_bytes(s.as_bytes()).map(|d| d as $t),
                    other => Err(type_err(stringify!($t), &other)),
                }
            }
        })+
    };
}
from_value_float!(f64, f32);

pub(crate) fn parse_float_bytes(b: &[u8]) -> Result<f64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::Type("expected utf-8 float text".into()))?;
    match s {
        "inf" | "+inf" | "Inf" => Ok(f64::INFINITY),
        "-inf" | "-Inf" => Ok(f64::NEG_INFINITY),
        "nan" | "-nan" | "NaN" => Ok(f64::NAN),
        _ => s
            .parse::<f64>()
            .map_err(|_| Error::Type(format!("cannot parse {:?} as float", s))),
    }
}

impl FromValue for String {
    fn from_value(value: Value) -> Result<Self> {
        match value {
            Value::Bulk(b) => String::from_utf8(b).map_err(|e| Error::Type(format!("invalid utf-8: {}", e))),
            Value::SimpleString(s) => Ok(s),
            Value::VerbatimString { text, .. } => Ok(text),
            Value::BigNumber(s) => Ok(s),
            Value::Integer(i) => Ok(i.to_string()),
            Value::Double(d) => Ok(d.to_string()),
            other => Err(type_err("String", &other)),
        }
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: Value) -> Result<Self> {
        if value.is_nil() {
            Ok(None)
        } else {
            Ok(Some(T::from_value(value)?))
        }
    }
}

impl<T: FromValue> FromValue for Vec<T> {
    fn from_value(value: Value) -> Result<Self> {
        let items = value.into_items()?;
        items.into_iter().map(T::from_value).collect()
    }
}

impl<A: FromValue, B: FromValue> FromValue for (A, B) {
    fn from_value(value: Value) -> Result<Self> {
        let mut items = value.into_items()?;
        if items.len() != 2 {
            return Err(Error::Type(format!("expected 2 elements, got {}", items.len())));
        }
        let b = items.pop().unwrap();
        let a = items.pop().unwrap();
        Ok((A::from_value(a)?, B::from_value(b)?))
    }
}

impl<A: FromValue, B: FromValue, C: FromValue> FromValue for (A, B, C) {
    fn from_value(value: Value) -> Result<Self> {
        let mut items = value.into_items()?;
        if items.len() != 3 {
            return Err(Error::Type(format!("expected 3 elements, got {}", items.len())));
        }
        let c = items.pop().unwrap();
        let b = items.pop().unwrap();
        let a = items.pop().unwrap();
        Ok((A::from_value(a)?, B::from_value(b)?, C::from_value(c)?))
    }
}

impl<A: FromValue, B: FromValue, C: FromValue, D: FromValue> FromValue for (A, B, C, D) {
    fn from_value(value: Value) -> Result<Self> {
        let mut items = value.into_items()?;
        if items.len() != 4 {
            return Err(Error::Type(format!("expected 4 elements, got {}", items.len())));
        }
        let d = items.pop().unwrap();
        let c = items.pop().unwrap();
        let b = items.pop().unwrap();
        let a = items.pop().unwrap();
        Ok((A::from_value(a)?, B::from_value(b)?, C::from_value(c)?, D::from_value(d)?))
    }
}

impl<K: FromValue + std::hash::Hash + Eq, V: FromValue> FromValue for HashMap<K, V> {
    fn from_value(value: Value) -> Result<Self> {
        let pairs = value.into_pairs()?;
        pairs
            .into_iter()
            .map(|(k, v)| Ok((K::from_value(k)?, V::from_value(v)?)))
            .collect()
    }
}

fn type_err(target: &str, value: &Value) -> Error {
    Error::Type(format!("cannot convert {} reply into {}", value.kind(), target))
}

/// Converts a multi pair reply (`WITHSCORES`, `WITHVALUES`, and similar)
/// into a `Vec<(A, B)>`, accepting a RESP3 map, an array of two element
/// pairs, or a flat alternating array. Deliberately not a [`FromValue`]
/// impl for `Vec<(A, B)>`: that would collide with the generic `Vec<T>`
/// impl, which (correctly, for every other `T`) treats the reply as a
/// flat list of `T`-shaped items rather than as a list of pairs.
pub(crate) fn pairs_vec<A: FromValue, B: FromValue>(value: Value) -> Result<Vec<(A, B)>> {
    value
        .into_pairs()?
        .into_iter()
        .map(|(k, v)| Ok((A::from_value(k)?, B::from_value(v)?)))
        .collect()
}

/// Converts a `*.STATS`/`*.INFO`-style reply (a RESP3 map, a flat
/// `name, value, name, value, ...` array, or an array of two-element
/// pairs) into a name -> value lookup, for tolerant field-by-name
/// parsing rather than positional parsing: a field the server does not
/// send is simply absent from the map (callers default it), and a field
/// the client does not know about is silently ignored. This is the
/// parsing rule used for every `*CACHE.STATS`,
/// `LLMSTATS.GET`, `LLMSTATS.PRICE GET`/`LIST` and `LLMBUDGET.STATUS`
/// reply. A key that is not a string, or a duplicate
/// key, is not an error: the last value under a given name wins.
pub(crate) fn named_fields(value: Value) -> Result<HashMap<String, Value>> {
    let pairs = value.into_pairs()?;
    let mut map = HashMap::with_capacity(pairs.len());
    for (k, v) in pairs {
        if let Ok(key) = String::from_value(k) {
            map.insert(key, v);
        }
    }
    Ok(map)
}

/// `map[key]` as an `i64`, or `0` when the field is missing or is not a
/// value `i64::from_value` accepts.
pub(crate) fn field_i64(map: &HashMap<String, Value>, key: &str) -> i64 {
    map.get(key).cloned().and_then(|v| i64::from_value(v).ok()).unwrap_or(0)
}

/// `map[key]` as an `f64`, or `0.0` when the field is missing or is not a
/// value `f64::from_value` accepts.
pub(crate) fn field_f64(map: &HashMap<String, Value>, key: &str) -> f64 {
    map.get(key).cloned().and_then(|v| f64::from_value(v).ok()).unwrap_or(0.0)
}

/// `map[key]` as a `String`, or `""` when the field is missing or is not
/// a value `String::from_value` accepts.
pub(crate) fn field_string(map: &HashMap<String, Value>, key: &str) -> String {
    map.get(key).cloned().and_then(|v| String::from_value(v).ok()).unwrap_or_default()
}

/// `map[key]` as a boolean encoded as the integer `0`/`1`, or `false`
/// when the field is missing.
pub(crate) fn field_bool_int(map: &HashMap<String, Value>, key: &str) -> bool {
    field_i64(map, key) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_array_to_pairs() {
        let v = Value::Array(vec![
            Value::Bulk(b"a".to_vec()),
            Value::Integer(1),
            Value::Bulk(b"b".to_vec()),
            Value::Integer(2),
        ]);
        let pairs = v.into_pairs().unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], (Value::Bulk(b"a".to_vec()), Value::Integer(1)));
    }

    #[test]
    fn array_of_pairs_to_pairs() {
        let v = Value::Array(vec![
            Value::Array(vec![Value::Bulk(b"a".to_vec()), Value::Integer(1)]),
            Value::Array(vec![Value::Bulk(b"b".to_vec()), Value::Integer(2)]),
        ]);
        let pairs = v.into_pairs().unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[1], (Value::Bulk(b"b".to_vec()), Value::Integer(2)));
    }

    #[test]
    fn map_passthrough() {
        let v = Value::Map(vec![(Value::Bulk(b"a".to_vec()), Value::Integer(1))]);
        let pairs = v.into_pairs().unwrap();
        assert_eq!(pairs.len(), 1);
    }

    #[test]
    fn nil_to_option() {
        assert_eq!(Option::<i64>::from_value(Value::Nil).unwrap(), None);
        assert_eq!(Option::<i64>::from_value(Value::Integer(4)).unwrap(), Some(4));
    }

    #[test]
    fn set_as_vec() {
        let v = Value::Set(vec![Value::Integer(1), Value::Integer(2)]);
        let items: Vec<i64> = FromValue::from_value(v).unwrap();
        assert_eq!(items, vec![1, 2]);
    }

    #[test]
    fn double_parses_inf() {
        assert_eq!(f64::from_value(Value::Bulk(b"inf".to_vec())).unwrap(), f64::INFINITY);
    }

    #[test]
    fn named_fields_from_flat_array() {
        let v = Value::Array(vec![
            Value::Bulk(b"entries".to_vec()),
            Value::Integer(3),
            Value::Bulk(b"hit_rate".to_vec()),
            Value::Bulk(b"0.5".to_vec()),
        ]);
        let m = named_fields(v).unwrap();
        assert_eq!(field_i64(&m, "entries"), 3);
        assert_eq!(field_f64(&m, "hit_rate"), 0.5);
    }

    #[test]
    fn named_fields_from_resp3_map() {
        let v = Value::Map(vec![
            (Value::Bulk(b"state".to_vec()), Value::Bulk(b"valid".to_vec())),
        ]);
        let m = named_fields(v).unwrap();
        assert_eq!(field_string(&m, "state"), "valid");
    }

    #[test]
    fn named_fields_missing_defaults_and_ignores_unknown() {
        let v = Value::Array(vec![
            Value::Bulk(b"future_field".to_vec()),
            Value::Bulk(b"surprise".to_vec()),
        ]);
        let m = named_fields(v).unwrap();
        // A field the client does not know about is ignored...
        assert_eq!(field_string(&m, "future_field"), "surprise");
        // ...and a field the client expects but the server omitted defaults
        // to zero/empty rather than erroring.
        assert_eq!(field_i64(&m, "entries"), 0);
        assert_eq!(field_f64(&m, "hit_rate"), 0.0);
        assert_eq!(field_string(&m, "state"), "");
        assert!(!field_bool_int(&m, "online"));
    }
}
