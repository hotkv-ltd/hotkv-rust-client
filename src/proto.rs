// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! RESP2 and RESP3 wire encoding and decoding.

use std::future::Future;
use std::pin::Pin;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt};

use crate::args::Cmd;
use crate::error::{Error, Result, ServerError};
use crate::value::{parse_float_bytes, Value};

/// Encodes one command as a RESP array of bulk strings (the wire format
/// every RESP2 and RESP3 server accepts for client requests).
pub(crate) fn encode_command(cmd: &Cmd) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 * cmd.parts.len().max(1));
    out.extend_from_slice(format!("*{}\r\n", cmd.parts.len()).as_bytes());
    for part in &cmd.parts {
        out.extend_from_slice(format!("${}\r\n", part.len()).as_bytes());
        out.extend_from_slice(part);
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// Encodes several commands back to back, for pipelining.
pub(crate) fn encode_commands(cmds: &[Cmd]) -> Vec<u8> {
    let mut out = Vec::new();
    for cmd in cmds {
        out.extend_from_slice(&encode_command(cmd));
    }
    out
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Reads and parses exactly one RESP reply (recursing into nested
/// aggregates). Boxed because async fns cannot recurse directly.
pub(crate) fn read_value<'a, R>(reader: &'a mut R) -> BoxFuture<'a, Result<Value>>
where
    R: AsyncBufRead + Unpin + Send,
{
    Box::pin(async move {
        let line = read_line(reader).await?;
        if line.is_empty() {
            return Err(Error::Protocol("empty reply line".into()));
        }
        let type_byte = line[0];
        let rest = &line[1..];
        match type_byte {
            b'+' => Ok(Value::SimpleString(lossy(rest))),
            b'-' => Ok(Value::Error(ServerError::parse(&lossy(rest)))),
            b':' => Ok(Value::Integer(parse_i64(rest)?)),
            b'$' => {
                let len = parse_i64(rest)?;
                if len < 0 {
                    return Ok(Value::Nil);
                }
                Ok(Value::Bulk(read_payload(reader, len as usize).await?))
            }
            b'*' => {
                let len = parse_i64(rest)?;
                if len < 0 {
                    return Ok(Value::Nil);
                }
                read_array(reader, rest).await.map(Value::Array)
            }
            b'_' => Ok(Value::Nil),
            b'#' => match rest {
                b"t" => Ok(Value::Boolean(true)),
                b"f" => Ok(Value::Boolean(false)),
                other => Err(Error::Protocol(format!("invalid boolean reply: {:?}", lossy(other)))),
            },
            b',' => {
                let s = std::str::from_utf8(rest).map_err(|_| Error::Protocol("invalid double reply".into()))?;
                Ok(Value::Double(parse_float_bytes(s.as_bytes())?))
            }
            b'(' => Ok(Value::BigNumber(lossy(rest))),
            b'!' => {
                let len = parse_i64(rest)?;
                if len < 0 {
                    return Ok(Value::Nil);
                }
                let data = read_payload(reader, len as usize).await?;
                Ok(Value::Error(ServerError::parse(&lossy(&data))))
            }
            b'=' => {
                let len = parse_i64(rest)?;
                if len < 0 {
                    return Ok(Value::Nil);
                }
                let data = read_payload(reader, len as usize).await?;
                Ok(parse_verbatim(data))
            }
            b'%' => {
                let len = parse_i64(rest)?;
                if len < 0 {
                    return Ok(Value::Nil);
                }
                let mut pairs = Vec::with_capacity(len as usize);
                for _ in 0..len {
                    let k = read_value(reader).await?;
                    let v = read_value(reader).await?;
                    pairs.push((k, v));
                }
                Ok(Value::Map(pairs))
            }
            b'~' => read_array(reader, rest).await.map(Value::Set),
            b'>' => read_array(reader, rest).await.map(Value::Push),
            b'|' => {
                // Attribute reply: metadata attached to the value that
                // follows it. We do not expose attributes, so read and
                // discard the map, then return the actual value.
                let len = parse_i64(rest)?;
                if len > 0 {
                    for _ in 0..len {
                        let _k = read_value(reader).await?;
                        let _v = read_value(reader).await?;
                    }
                }
                read_value(reader).await
            }
            other => Err(Error::Protocol(format!(
                "unknown RESP type byte {:?}",
                other as char
            ))),
        }
    })
}

async fn read_array<R>(reader: &mut R, header: &[u8]) -> Result<Vec<Value>>
where
    R: AsyncBufRead + Unpin + Send,
{
    let len = parse_i64(header)?;
    if len < 0 {
        return Ok(Vec::new());
    }
    let mut items = Vec::with_capacity(len as usize);
    for _ in 0..len {
        items.push(read_value(reader).await?);
    }
    Ok(items)
}

fn parse_verbatim(data: Vec<u8>) -> Value {
    if data.len() >= 4 && data[3] == b':' {
        let format = String::from_utf8_lossy(&data[..3]).into_owned();
        let text = String::from_utf8_lossy(&data[4..]).into_owned();
        Value::VerbatimString { format, text }
    } else {
        Value::VerbatimString {
            format: String::new(),
            text: String::from_utf8_lossy(&data).into_owned(),
        }
    }
}

/// Reads one CRLF terminated header line, returning it without the
/// trailing CRLF (or LF).
async fn read_line<R>(reader: &mut R) -> Result<Vec<u8>>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf = Vec::with_capacity(64);
    let n = reader.read_until(b'\n', &mut buf).await?;
    if n == 0 {
        return Err(Error::Connection("server closed the connection".into()));
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    Ok(buf)
}

/// Reads exactly `len` payload bytes followed by the trailing CRLF.
async fn read_payload<R>(reader: &mut R, len: usize) -> Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut data = vec![0u8; len];
    reader.read_exact(&mut data).await?;
    let mut crlf = [0u8; 2];
    reader.read_exact(&mut crlf).await?;
    Ok(data)
}

fn parse_i64(bytes: &[u8]) -> Result<i64> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| Error::Protocol(format!("invalid integer reply: {:?}", lossy(bytes))))
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    async fn decode(bytes: &[u8]) -> Value {
        let mut reader = BufReader::new(bytes);
        read_value(&mut reader).await.expect("decode")
    }

    #[tokio::test]
    async fn encodes_command() {
        let cmd = Cmd::new("SET").arg("k").arg("v");
        let bytes = encode_command(&cmd);
        assert_eq!(bytes, b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n".to_vec());
    }

    #[tokio::test]
    async fn decodes_simple_string() {
        assert_eq!(decode(b"+OK\r\n").await, Value::SimpleString("OK".into()));
    }

    #[tokio::test]
    async fn decodes_integer() {
        assert_eq!(decode(b":1000\r\n").await, Value::Integer(1000));
    }

    #[tokio::test]
    async fn decodes_negative_integer() {
        assert_eq!(decode(b":-1\r\n").await, Value::Integer(-1));
    }

    #[tokio::test]
    async fn decodes_bulk_string_binary_safe() {
        let mut input = Vec::new();
        input.extend_from_slice(b"$5\r\n");
        input.extend_from_slice(&[0u8, 1, 2, 255, b'x']);
        input.extend_from_slice(b"\r\n");
        assert_eq!(decode(&input).await, Value::Bulk(vec![0, 1, 2, 255, b'x']));
    }

    #[tokio::test]
    async fn decodes_nil_bulk_resp2() {
        assert_eq!(decode(b"$-1\r\n").await, Value::Nil);
    }

    #[tokio::test]
    async fn decodes_nil_array_resp2() {
        assert_eq!(decode(b"*-1\r\n").await, Value::Nil);
    }

    #[tokio::test]
    async fn decodes_resp3_null() {
        assert_eq!(decode(b"_\r\n").await, Value::Nil);
    }

    #[tokio::test]
    async fn decodes_array() {
        let v = decode(b"*2\r\n:1\r\n:2\r\n").await;
        assert_eq!(v, Value::Array(vec![Value::Integer(1), Value::Integer(2)]));
    }

    #[tokio::test]
    async fn decodes_nested_array() {
        let v = decode(b"*1\r\n*2\r\n:1\r\n:2\r\n").await;
        assert_eq!(
            v,
            Value::Array(vec![Value::Array(vec![Value::Integer(1), Value::Integer(2)])])
        );
    }

    #[tokio::test]
    async fn decodes_error() {
        let v = decode(b"-WRONGTYPE bad type\r\n").await;
        match v {
            Value::Error(e) => {
                assert_eq!(e.code, "WRONGTYPE");
                assert_eq!(e.message, "bad type");
            }
            _ => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn decodes_resp3_double() {
        assert_eq!(decode(b",3.25\r\n").await, Value::Double(3.25));
        assert_eq!(decode(b",inf\r\n").await, Value::Double(f64::INFINITY));
    }

    #[tokio::test]
    async fn decodes_resp3_boolean() {
        assert_eq!(decode(b"#t\r\n").await, Value::Boolean(true));
        assert_eq!(decode(b"#f\r\n").await, Value::Boolean(false));
    }

    #[tokio::test]
    async fn decodes_resp3_big_number() {
        assert_eq!(
            decode(b"(3492890328409238509324850943850943825024385\r\n").await,
            Value::BigNumber("3492890328409238509324850943850943825024385".into())
        );
    }

    #[tokio::test]
    async fn decodes_resp3_blob_error() {
        let v = decode(b"!21\r\nSYNTAX invalid syntax\r\n").await;
        match v {
            Value::Error(e) => assert_eq!(e.code, "SYNTAX"),
            _ => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn decodes_resp3_verbatim_string() {
        let v = decode(b"=15\r\ntxt:Some string\r\n").await;
        assert_eq!(
            v,
            Value::VerbatimString {
                format: "txt".into(),
                text: "Some string".into()
            }
        );
    }

    #[tokio::test]
    async fn decodes_resp3_map() {
        let v = decode(b"%1\r\n$3\r\nkey\r\n:1\r\n").await;
        assert_eq!(v, Value::Map(vec![(Value::Bulk(b"key".to_vec()), Value::Integer(1))]));
    }

    #[tokio::test]
    async fn decodes_resp3_set() {
        let v = decode(b"~2\r\n:1\r\n:2\r\n").await;
        assert_eq!(v, Value::Set(vec![Value::Integer(1), Value::Integer(2)]));
    }

    #[tokio::test]
    async fn decodes_resp3_push() {
        let v = decode(b">2\r\n+message\r\n:1\r\n").await;
        assert_eq!(v, Value::Push(vec![Value::SimpleString("message".into()), Value::Integer(1)]));
    }

    #[tokio::test]
    async fn decodes_resp3_attribute_then_value() {
        // Attribute map with one entry, followed by the real reply.
        let v = decode(b"|1\r\n+key\r\n:1\r\n:42\r\n").await;
        assert_eq!(v, Value::Integer(42));
    }

    #[tokio::test]
    async fn streaming_partial_reads_still_decode() {
        // A reader that yields the input one byte at a time still decodes
        // correctly (exercises the partial-read / streaming path).
        struct OneByte<'a> {
            data: &'a [u8],
            pos: usize,
        }
        impl<'a> tokio::io::AsyncRead for OneByte<'a> {
            fn poll_read(
                mut self: std::pin::Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                if self.pos < self.data.len() {
                    buf.put_slice(&[self.data[self.pos]]);
                    self.pos += 1;
                }
                std::task::Poll::Ready(Ok(()))
            }
        }
        let data = b"*2\r\n$3\r\nfoo\r\n:7\r\n".to_vec();
        let src = OneByte { data: &data, pos: 0 };
        let mut reader = BufReader::new(src);
        let v = read_value(&mut reader).await.unwrap();
        assert_eq!(v, Value::Array(vec![Value::Bulk(b"foo".to_vec()), Value::Integer(7)]));
    }
}
