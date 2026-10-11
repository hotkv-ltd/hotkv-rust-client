// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! [`SemanticCache`]: a get-or-call cache in front of `SCACHE.*`, keyed
//! by embedding similarity rather than an exact prompt match.

use std::future::Future;

use super::LlmCallResult;
use crate::client::Client;
use crate::error::{Error, Result};

/// An idiomatic get-or-call cache in front of `SCACHE.*`: looks up the
/// nearest cached answer to an embedding vector above the namespace's
/// similarity threshold, calling the caller's function only on a miss.
/// Never computes embeddings itself; callers supply vectors from whatever
/// embedding provider they use. Needs a server license that includes the
/// `semantic-cache` feature.
pub struct SemanticCache {
    client: Client,
    ns: String,
    ttl_seconds: Option<i64>,
}

impl SemanticCache {
    /// A `SemanticCache` backed by namespace `ns`, with the given default
    /// TTL for stored entries (`None` for no expiry). Call
    /// [`SemanticCache::create_namespace`] once before first use.
    pub fn new(client: Client, ns: impl Into<String>, ttl_seconds: Option<i64>) -> Self {
        SemanticCache { client, ns: ns.into(), ttl_seconds }
    }

    /// Ensures the `SCACHE` namespace backing this cache exists, with the
    /// given embedding dimensionality, distance metric (`"cosine"`,
    /// `"l2"`, `"ip"`; `None` defaults to cosine) and similarity threshold
    /// (`None` uses the engine default, `0.85`).
    pub async fn create_namespace(&self, dims: i64, metric: Option<&str>, threshold: Option<f64>) -> Result<()> {
        match self.client.scache().create(&self.ns, dims, metric, threshold, self.ttl_seconds).await {
            Ok(()) => Ok(()),
            Err(Error::Server(e)) if e.message.contains("already exists") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Returns the cached answer for the nearest neighbor of `vector`
    /// above the namespace's similarity threshold, if any; otherwise
    /// calls `f`, caches `(query, response, vector)` together with any
    /// usage `f` reports (`model` required for it to be attached; see
    /// [`crate::ai::LlmUsage`]), and returns the fresh answer. The
    /// returned `bool` is whether it came from cache.
    pub async fn get_or_call<F, Fut>(&self, query: &str, vector: &[f64], f: F) -> Result<(String, bool)>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<LlmCallResult>>,
    {
        match self.client.scache().get(&self.ns, vector, None).await {
            Ok(Some((response, _similarity))) => return Ok((response, true)),
            Ok(None) => {}
            Err(e) if e.is_not_licensed() => {}
            Err(e) => return Err(e),
        }

        let result = f().await?;
        let usage = result.usage.as_usage();
        let _ = self.client.scache().set(&self.ns, query, &result.response, vector, self.ttl_seconds, usage.as_ref()).await;
        Ok((result.response, false))
    }

    /// The current `SCACHE.STATS` for this cache's namespace, including
    /// how many tokens and how much cost recorded usage has saved on
    /// hits (see [`crate::ai::UsageSavings`]).
    pub async fn stats(&self) -> Result<crate::ai::ScacheStats> {
        self.client.scache().stats(&self.ns).await
    }
}
