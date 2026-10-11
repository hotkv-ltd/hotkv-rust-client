// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Official Rust client for [HotKV](https://hotkv.com), a RESP2/RESP3
//! compatible in-memory data store with extra HotKV commands and
//! enterprise AI commands (prompt cache, semantic cache, agent memory,
//! RAG, LLM stats/budget, and more).
//!
//! ```no_run
//! use hotkv::Client;
//!
//! # async fn run() -> hotkv::Result<()> {
//! let client = Client::connect("hotkv://127.0.0.1:6379").await?;
//! client.set("greeting", "hello").await?;
//! let value: Option<String> = client.get("greeting").await?;
//! assert_eq!(value.as_deref(), Some("hello"));
//! # Ok(())
//! # }
//! ```
//!
//! See the crate README for TLS, pooling, pipelines, transactions,
//! Pub/Sub, the cluster client and the AI caching helpers.

pub mod ai;
pub mod args;
pub mod blocking;
pub mod cluster;
mod client;
pub mod commands;
mod conn;
mod error;
pub mod numfmt;
mod options;
mod pipeline;
mod pool;
mod proto;
mod pubsub;
mod scan;
#[cfg(feature = "tls")]
mod tls;
mod transaction;
pub mod value;

pub use client::Client;
pub use cluster::{ClusterClient, ClusterOptions};
pub use error::{Error, Result, ServerError};
pub use options::{ClientOptions, TlsOptions};
pub use pipeline::Pipeline;
pub use pool::PoolStats;
pub use pubsub::{PubSub, PubSubMessage};
pub use scan::Scan;
pub use transaction::Transaction;
pub use value::{FromValue, Value};

/// The client library version (matches the crate version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
