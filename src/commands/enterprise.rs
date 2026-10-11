// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Enterprise, non-AI command families: `RATELIMIT.*`, `TS.*`,
//! `FEATURE.*`, `OBSERVE`/`ALERTS`. All need a server license that
//! includes the matching feature; without one they answer `NOLICENSE`.

use crate::args::{Cmd, ToArg};
use crate::client::Client;
use crate::error::{Error, Result};
use crate::value::{field_bool_int, field_f64, field_i64, field_string, named_fields, pairs_vec, FromValue, Value};

fn decision5(value: Value) -> Result<[i64; 5]> {
    let items = value.into_items()?;
    if items.len() != 5 {
        return Err(Error::Protocol(format!("expected 5 elements, got {}", items.len())));
    }
    let mut it = items.into_iter();
    let mut next = || i64::from_value(it.next().unwrap());
    Ok([next()?, next()?, next()?, next()?, next()?])
}

impl Client {
    pub fn ratelimit(&self) -> RateLimitCommands {
        RateLimitCommands(self.clone())
    }
    pub fn ts(&self) -> TsCommands {
        TsCommands(self.clone())
    }
    pub fn feature(&self) -> FeatureCommands {
        FeatureCommands(self.clone())
    }
    pub fn observe(&self) -> ObserveCommands {
        ObserveCommands(self.clone())
    }
    pub fn alerts(&self) -> AlertsCommands {
        AlertsCommands(self.clone())
    }
}

// -------------------------------------------------------- RATELIMIT.* --

/// One `RATELIMIT.CHECK`/`RATELIMIT.PEEK` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitDecision {
    pub allowed: bool,
    pub remaining: i64,
    pub retry_after_ms: i64,
    pub limit: i64,
    pub current: i64,
}

impl FromValue for RateLimitDecision {
    fn from_value(value: Value) -> Result<Self> {
        let [allowed, remaining, retry_after_ms, limit, current] = decision5(value)?;
        Ok(RateLimitDecision {
            allowed: allowed != 0,
            remaining,
            retry_after_ms,
            limit,
            current,
        })
    }
}

/// `RATELIMIT.STATS` fields.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitStats {
    pub name: String,
    pub algo: String,
    pub limit: i64,
    pub window_ms: i64,
    pub unique_keys: i64,
    pub total_allowed: i64,
    pub total_denied: i64,
}

impl FromValue for RateLimitStats {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(RateLimitStats {
            name: field_string(&m, "name"),
            algo: field_string(&m, "algo"),
            limit: field_i64(&m, "limit"),
            window_ms: field_i64(&m, "window_ms"),
            unique_keys: field_i64(&m, "unique_keys"),
            total_allowed: field_i64(&m, "total_allowed"),
            total_denied: field_i64(&m, "total_denied"),
        })
    }
}

/// `RATELIMIT.*` commands. Reach it with [`Client::ratelimit`].
pub struct RateLimitCommands(pub(crate) Client);

impl RateLimitCommands {
    /// `RATELIMIT.CREATE name limit window_ms [ALGO a] [REFILL r] [DRAIN d]`.
    /// `algo` is one of `"sliding_window"`, `"token_bucket"`,
    /// `"leaky_bucket"`.
    pub async fn create(&self, name: impl ToArg, limit: i64, window_ms: i64, algo: Option<&str>, refill: Option<f64>, drain: Option<f64>) -> Result<()> {
        let cmd = Cmd::new("RATELIMIT.CREATE")
            .arg(name)
            .arg(limit)
            .arg(window_ms)
            .kw_opt("ALGO", algo)
            .kw_opt("REFILL", refill)
            .kw_opt("DRAIN", drain);
        self.0.call(cmd).await
    }

    /// `RATELIMIT.CHECK name key [COST n]` (mutating: consumes tokens).
    pub async fn check(&self, name: impl ToArg, key: impl ToArg, cost: Option<i64>) -> Result<RateLimitDecision> {
        self.0.call(Cmd::new("RATELIMIT.CHECK").arg(name).arg(key).kw_opt("COST", cost)).await
    }

    /// `RATELIMIT.PEEK name key` (non-mutating).
    pub async fn peek(&self, name: impl ToArg, key: impl ToArg) -> Result<RateLimitDecision> {
        self.0.call_ro(Cmd::new("RATELIMIT.PEEK").arg(name).arg(key)).await
    }

    /// `RATELIMIT.RESET name key`.
    pub async fn reset(&self, name: impl ToArg, key: impl ToArg) -> Result<()> {
        self.0.call(Cmd::new("RATELIMIT.RESET").arg(name).arg(key)).await
    }

    /// `RATELIMIT.STATS name`.
    pub async fn stats(&self, name: impl ToArg) -> Result<RateLimitStats> {
        self.0.call_ro(Cmd::new("RATELIMIT.STATS").arg(name)).await
    }

    /// `RATELIMIT.DROP name`.
    pub async fn drop(&self, name: impl ToArg) -> Result<()> {
        self.0.call(Cmd::new("RATELIMIT.DROP").arg(name)).await
    }

    /// `RATELIMIT.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("RATELIMIT.LIST")).await
    }
}

// ---------------------------------------------------------------- TS.* --

/// `TS.INFO` fields. `labels` is a nested array of `(label, value)`, not
/// flattened into the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct TsInfo {
    pub key: String,
    pub total_samples: i64,
    pub first_timestamp: i64,
    pub last_timestamp: i64,
    pub last_value: f64,
    pub retention_ms: i64,
    pub memory_bytes: i64,
    pub labels: Vec<(String, String)>,
    pub rules: i64,
}

impl FromValue for TsInfo {
    fn from_value(value: Value) -> Result<Self> {
        let mut m = named_fields(value)?;
        let labels = match m.remove("labels") {
            Some(v) => Vec::<(String, String)>::from_value(v)?,
            None => Vec::new(),
        };
        Ok(TsInfo {
            key: field_string(&m, "key"),
            total_samples: field_i64(&m, "totalSamples"),
            first_timestamp: field_i64(&m, "firstTimestamp"),
            last_timestamp: field_i64(&m, "lastTimestamp"),
            last_value: field_f64(&m, "lastValue"),
            retention_ms: field_i64(&m, "retentionMs"),
            memory_bytes: field_i64(&m, "memoryBytes"),
            labels,
            rules: field_i64(&m, "rules"),
        })
    }
}

/// `TS.*` (time series) commands. Reach it with [`Client::ts`].
pub struct TsCommands(pub(crate) Client);

impl TsCommands {
    /// `TS.CREATE key [RETENTION ms] [LABELS l1 v1 ...]`. `RETENTION`
    /// must be given before `LABELS` on the wire; this always emits them
    /// in that order.
    pub async fn create(&self, key: impl ToArg, retention_ms: Option<i64>, labels: &[(&str, &str)]) -> Result<()> {
        let mut cmd = Cmd::new("TS.CREATE").arg(key).kw_opt("RETENTION", retention_ms);
        if !labels.is_empty() {
            cmd = cmd.arg("LABELS");
            for (k, v) in labels {
                cmd = cmd.arg(*k).arg(*v);
            }
        }
        self.0.call(cmd).await
    }

    /// `TS.ADD key timestamp|* value` -> the timestamp (ms) used.
    pub async fn add(&self, key: impl ToArg, timestamp: impl ToArg, value: f64) -> Result<i64> {
        self.0.call(Cmd::new("TS.ADD").arg(key).arg(timestamp).arg(value)).await
    }

    /// `TS.RANGE key from to [COUNT n]`. `from`/`to` accept `"-"`, `"+"`
    /// or a millisecond timestamp.
    pub async fn range(&self, key: impl ToArg, from: impl ToArg, to: impl ToArg, count: Option<i64>) -> Result<Vec<(i64, f64)>> {
        self.0.call_ro(Cmd::new("TS.RANGE").arg(key).arg(from).arg(to).kw_opt("COUNT", count)).await
    }

    /// `TS.AGG key from to bucket_ms agg_type`. `agg_type` is one of
    /// `"avg"`, `"min"`, `"max"`, `"sum"`, `"count"`.
    pub async fn agg(&self, key: impl ToArg, from: impl ToArg, to: impl ToArg, bucket_ms: i64, agg_type: &str) -> Result<Vec<(i64, f64)>> {
        let cmd = Cmd::new("TS.AGG").arg(key).arg(from).arg(to).arg(bucket_ms).arg(agg_type);
        self.0.call_ro(cmd).await
    }

    /// `TS.INFO key`.
    pub async fn info(&self, key: impl ToArg) -> Result<TsInfo> {
        self.0.call_ro(Cmd::new("TS.INFO").arg(key)).await
    }

    /// `TS.DELETE key`.
    pub async fn delete(&self, key: impl ToArg) -> Result<()> {
        self.0.call(Cmd::new("TS.DELETE").arg(key)).await
    }

    /// `TS.RETENTION key retention_ms`.
    pub async fn retention(&self, key: impl ToArg, retention_ms: i64) -> Result<()> {
        self.0.call(Cmd::new("TS.RETENTION").arg(key).arg(retention_ms)).await
    }

    /// `TS.DOWNSAMPLE src_key dest_key agg_type bucket_ms`.
    pub async fn downsample(&self, src_key: impl ToArg, dest_key: impl ToArg, agg_type: &str, bucket_ms: i64) -> Result<()> {
        let cmd = Cmd::new("TS.DOWNSAMPLE").arg(src_key).arg(dest_key).arg(agg_type).arg(bucket_ms);
        self.0.call(cmd).await
    }

    /// `TS.LIST [LABEL key value]`.
    pub async fn list(&self, label: Option<(&str, &str)>) -> Result<Vec<String>> {
        let mut cmd = Cmd::new("TS.LIST");
        if let Some((k, v)) = label {
            cmd = cmd.arg("LABEL").arg(k).arg(v);
        }
        self.0.call_ro(cmd).await
    }
}

// ------------------------------------------------------------ FEATURE.* --

/// One `FEATURE.GET`/`FEATURE.BATCH` record.
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureRecord {
    pub features: Vec<(String, String)>,
    pub updated_at_ms: i64,
    pub freshness_ms: i64,
}

impl FromValue for FeatureRecord {
    fn from_value(value: Value) -> Result<Self> {
        let mut items = value.into_items()?;
        if items.len() != 3 {
            return Err(Error::Protocol(format!("expected 3 elements in a FEATURE record, got {}", items.len())));
        }
        let freshness_ms = i64::from_value(items.pop().unwrap())?;
        let updated_at_ms = i64::from_value(items.pop().unwrap())?;
        let features = pairs_vec(items.pop().unwrap())?;
        Ok(FeatureRecord { features, updated_at_ms, freshness_ms })
    }
}

/// `FEATURE.STATS` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureStats {
    pub entities: i64,
    pub total_reads: i64,
    pub total_writes: i64,
    pub memory_bytes: i64,
}

impl FromValue for FeatureStats {
    fn from_value(value: Value) -> Result<Self> {
        let m = named_fields(value)?;
        Ok(FeatureStats {
            entities: field_i64(&m, "entities"),
            total_reads: field_i64(&m, "totalReads"),
            total_writes: field_i64(&m, "totalWrites"),
            memory_bytes: field_i64(&m, "memoryBytes"),
        })
    }
}

/// `FEATURE.*` (feature store) commands. Reach it with [`Client::feature`].
pub struct FeatureCommands(pub(crate) Client);

impl FeatureCommands {
    /// `FEATURE.CREATE group description SCHEMA name1 type1 [...]`.
    /// `type` is one of `"int64"`, `"double"`/`"float"`, `"string"`,
    /// `"int_array"`, `"double_array"`/`"float_array"`.
    pub async fn create(&self, group: impl ToArg, description: impl ToArg, schema: &[(&str, &str)]) -> Result<()> {
        let mut cmd = Cmd::new("FEATURE.CREATE").arg(group).arg(description).arg("SCHEMA");
        for (name, ty) in schema {
            cmd = cmd.arg(*name).arg(*ty);
        }
        self.0.call(cmd).await
    }

    /// `FEATURE.SET group entity feature1 value1 [...]`.
    pub async fn set<I, F, V>(&self, group: impl ToArg, entity: impl ToArg, features: I) -> Result<()>
    where
        I: IntoIterator<Item = (F, V)>,
        F: ToArg,
        V: ToArg,
    {
        let mut cmd = Cmd::new("FEATURE.SET").arg(group).arg(entity);
        for (f, v) in features {
            cmd = cmd.arg(f).arg(v);
        }
        self.0.call(cmd).await
    }

    /// `FEATURE.SETAT group entity unix_ms feature1 value1 [...]`: like
    /// [`FeatureCommands::set`], but the record's update time is `unix_ms`
    /// instead of now (for back-filling historical values).
    pub async fn set_at<I, F, V>(&self, group: impl ToArg, entity: impl ToArg, unix_ms: i64, features: I) -> Result<()>
    where
        I: IntoIterator<Item = (F, V)>,
        F: ToArg,
        V: ToArg,
    {
        let mut cmd = Cmd::new("FEATURE.SETAT").arg(group).arg(entity).arg(unix_ms);
        for (f, v) in features {
            cmd = cmd.arg(f).arg(v);
        }
        self.0.call(cmd).await
    }

    /// `FEATURE.GET group entity [feature1 feature2 ...]`. An empty
    /// `features` list omits the filter, returning every stored feature.
    pub async fn get(&self, group: impl ToArg, entity: impl ToArg, features: &[&str]) -> Result<Option<FeatureRecord>> {
        let cmd = Cmd::new("FEATURE.GET").arg(group).arg(entity).args(features.iter().copied());
        self.0.call_ro(cmd).await
    }

    /// `FEATURE.BATCH group entity1 entity2 ... [FEATURES f1 f2 ...]`.
    pub async fn batch(&self, group: impl ToArg, entities: &[&str], features: &[&str]) -> Result<Vec<Option<FeatureRecord>>> {
        let mut cmd = Cmd::new("FEATURE.BATCH").arg(group).args(entities.iter().copied());
        if !features.is_empty() {
            cmd = cmd.arg("FEATURES").args(features.iter().copied());
        }
        self.0.call_ro(cmd).await
    }

    /// `FEATURE.SCHEMA group` -> `(name, type)` pairs.
    pub async fn schema(&self, group: impl ToArg) -> Result<Vec<(String, String)>> {
        self.0.call_ro(Cmd::new("FEATURE.SCHEMA").arg(group)).await
    }

    /// `FEATURE.LIST`.
    pub async fn list(&self) -> Result<Vec<String>> {
        self.0.call_ro(Cmd::new("FEATURE.LIST")).await
    }

    /// `FEATURE.DROP group`.
    pub async fn drop(&self, group: impl ToArg) -> Result<()> {
        self.0.call(Cmd::new("FEATURE.DROP").arg(group)).await
    }

    /// `FEATURE.STATS group`.
    pub async fn stats(&self, group: impl ToArg) -> Result<FeatureStats> {
        self.0.call_ro(Cmd::new("FEATURE.STATS").arg(group)).await
    }
}

// ------------------------------------------------------- OBSERVE/ALERTS --

/// `OBSERVE INFO` fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ObserveInfo {
    pub uptime_seconds: i64,
    pub connected_clients: i64,
    pub total_commands: i64,
    pub used_memory_bytes: i64,
    pub peak_memory_bytes: i64,
    pub total_connections: i64,
    pub enterprise_features: String,
}

/// `OBSERVE METRICS` fields.
#[derive(Debug, Clone, PartialEq)]
pub struct ObserveMetrics {
    pub uptime_seconds: i64,
    pub connected_clients: i64,
    pub total_commands: i64,
    pub total_connections: i64,
    pub used_memory_bytes: i64,
    pub peak_memory_bytes: i64,
    pub slowlog_length: i64,
    pub alerts_sent: i64,
    pub alerts_failed: i64,
    pub num_workers: i64,
}

/// `OBSERVE`/`ALERTS` commands. Reach it with [`Client::observe`] /
/// [`Client::alerts`].
pub struct ObserveCommands(pub(crate) Client);

impl ObserveCommands {
    /// `OBSERVE INFO`.
    pub async fn info(&self) -> Result<ObserveInfo> {
        let m = named_fields(self.0.command_idempotent(Cmd::new("OBSERVE").arg("INFO")).await?)?;
        Ok(ObserveInfo {
            uptime_seconds: field_i64(&m, "uptime_seconds"),
            connected_clients: field_i64(&m, "connected_clients"),
            total_commands: field_i64(&m, "total_commands"),
            used_memory_bytes: field_i64(&m, "used_memory_bytes"),
            peak_memory_bytes: field_i64(&m, "peak_memory_bytes"),
            total_connections: field_i64(&m, "total_connections"),
            enterprise_features: field_string(&m, "enterprise_features"),
        })
    }

    /// `OBSERVE METRICS`.
    pub async fn metrics(&self) -> Result<ObserveMetrics> {
        let m = named_fields(self.0.command_idempotent(Cmd::new("OBSERVE").arg("METRICS")).await?)?;
        Ok(ObserveMetrics {
            uptime_seconds: field_i64(&m, "uptime_seconds"),
            connected_clients: field_i64(&m, "connected_clients"),
            total_commands: field_i64(&m, "total_commands"),
            total_connections: field_i64(&m, "total_connections"),
            used_memory_bytes: field_i64(&m, "used_memory_bytes"),
            peak_memory_bytes: field_i64(&m, "peak_memory_bytes"),
            slowlog_length: field_i64(&m, "slowlog_length"),
            alerts_sent: field_i64(&m, "alerts_sent"),
            alerts_failed: field_i64(&m, "alerts_failed"),
            num_workers: field_i64(&m, "num_workers"),
        })
    }
}

/// `ALERTS INFO` fields.
#[derive(Debug, Clone, PartialEq)]
pub struct AlertsInfo {
    pub enabled: bool,
    pub flag_enabled: bool,
    pub webhook_url: String,
    pub provider: String,
    pub supported_providers: String,
    pub min_severity: String,
    pub alerts_sent: i64,
    pub alerts_failed: i64,
    pub alerts_dropped: i64,
    pub alerts_rate_limited: i64,
    pub alerts_deduped: i64,
    pub alerts_filtered: i64,
    pub alerts_queued: i64,
}

/// `ALERTS.*` commands. Reach it with [`Client::alerts`].
pub struct AlertsCommands(pub(crate) Client);

impl AlertsCommands {
    /// `ALERTS INFO`.
    pub async fn info(&self) -> Result<AlertsInfo> {
        let m = named_fields(self.0.command_idempotent(Cmd::new("ALERTS").arg("INFO")).await?)?;
        Ok(AlertsInfo {
            enabled: field_bool_int(&m, "enabled"),
            flag_enabled: field_bool_int(&m, "flag_enabled"),
            webhook_url: field_string(&m, "webhook_url"),
            provider: field_string(&m, "provider"),
            supported_providers: field_string(&m, "supported_providers"),
            min_severity: field_string(&m, "min_severity"),
            alerts_sent: field_i64(&m, "alerts_sent"),
            alerts_failed: field_i64(&m, "alerts_failed"),
            alerts_dropped: field_i64(&m, "alerts_dropped"),
            alerts_rate_limited: field_i64(&m, "alerts_rate_limited"),
            alerts_deduped: field_i64(&m, "alerts_deduped"),
            alerts_filtered: field_i64(&m, "alerts_filtered"),
            alerts_queued: field_i64(&m, "alerts_queued"),
        })
    }

    /// `ALERTS TEST [info|warning|critical] [message ...]`.
    pub async fn test(&self, severity: Option<&str>, message: Option<&str>) -> Result<String> {
        let mut cmd = Cmd::new("ALERTS").arg("TEST");
        if let Some(s) = severity {
            cmd = cmd.arg(s);
        }
        if let Some(m) = message {
            cmd = cmd.arg(m);
        }
        self.0.call(cmd).await
    }

    /// `ALERTS SETURL url` (empty string disables alerting).
    pub async fn set_url(&self, url: impl ToArg) -> Result<String> {
        self.0.call(Cmd::new("ALERTS").arg("SETURL").arg(url)).await
    }

    /// `ALERTS SETSEVERITY info|warning|critical`.
    pub async fn set_severity(&self, severity: &str) -> Result<String> {
        self.0.call(Cmd::new("ALERTS").arg("SETSEVERITY").arg(severity)).await
    }

    /// `ALERTS ON` (alias `ENABLE`).
    pub async fn on(&self) -> Result<String> {
        self.0.call(Cmd::new("ALERTS").arg("ON")).await
    }

    /// `ALERTS OFF` (alias `DISABLE`).
    pub async fn off(&self) -> Result<String> {
        self.0.call(Cmd::new("ALERTS").arg("OFF")).await
    }
}
