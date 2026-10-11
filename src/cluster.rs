// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Cluster support: CRC16 key hashing, the `CLUSTER SLOTS` slot map, and a
//! [`ClusterClient`] that routes commands by key and follows `MOVED`/`ASK`
//! redirects.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::args::Cmd;
use crate::client::Client;
use crate::error::{Error, Result};
use crate::options::ClientOptions;
use crate::value::{FromValue, Value};

const CLUSTER_SLOT_COUNT: u16 = 16384;

/// Computes the raw CRC16/XMODEM checksum (polynomial 0x1021, initial
/// value 0, no reflection) of `data`: `crc16(b"123456789") == 0x31C3`.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Computes the cluster hash slot (0..16383) for `key`, the same way
/// `CLUSTER KEYSLOT` does: if `key` contains a `{...}` hash tag with at
/// least one byte between the braces, only that inner substring is
/// hashed; otherwise the whole key is hashed.
pub fn key_slot(key: &[u8]) -> u16 {
    let hashed = match key.iter().position(|&b| b == b'{') {
        Some(open) => {
            let rest = &key[open + 1..];
            match rest.iter().position(|&b| b == b'}') {
                Some(close) if close > 0 => &rest[..close],
                _ => key,
            }
        }
        None => key,
    };
    crc16(hashed) % CLUSTER_SLOT_COUNT
}

/// Options controlling [`ClusterClient`] routing behavior.
#[derive(Debug, Clone)]
pub struct ClusterOptions {
    /// Maximum number of `MOVED`/`ASK` redirects followed for one command
    /// before giving up with [`Error::TooManyRedirects`].
    pub max_redirects: usize,
    /// When true, commands sent through [`ClusterClient::execute_readonly`]
    /// prefer a replica for the target slot (sending `READONLY` first),
    /// falling back to the master when the slot has no known replica.
    pub read_from_replicas: bool,
}

impl Default for ClusterOptions {
    fn default() -> Self {
        ClusterOptions {
            max_redirects: 5,
            read_from_replicas: false,
        }
    }
}

#[derive(Debug, Clone)]
struct SlotRange {
    start: u16,
    end: u16,
    master: String,
    replicas: Vec<String>,
}

/// A cluster aware client: discovers the slot map with `CLUSTER SLOTS`,
/// routes each command to the node that owns its key's slot, and follows
/// `MOVED` (refreshing the map) and `ASK` (sending `ASKING` first)
/// redirects transparently.
pub struct ClusterClient {
    base_opts: ClientOptions,
    cluster_opts: ClusterOptions,
    seeds: Vec<(String, u16)>,
    nodes: RwLock<HashMap<String, Client>>,
    slots: RwLock<Vec<SlotRange>>,
}

impl ClusterClient {
    /// Connects to a cluster given one or more seed `host:port` addresses
    /// and discovers the slot map from the first reachable seed.
    /// `base_opts`' `host`/`port` are ignored; every other field (auth,
    /// TLS, pool sizing, timeouts) is reused for every node connection.
    pub async fn connect(seeds: &[&str], base_opts: ClientOptions, cluster_opts: ClusterOptions) -> Result<Self> {
        let mut parsed = Vec::with_capacity(seeds.len());
        for s in seeds {
            let (host, port) = s
                .rsplit_once(':')
                .ok_or_else(|| Error::Url(format!("seed '{}' is not host:port", s)))?;
            let port: u16 = port
                .parse()
                .map_err(|_| Error::Url(format!("invalid port in seed '{}'", s)))?;
            parsed.push((host.to_string(), port));
        }
        let cluster = ClusterClient {
            base_opts,
            cluster_opts,
            seeds: parsed,
            nodes: RwLock::new(HashMap::new()),
            slots: RwLock::new(Vec::new()),
        };
        cluster.refresh_slots().await?;
        Ok(cluster)
    }

    fn node_options(&self, host: &str, port: u16) -> ClientOptions {
        let mut opts = self.base_opts.clone();
        opts.host = host.to_string();
        opts.port = port;
        opts.unix_socket = None;
        opts
    }

    async fn client_for(&self, host: &str, port: u16) -> Result<Client> {
        let addr = format!("{}:{}", host, port);
        if let Some(c) = self.nodes.read().unwrap().get(&addr) {
            return Ok(c.clone());
        }
        let client = Client::with_options(self.node_options(host, port)).await?;
        self.nodes.write().unwrap().insert(addr, client.clone());
        Ok(client)
    }

    async fn client_for_addr(&self, addr: &str) -> Result<Client> {
        let (host, port) = addr
            .rsplit_once(':')
            .ok_or_else(|| Error::Protocol(format!("bad redirect target '{}'", addr)))?;
        let port: u16 = port
            .parse()
            .map_err(|_| Error::Protocol(format!("bad redirect port in '{}'", addr)))?;
        self.client_for(host, port).await
    }

    /// Re-runs `CLUSTER SLOTS` against the first reachable seed or known
    /// node and rebuilds the slot map. Called automatically on `MOVED`;
    /// callers may also call it after a known topology change.
    pub async fn refresh_slots(&self) -> Result<()> {
        let mut candidates: Vec<(String, u16)> = self.seeds.clone();
        for addr in self.nodes.read().unwrap().keys() {
            if let Some((h, p)) = addr.rsplit_once(':') {
                if let Ok(p) = p.parse() {
                    candidates.push((h.to_string(), p));
                }
            }
        }

        let mut last_err = Error::Connection("no cluster seed reachable".into());
        for (host, port) in candidates {
            let client = match self.client_for(&host, port).await {
                Ok(c) => c,
                Err(e) => {
                    last_err = e;
                    continue;
                }
            };
            match client.command(Cmd::new("CLUSTER").arg("SLOTS")).await {
                Ok(v) => {
                    let ranges = parse_cluster_slots(v, &host)?;
                    *self.slots.write().unwrap() = ranges;
                    return Ok(());
                }
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    fn master_for_slot(&self, slot: u16) -> Option<(String, Vec<String>)> {
        let slots = self.slots.read().unwrap();
        slots
            .iter()
            .find(|r| slot >= r.start && slot <= r.end)
            .map(|r| (r.master.clone(), r.replicas.clone()))
    }

    /// Routes `cmd` to the master owning `key`'s slot, following `MOVED`
    /// and `ASK` redirects (bounded by
    /// [`ClusterOptions::max_redirects`]).
    pub async fn execute(&self, key: &str, cmd: Cmd) -> Result<Value> {
        self.execute_inner(key, cmd, false).await
    }

    /// Like [`ClusterClient::execute`], but when
    /// [`ClusterOptions::read_from_replicas`] is enabled and the slot has
    /// a known replica, sends `READONLY` and prefers that replica. Only
    /// use this for commands that are safe to serve from a (possibly
    /// lagging) replica.
    pub async fn execute_readonly(&self, key: &str, cmd: Cmd) -> Result<Value> {
        self.execute_inner(key, cmd, true).await
    }

    async fn execute_inner(&self, key: &str, cmd: Cmd, readonly: bool) -> Result<Value> {
        let slot = key_slot(key.as_bytes());

        if self.master_for_slot(slot).is_none() {
            self.refresh_slots().await?;
        }
        let (master, replicas) = self
            .master_for_slot(slot)
            .ok_or_else(|| Error::Protocol(format!("slot {} is not covered by the cluster map", slot)))?;

        let use_replica = readonly && self.cluster_opts.read_from_replicas && !replicas.is_empty();
        let mut target_addr = if use_replica { replicas[0].clone() } else { master };
        let mut needs_readonly = use_replica;
        let mut asking = false;

        for _ in 0..=self.cluster_opts.max_redirects {
            let client = self.client_for_addr(&target_addr).await?;
            // ASKING applies to the next command on the same connection, and
            // READONLY to the connection it is sent on, so each goes in one
            // write with the command, on one pooled connection. Sent as
            // separate calls, another task could take the connection (or the
            // flag) between the two.
            let prefix = if asking {
                Some("ASKING")
            } else if needs_readonly {
                Some("READONLY")
            } else {
                None
            };
            asking = false;
            needs_readonly = false;

            let reply = match prefix {
                None => client.command(cmd.clone()).await,
                Some(name) => {
                    let mut pipe = client.pipeline();
                    pipe.queue(Cmd::new(name));
                    pipe.queue(cmd.clone());
                    match pipe.execute().await {
                        Ok(mut replies) => replies.pop().unwrap_or(Err(Error::Closed)),
                        Err(e) => Err(e),
                    }
                }
            };
            match reply {
                Ok(v) => return Ok(v),
                Err(Error::Server(e)) if e.is_moved() => {
                    self.refresh_slots().await.ok();
                    let Some((_, addr)) = e.redirect_target() else {
                        return Err(Error::Server(e));
                    };
                    target_addr = addr;
                }
                Err(Error::Server(e)) if e.is_ask() => {
                    let Some((_, addr)) = e.redirect_target() else {
                        return Err(Error::Server(e));
                    };
                    target_addr = addr;
                    asking = true;
                }
                Err(e) => return Err(e),
            }
        }
        Err(Error::TooManyRedirects)
    }

    /// The number of distinct nodes discovered so far.
    pub fn node_count(&self) -> usize {
        self.nodes.read().unwrap().len()
    }
}

fn parse_cluster_slots(v: Value, fallback_host: &str) -> Result<Vec<SlotRange>> {
    let entries = v.into_items()?;
    let mut ranges = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut fields = entry.into_items()?;
        if fields.len() < 3 {
            return Err(Error::Protocol("malformed CLUSTER SLOTS entry".into()));
        }
        let mut drain = fields.drain(..);
        let start = i64::from_value(drain.next().unwrap())? as u16;
        let end = i64::from_value(drain.next().unwrap())? as u16;
        let nodes: Vec<Value> = drain.collect();

        let mut addrs = Vec::with_capacity(nodes.len());
        for node in nodes {
            let mut parts = node.into_items()?.into_iter();
            let ip = String::from_value(parts.next().unwrap_or(Value::Nil))?;
            let port = i64::from_value(parts.next().unwrap_or(Value::Nil))?;
            let host = if ip.is_empty() { fallback_host.to_string() } else { ip };
            addrs.push(format!("{}:{}", host, port));
        }
        if addrs.is_empty() {
            return Err(Error::Protocol("CLUSTER SLOTS entry has no nodes".into()));
        }
        let master = addrs.remove(0);
        ranges.push(SlotRange {
            start,
            end,
            master,
            replicas: addrs,
        });
    }
    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_xmodem_known_vector() {
        assert_eq!(crc16(b"123456789"), 0x31C3);
    }

    #[test]
    fn key_slot_known_vectors() {
        assert_eq!(key_slot(b"foo"), 12182);
        assert_eq!(key_slot(b"bar"), 5061);
        assert_eq!(key_slot(b"hello"), 866);
        assert_eq!(key_slot(b"{user1000}.following"), 3443);
        assert_eq!(key_slot(b"{user1000}.followers"), 3443);
        assert_eq!(key_slot(b"{}foo"), 9500);
        assert_eq!(key_slot(b"foo{}{bar}"), 8363);
        assert_eq!(key_slot(b"foo{{bar}}zap"), 4015);
        assert_eq!(key_slot(b"foo{bar}{zap}"), 5061);
    }

    #[test]
    fn parses_cluster_slots_reply() {
        let v = Value::Array(vec![Value::Array(vec![
            Value::Integer(0),
            Value::Integer(5460),
            Value::Array(vec![
                Value::Bulk(b"10.0.0.1".to_vec()),
                Value::Integer(6379),
                Value::Bulk(b"nodeid1".to_vec()),
            ]),
            Value::Array(vec![
                Value::Bulk(b"10.0.0.2".to_vec()),
                Value::Integer(6379),
                Value::Bulk(b"nodeid2".to_vec()),
            ]),
        ])]);
        let ranges = parse_cluster_slots(v, "127.0.0.1").unwrap();
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start, 0);
        assert_eq!(ranges[0].end, 5460);
        assert_eq!(ranges[0].master, "10.0.0.1:6379");
        assert_eq!(ranges[0].replicas, vec!["10.0.0.2:6379".to_string()]);
    }

    #[test]
    fn empty_ip_falls_back_to_queried_host() {
        let v = Value::Array(vec![Value::Array(vec![
            Value::Integer(0),
            Value::Integer(16383),
            Value::Array(vec![Value::Bulk(b"".to_vec()), Value::Integer(6379), Value::Bulk(b"id".to_vec())]),
        ])]);
        let ranges = parse_cluster_slots(v, "10.1.1.1").unwrap();
        assert_eq!(ranges[0].master, "10.1.1.1:6379");
    }
}
