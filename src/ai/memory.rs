// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! `PROMPT.*` (versioned prompt registry), `EMBED.DEDUP.*`, `AMEM.*`
//! (agent memory, plus `AMEM.SUMMARY.*`) and `RAG.*`.

use crate::args::{Cmd, ToArg};
use super::cache::Expiry;
use crate::client::Client;
use crate::error::{Error, Result};
use crate::value::{field_f64, field_i64, field_string, named_fields, pairs_vec, FromValue, Value};

/// A vector as the comma separated float list `RAG.INGEST`/`RAG.SEARCH`
/// read, each component narrowed to the 32 bit float the server stores.
fn join_vector(v: &[f64]) -> Result<String> {
    let parts = v.iter().map(|f| crate::numfmt::f32_component(*f)).collect::<Result<Vec<_>>>()?;
    Ok(parts.join(","))
}


// -------------------------------------------------------------- PROMPT --

/// A `PROMPT.GET`/`PROMPT.LATEST` reply.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptEntry {
    pub version: i64,
    pub content: String,
    pub metadata: String,
    pub created_unix: i64,
}

impl FromValue for PromptEntry {
    fn from_value(value: Value) -> Result<Self> {
        let mut items = value.into_items()?;
        if items.len() != 4 {
            return Err(Error::Protocol(format!("expected 4 elements in a PROMPT entry, got {}", items.len())));
        }
        let created_unix = i64::from_value(items.pop().unwrap())?;
        let metadata = String::from_value(items.pop().unwrap())?;
        let content = String::from_value(items.pop().unwrap())?;
        let version = i64::from_value(items.pop().unwrap())?;
        Ok(PromptEntry { version, content, metadata, created_unix })
    }
}

/// `PROMPT.*` (versioned prompt/template registry) commands. Reach it
/// with [`Client::prompt`].
pub struct PromptCommands(pub(crate) Client);

impl PromptCommands {
    /// `PROMPT.PUT name content [META json]` -> the new version number.
    pub async fn put(&self, name: impl ToArg, content: impl ToArg, meta_json: Option<&str>) -> Result<i64> {
        let cmd = Cmd::new("PROMPT.PUT").arg(name).arg(content).kw_opt("META", meta_json);
        self.0.call(cmd).await
    }

    /// `PROMPT.GET name [version]`. `version` `None` (or `0`) means the
    /// latest version.
    pub async fn get(&self, name: impl ToArg, version: Option<i64>) -> Result<Option<PromptEntry>> {
        let mut cmd = Cmd::new("PROMPT.GET").arg(name);
        if let Some(v) = version {
            cmd = cmd.arg(v);
        }
        self.0.call_ro(cmd).await
    }

    /// `PROMPT.LATEST name`.
    pub async fn latest(&self, name: impl ToArg) -> Result<Option<PromptEntry>> {
        self.0.call_ro(Cmd::new("PROMPT.LATEST").arg(name)).await
    }

    /// `PROMPT.SETACTIVE name version`.
    pub async fn set_active(&self, name: impl ToArg, version: i64) -> Result<()> {
        self.0.call(Cmd::new("PROMPT.SETACTIVE").arg(name).arg(version)).await
    }

    /// `PROMPT.VERSIONS name`.
    pub async fn versions(&self, name: impl ToArg) -> Result<Vec<i64>> {
        self.0.call_ro(Cmd::new("PROMPT.VERSIONS").arg(name)).await
    }

    /// `PROMPT.DROP name`.
    pub async fn drop(&self, name: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("PROMPT.DROP").arg(name)).await
    }

    /// `PROMPT.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("PROMPT.LIST")).await
    }
}

// --------------------------------------------------------- EMBED.DEDUP --

/// `EMBED.DEDUP.STATS` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbedDedupStats {
    pub hits: i64,
    pub misses: i64,
    pub entries: i64,
}

impl FromValue for EmbedDedupStats {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(EmbedDedupStats { hits: field_i64(&m, "hits"), misses: field_i64(&m, "misses"), entries: field_i64(&m, "entries") })
    }
}

/// `EMBED.DEDUP.*` commands. Reach it with [`Client::embed_dedup`].
pub struct EmbedDedupCommands(pub(crate) Client);

impl EmbedDedupCommands {
    /// `EMBED.DEDUP.PUT ns hash model embedding` -> whether it was newly
    /// stored (`false` means the hash already existed and its refcount
    /// was incremented).
    pub async fn put(&self, ns: impl ToArg, hash: impl ToArg, model: impl ToArg, embedding: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("EMBED.DEDUP.PUT").arg(ns).arg(hash).arg(model).arg(embedding)).await
    }

    /// `EMBED.DEDUP.GET ns hash`.
    pub async fn get(&self, ns: impl ToArg, hash: impl ToArg) -> Result<Option<String>> {
        self.0.call_ro(Cmd::new("EMBED.DEDUP.GET").arg(ns).arg(hash)).await
    }

    /// `EMBED.DEDUP.RELEASE ns hash`: decrements the refcount, freeing the
    /// entry at zero.
    pub async fn release(&self, ns: impl ToArg, hash: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("EMBED.DEDUP.RELEASE").arg(ns).arg(hash)).await
    }

    /// `EMBED.DEDUP.STATS ns`.
    pub async fn stats(&self, ns: impl ToArg) -> Result<EmbedDedupStats> {
        self.0.call_ro(Cmd::new("EMBED.DEDUP.STATS").arg(ns)).await
    }

    /// `EMBED.DEDUP.DROP ns`.
    pub async fn drop(&self, ns: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("EMBED.DEDUP.DROP").arg(ns)).await
    }

    /// `EMBED.DEDUP.PURGE ns`: purges expired/unreferenced entries.
    pub async fn purge(&self, ns: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("EMBED.DEDUP.PURGE").arg(ns)).await
    }

    /// `EMBED.DEDUP.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("EMBED.DEDUP.LIST")).await
    }
}

// -------------------------------------------------------------- AMEM.* --

/// One `AMEM.CONV.GET`/`AMEM.CONV.WINDOW` turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvTurn {
    pub role: String,
    pub content: String,
}

impl FromValue for ConvTurn {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(ConvTurn { role: field_string(&m, "role"), content: field_string(&m, "content") })
    }
}

/// One `AMEM.SEM.RECALL` result.
#[derive(Debug, Clone, PartialEq)]
pub struct SemRecall {
    pub key: String,
    pub content: String,
    pub similarity: f64,
}

impl FromValue for SemRecall {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(SemRecall { key: field_string(&m, "key"), content: field_string(&m, "content"), similarity: field_f64(&m, "similarity") })
    }
}

/// One `AMEM.EPIS.QUERY` event.
#[derive(Debug, Clone, PartialEq)]
pub struct EpisodicEvent {
    pub event_type: String,
    pub content: String,
    pub metadata: String,
    pub timestamp_ns: i64,
}

impl FromValue for EpisodicEvent {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(EpisodicEvent {
            event_type: field_string(&m, "type"),
            content: field_string(&m, "content"),
            metadata: field_string(&m, "metadata"),
            timestamp_ns: field_i64(&m, "timestamp"),
        })
    }
}

/// `AMEM.EPIS.STATS` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpisodicStats {
    pub total_events: i64,
    pub oldest_ts: i64,
    pub newest_ts: i64,
}

impl FromValue for EpisodicStats {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(EpisodicStats { total_events: field_i64(&m, "total_events"), oldest_ts: field_i64(&m, "oldest_ts"), newest_ts: field_i64(&m, "newest_ts") })
    }
}

/// One `AMEM.SEARCH` result.
#[derive(Debug, Clone, PartialEq)]
pub struct CrossSearchResult {
    pub source: String,
    pub key: String,
    pub content: String,
    pub similarity: f64,
}

impl FromValue for CrossSearchResult {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(CrossSearchResult {
            source: field_string(&m, "source"),
            key: field_string(&m, "key"),
            content: field_string(&m, "content"),
            similarity: field_f64(&m, "similarity"),
        })
    }
}

/// A stored `AMEM.SUMMARY.GET` summary.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSummary {
    pub summary: String,
    pub version: i64,
    pub updated_unix: i64,
}

impl FromValue for AgentSummary {
    fn from_value(value: Value) -> Result<Self> {
        let (summary, version, updated_unix) = FromValue::from_value(value)?;
        Ok(AgentSummary { summary, version, updated_unix })
    }
}

/// `AMEM.*` (AI agent memory) commands. Reach it with [`Client::amem`].
pub struct AmemCommands(pub(crate) Client);

impl AmemCommands {
    /// `AMEM.CONV.CREATE session_id [WINDOW size] [TTL s]`.
    pub async fn conv_create(&self, session_id: impl ToArg, window: Option<i64>, ttl_seconds: Option<i64>) -> Result<()> {
        self.conv_create_with_expiry(session_id, window, ttl_seconds.map(Expiry::Ttl)).await
    }

    /// `AMEM.CONV.CREATE session_id [WINDOW size] [TTL seconds | PXAT unix-ms]`.
    pub async fn conv_create_with_expiry(&self, session_id: impl ToArg, window: Option<i64>, expiry: Option<Expiry>) -> Result<()> {
        let cmd = Expiry::append(expiry, Cmd::new("AMEM.CONV.CREATE").arg(session_id).kw_opt("WINDOW", window));
        self.0.call(cmd).await
    }

    /// `AMEM.CONV.APPEND session_id role content`.
    pub async fn conv_append(&self, session_id: impl ToArg, role: impl ToArg, content: impl ToArg) -> Result<()> {
        self.0.call(Cmd::new("AMEM.CONV.APPEND").arg(session_id).arg(role).arg(content)).await
    }

    /// `AMEM.CONV.GET session_id`.
    pub async fn conv_get(&self, session_id: impl ToArg) -> Result<Vec<ConvTurn>> {
        self.0.call_ro(Cmd::new("AMEM.CONV.GET").arg(session_id)).await
    }

    /// `AMEM.CONV.WINDOW session_id n`: the last `n` turns.
    pub async fn conv_window(&self, session_id: impl ToArg, n: i64) -> Result<Vec<ConvTurn>> {
        self.0.call_ro(Cmd::new("AMEM.CONV.WINDOW").arg(session_id).arg(n)).await
    }

    /// `AMEM.CONV.CLEAR session_id`.
    pub async fn conv_clear(&self, session_id: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("AMEM.CONV.CLEAR").arg(session_id)).await
    }

    /// `AMEM.CONV.DROP session_id`.
    pub async fn conv_drop(&self, session_id: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("AMEM.CONV.DROP").arg(session_id)).await
    }

    /// `AMEM.SEM.CREATE agent_id dimensions`.
    pub async fn sem_create(&self, agent_id: impl ToArg, dimensions: i64) -> Result<()> {
        self.0.call(Cmd::new("AMEM.SEM.CREATE").arg(agent_id).arg(dimensions)).await
    }

    /// `AMEM.SEM.STORE agent_id key content val1 ... valN [TTL s]`.
    pub async fn sem_store(&self, agent_id: impl ToArg, key: impl ToArg, content: impl ToArg, vector: &[f64], ttl_seconds: Option<i64>) -> Result<()> {
        let cmd = Cmd::new("AMEM.SEM.STORE")
            .arg(agent_id)
            .arg(key)
            .arg(content)
            .args_f32(vector)
            .kw_opt("TTL", ttl_seconds);
        self.0.call(cmd).await
    }

    /// `AMEM.SEM.RECALL agent_id k val1 ... valN`.
    pub async fn sem_recall(&self, agent_id: impl ToArg, k: i64, vector: &[f64]) -> Result<Vec<SemRecall>> {
        let cmd = Cmd::new("AMEM.SEM.RECALL").arg(agent_id).arg(k).args_f32(vector);
        self.0.call_ro(cmd).await
    }

    /// `AMEM.SEM.FORGET agent_id key`.
    pub async fn sem_forget(&self, agent_id: impl ToArg, key: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("AMEM.SEM.FORGET").arg(agent_id).arg(key)).await
    }

    /// `AMEM.SEM.LIST agent_id`.
    pub async fn sem_list(&self, agent_id: impl ToArg) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("AMEM.SEM.LIST").arg(agent_id)).await
    }

    /// `AMEM.WORK.SET session_id key value [TTL s]`.
    pub async fn work_set(&self, session_id: impl ToArg, key: impl ToArg, value: impl ToArg, ttl_seconds: Option<i64>) -> Result<()> {
        self.work_set_with_expiry(session_id, key, value, ttl_seconds.map(Expiry::Ttl)).await
    }

    /// `AMEM.WORK.SET session_id key value [TTL seconds | PXAT unix-ms]`.
    pub async fn work_set_with_expiry(&self, session_id: impl ToArg, key: impl ToArg, value: impl ToArg, expiry: Option<Expiry>) -> Result<()> {
        let cmd = Expiry::append(expiry, Cmd::new("AMEM.WORK.SET").arg(session_id).arg(key).arg(value));
        self.0.call(cmd).await
    }

    /// `AMEM.WORK.GET session_id key`. Note: an empty stored value is
    /// indistinguishable from a miss on the wire; both reply `None` here.
    pub async fn work_get(&self, session_id: impl ToArg, key: impl ToArg) -> Result<Option<String>> {
        self.0.call_ro(Cmd::new("AMEM.WORK.GET").arg(session_id).arg(key)).await
    }

    /// `AMEM.WORK.DEL session_id key`.
    pub async fn work_del(&self, session_id: impl ToArg, key: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("AMEM.WORK.DEL").arg(session_id).arg(key)).await
    }

    /// `AMEM.WORK.CLEAR session_id`.
    pub async fn work_clear(&self, session_id: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("AMEM.WORK.CLEAR").arg(session_id)).await
    }

    /// `AMEM.WORK.LIST session_id`.
    pub async fn work_list(&self, session_id: impl ToArg) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("AMEM.WORK.LIST").arg(session_id)).await
    }

    /// `AMEM.EPIS.LOG agent_id event_type content [METADATA json]`.
    pub async fn epis_log(&self, agent_id: impl ToArg, event_type: impl ToArg, content: impl ToArg, metadata_json: Option<&str>) -> Result<()> {
        let cmd = Cmd::new("AMEM.EPIS.LOG").arg(agent_id).arg(event_type).arg(content).kw_opt("METADATA", metadata_json);
        self.0.call(cmd).await
    }

    /// `AMEM.EPIS.QUERY agent_id [TYPE t] [FROM ts] [TO ts] [LIMIT n]`.
    pub async fn epis_query(
        &self,
        agent_id: impl ToArg,
        event_type: Option<&str>,
        from_ts: Option<i64>,
        to_ts: Option<i64>,
        limit: Option<i64>,
    ) -> Result<Vec<EpisodicEvent>> {
        let cmd = Cmd::new("AMEM.EPIS.QUERY")
            .arg(agent_id)
            .kw_opt("TYPE", event_type)
            .kw_opt("FROM", from_ts)
            .kw_opt("TO", to_ts)
            .kw_opt("LIMIT", limit);
        self.0.call_ro(cmd).await
    }

    /// `AMEM.EPIS.CLEAR agent_id`.
    pub async fn epis_clear(&self, agent_id: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("AMEM.EPIS.CLEAR").arg(agent_id)).await
    }

    /// `AMEM.EPIS.STATS agent_id`.
    pub async fn epis_stats(&self, agent_id: impl ToArg) -> Result<EpisodicStats> {
        self.0.call_ro(Cmd::new("AMEM.EPIS.STATS").arg(agent_id)).await
    }

    /// `AMEM.SEARCH agent_id k val1 ... valN`: searches semantic,
    /// episodic and working memory together.
    pub async fn search(&self, agent_id: impl ToArg, k: i64, vector: &[f64]) -> Result<Vec<CrossSearchResult>> {
        let cmd = Cmd::new("AMEM.SEARCH").arg(agent_id).arg(k).args_f32(vector);
        self.0.call_ro(cmd).await
    }

    /// `AMEM.SUMMARY.SET agent_id summary` -> the new version number.
    /// Needs the `amem-summary` license feature.
    pub async fn summary_set(&self, agent_id: impl ToArg, summary: impl ToArg) -> Result<i64> {
        self.0.call(Cmd::new("AMEM.SUMMARY.SET").arg(agent_id).arg(summary)).await
    }

    /// `AMEM.SUMMARY.APPEND agent_id extra` -> the new version number.
    pub async fn summary_append(&self, agent_id: impl ToArg, extra: impl ToArg) -> Result<i64> {
        self.0.call(Cmd::new("AMEM.SUMMARY.APPEND").arg(agent_id).arg(extra)).await
    }

    /// `AMEM.SUMMARY.GET agent_id`.
    pub async fn summary_get(&self, agent_id: impl ToArg) -> Result<Option<AgentSummary>> {
        self.0.call_ro(Cmd::new("AMEM.SUMMARY.GET").arg(agent_id)).await
    }

    /// `AMEM.SUMMARY.DROP agent_id`.
    pub async fn summary_drop(&self, agent_id: impl ToArg) -> Result<bool> {
        self.0.call(Cmd::new("AMEM.SUMMARY.DROP").arg(agent_id)).await
    }

    /// `AMEM.SUMMARY.LIST`.
    pub async fn summary_list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("AMEM.SUMMARY.LIST")).await
    }
}

// --------------------------------------------------------------- RAG.* --

/// One `RAG.SEARCH` result.
#[derive(Debug, Clone, PartialEq)]
pub struct RagResult {
    pub chunk_id: String,
    pub doc_id: String,
    pub text: String,
    pub score: f64,
    pub vector_score: f64,
    pub bm25_score: f64,
    pub meta: Vec<(String, String)>,
}

impl FromValue for RagResult {
    fn from_value(value: Value) -> Result<Self> {
        let mut items = value.into_items()?;
        if items.len() < 6 {
            return Err(Error::Protocol(format!("expected at least 6 elements in a RAG.SEARCH result, got {}", items.len())));
        }
        let meta_items = items.split_off(6);
        let mut it = items.into_iter();
        let mut next = || it.next().unwrap();
        Ok(RagResult {
            chunk_id: String::from_value(next())?,
            doc_id: String::from_value(next())?,
            text: String::from_value(next())?,
            score: f64::from_value(next())?,
            vector_score: f64::from_value(next())?,
            bm25_score: f64::from_value(next())?,
            meta: pairs_vec(Value::Array(meta_items))?,
        })
    }
}

/// `RAG.STATS` fields.
#[derive(Debug, Clone, PartialEq)]
pub struct RagStats {
    pub name: String,
    pub dimensions: i64,
    pub total_docs: i64,
    pub total_chunks: i64,
    pub total_ingested: i64,
    pub total_queries: i64,
    pub chunk_size: i64,
    pub chunk_strategy: String,
    pub chunk_overlap: i64,
}

impl FromValue for RagStats {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(RagStats {
            name: field_string(&m, "name"),
            dimensions: field_i64(&m, "dimensions"),
            total_docs: field_i64(&m, "totalDocs"),
            total_chunks: field_i64(&m, "totalChunks"),
            total_ingested: field_i64(&m, "totalIngested"),
            total_queries: field_i64(&m, "totalQueries"),
            chunk_size: field_i64(&m, "chunkSize"),
            chunk_strategy: field_string(&m, "chunkStrategy"),
            chunk_overlap: field_i64(&m, "chunkOverlap"),
        })
    }
}

/// `RAG.*` (retrieval augmented generation pipeline) commands. Reach it
/// with [`Client::rag`].
pub struct RagCommands(pub(crate) Client);

impl RagCommands {
    /// `RAG.CREATE name dimensions [STRATEGY s] [CHUNK_SIZE n]
    /// [CHUNK_OVERLAP n]`. `strategy` is one of `"fixed"` (alias
    /// `"fixed_size"`), `"sentence"`, `"paragraph"`.
    pub async fn create(&self, name: impl ToArg, dimensions: i64, strategy: Option<&str>, chunk_size: Option<i64>, chunk_overlap: Option<i64>) -> Result<()> {
        let cmd = Cmd::new("RAG.CREATE")
            .arg(name)
            .arg(dimensions)
            .kw_opt("STRATEGY", strategy)
            .kw_opt("CHUNK_SIZE", chunk_size)
            .kw_opt("CHUNK_OVERLAP", chunk_overlap);
        self.0.call(cmd).await
    }

    /// `RAG.INGEST pipeline doc_id text [META k v ...] [VECTORS vec ...]`
    /// -> chunks ingested. Each vector in `vectors` becomes one
    /// comma-joined `VECTORS` argument (one entry per chunk `text`
    /// produces).
    pub async fn ingest(&self, pipeline: impl ToArg, doc_id: impl ToArg, text: impl ToArg, meta: &[(&str, &str)], vectors: &[&[f64]]) -> Result<i64> {
        let mut cmd = Cmd::new("RAG.INGEST").arg(pipeline).arg(doc_id).arg(text);
        if !meta.is_empty() {
            cmd = cmd.arg("META");
            for (k, v) in meta {
                cmd = cmd.arg(*k).arg(*v);
            }
        }
        if !vectors.is_empty() {
            cmd = cmd.arg("VECTORS");
            for vec in vectors {
                cmd = cmd.arg(join_vector(vec)?);
            }
        }
        self.0.call(cmd).await
    }

    /// `RAG.SEARCH pipeline query_text [K n] [ALPHA f] [VECTOR v,...]
    /// [FILTER k v ...]`.
    pub async fn search(
        &self,
        pipeline: impl ToArg,
        query_text: impl ToArg,
        k: Option<i64>,
        alpha: Option<f64>,
        vector: Option<&[f64]>,
        filter: &[(&str, &str)],
    ) -> Result<Vec<RagResult>> {
        let mut cmd = Cmd::new("RAG.SEARCH").arg(pipeline).arg(query_text).kw_opt("K", k).kw_f32("ALPHA", alpha);
        if let Some(v) = vector {
            cmd = cmd.arg("VECTOR").arg(join_vector(v)?);
        }
        if !filter.is_empty() {
            cmd = cmd.arg("FILTER");
            for (k, v) in filter {
                cmd = cmd.arg(*k).arg(*v);
            }
        }
        self.0.call_ro(cmd).await
    }

    /// `RAG.STATS pipeline`.
    pub async fn stats(&self, pipeline: impl ToArg) -> Result<RagStats> {
        self.0.call_ro(Cmd::new("RAG.STATS").arg(pipeline)).await
    }

    /// `RAG.DROP pipeline`.
    pub async fn drop(&self, pipeline: impl ToArg) -> Result<()> {
        self.0.call(Cmd::new("RAG.DROP").arg(pipeline)).await
    }

    /// `RAG.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("RAG.LIST")).await
    }
}

impl Client {
    /// The `PROMPT.*` (versioned prompt registry) command group.
    pub fn prompt(&self) -> PromptCommands {
        PromptCommands(self.clone())
    }

    /// The `EMBED.DEDUP.*` command group.
    pub fn embed_dedup(&self) -> EmbedDedupCommands {
        EmbedDedupCommands(self.clone())
    }

    /// The `AMEM.*` (AI agent memory) command group.
    pub fn amem(&self) -> AmemCommands {
        AmemCommands(self.clone())
    }

    /// The `RAG.*` command group.
    pub fn rag(&self) -> RagCommands {
        RagCommands(self.clone())
    }
}
