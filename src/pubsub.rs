// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Pub/Sub: `SUBSCRIBE`, `PSUBSCRIBE`, `SSUBSCRIBE` and message delivery.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;

use crate::args::Cmd;
use crate::client::Client;
use crate::error::{Error, Result};
use crate::pool::PooledConnection;
use crate::value::{FromValue, Value};

/// One message or subscription state change delivered on a [`PubSub`]
/// connection.
#[derive(Debug, Clone, PartialEq)]
pub enum PubSubMessage {
    /// A message published to a channel this connection is subscribed to.
    Message { channel: String, payload: Vec<u8> },
    /// A message published to a channel matching a subscribed pattern.
    PMessage {
        pattern: String,
        channel: String,
        payload: Vec<u8>,
    },
    /// A message published to a channel via `SSUBSCRIBE` (sharded).
    SMessage { channel: String, payload: Vec<u8> },
    Subscribed { channel: String, count: i64 },
    Unsubscribed { channel: String, count: i64 },
    PSubscribed { pattern: String, count: i64 },
    PUnsubscribed { pattern: String, count: i64 },
    SSubscribed { channel: String, count: i64 },
    SUnsubscribed { channel: String, count: i64 },
}

type PendingRead = Pin<Box<dyn Future<Output = (PooledConnection, Result<Value>)> + Send>>;

/// A dedicated Pub/Sub connection. Build it with [`Client::pubsub`],
/// [`Client::subscribe`], [`Client::psubscribe`] or [`Client::ssubscribe`].
/// Implements [`futures_core::Stream`] (`Item = Result<PubSubMessage>`);
/// [`PubSub::next`] is available directly too, for callers who would
/// rather not depend on `StreamExt`.
pub struct PubSub {
    conn: Option<PooledConnection>,
    pending: Option<PendingRead>,
}

impl PubSub {
    pub(crate) async fn open(client: &Client) -> Result<Self> {
        let mut conn = client.borrow_conn().await?;
        // This connection now carries subscription state; it must never
        // be handed back to the shared pool for ordinary commands.
        conn.mark_tainted();
        Ok(PubSub {
            conn: Some(conn),
            pending: None,
        })
    }

    fn conn_mut(&mut self) -> Result<&mut PooledConnection> {
        self.conn
            .as_mut()
            .ok_or_else(|| Error::Connection("a Pub/Sub read is already in progress on this connection".into()))
    }

    /// `SUBSCRIBE channel [channel ...]`.
    pub async fn subscribe<I, S>(&mut self, channels: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.send("SUBSCRIBE", channels).await
    }

    /// `PSUBSCRIBE pattern [pattern ...]`.
    pub async fn psubscribe<I, S>(&mut self, patterns: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.send("PSUBSCRIBE", patterns).await
    }

    /// `SSUBSCRIBE channel [channel ...]` (sharded Pub/Sub).
    pub async fn ssubscribe<I, S>(&mut self, channels: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.send("SSUBSCRIBE", channels).await
    }

    /// `UNSUBSCRIBE [channel ...]`; with no channels, unsubscribes from
    /// all of them.
    pub async fn unsubscribe<I, S>(&mut self, channels: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.send("UNSUBSCRIBE", channels).await
    }

    /// `PUNSUBSCRIBE [pattern ...]`.
    pub async fn punsubscribe<I, S>(&mut self, patterns: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.send("PUNSUBSCRIBE", patterns).await
    }

    /// `SUNSUBSCRIBE [channel ...]`.
    pub async fn sunsubscribe<I, S>(&mut self, channels: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.send("SUNSUBSCRIBE", channels).await
    }

    async fn send<I, S>(&mut self, name: &str, args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let cmd = Cmd::new(name).args(args.into_iter().map(|s| s.as_ref().to_string()));
        self.conn_mut()?.send_only(&cmd).await
    }

    /// Waits for the next message or subscription confirmation. Prefer
    /// this over the [`Stream`] impl when not already using `StreamExt`.
    pub async fn next(&mut self) -> Option<Result<PubSubMessage>> {
        std::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }
}

impl Stream for PubSub {
    type Item = Result<PubSubMessage>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if this.pending.is_none() {
                let Some(mut conn) = this.conn.take() else {
                    return Poll::Ready(Some(Err(Error::Connection(
                        "a Pub/Sub read is already in progress on this connection".into(),
                    ))));
                };
                this.pending = Some(Box::pin(async move {
                    let r = conn.recv_value().await;
                    (conn, r)
                }));
            }

            let fut = this.pending.as_mut().expect("just set");
            match fut.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready((conn, result)) => {
                    this.conn = Some(conn);
                    this.pending = None;
                    match result.and_then(parse_message) {
                        Ok(Some(msg)) => return Poll::Ready(Some(Ok(msg))),
                        Ok(None) => continue,
                        Err(e) => return Poll::Ready(Some(Err(e))),
                    }
                }
            }
        }
    }
}

fn parse_message(v: Value) -> Result<Option<PubSubMessage>> {
    let items = match v {
        Value::Push(items) | Value::Array(items) => items,
        Value::Error(e) => return Err(e.into()),
        _ => return Ok(None),
    };
    let mut it = items.into_iter();
    let kind = match it.next() {
        Some(Value::Bulk(b)) => String::from_utf8_lossy(&b).to_ascii_lowercase(),
        Some(Value::SimpleString(s)) => s.to_ascii_lowercase(),
        _ => return Ok(None),
    };

    let msg = match kind.as_str() {
        "message" => PubSubMessage::Message {
            channel: next_field(&mut it)?,
            payload: next_bytes(&mut it)?,
        },
        "pmessage" => PubSubMessage::PMessage {
            pattern: next_field(&mut it)?,
            channel: next_field(&mut it)?,
            payload: next_bytes(&mut it)?,
        },
        "smessage" => PubSubMessage::SMessage {
            channel: next_field(&mut it)?,
            payload: next_bytes(&mut it)?,
        },
        "subscribe" => PubSubMessage::Subscribed {
            channel: next_field(&mut it)?,
            count: next_field(&mut it)?,
        },
        "unsubscribe" => PubSubMessage::Unsubscribed {
            channel: next_field(&mut it)?,
            count: next_field(&mut it)?,
        },
        "psubscribe" => PubSubMessage::PSubscribed {
            pattern: next_field(&mut it)?,
            count: next_field(&mut it)?,
        },
        "punsubscribe" => PubSubMessage::PUnsubscribed {
            pattern: next_field(&mut it)?,
            count: next_field(&mut it)?,
        },
        "ssubscribe" => PubSubMessage::SSubscribed {
            channel: next_field(&mut it)?,
            count: next_field(&mut it)?,
        },
        "sunsubscribe" => PubSubMessage::SUnsubscribed {
            channel: next_field(&mut it)?,
            count: next_field(&mut it)?,
        },
        _ => return Ok(None),
    };
    Ok(Some(msg))
}

fn next_field<T: FromValue>(it: &mut std::vec::IntoIter<Value>) -> Result<T> {
    let v = it
        .next()
        .ok_or_else(|| Error::Protocol("truncated Pub/Sub message".into()))?;
    T::from_value(v)
}

fn next_bytes(it: &mut std::vec::IntoIter<Value>) -> Result<Vec<u8>> {
    let v = it
        .next()
        .ok_or_else(|| Error::Protocol("truncated Pub/Sub message".into()))?;
    v.into_bytes()
}
