// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Connection options and `hotkv://` URL parsing.

use std::path::PathBuf;
use std::time::Duration;

use crate::error::{Error, Result};

pub(crate) const DEFAULT_PORT: u16 = 6379;

/// TLS configuration. Only meaningful when built with the `tls` cargo
/// feature; with it disabled, setting `enabled = true` fails at connect
/// time with [`Error::Tls`].
#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    pub enabled: bool,
    /// PEM file with additional trusted CA certificates. When unset, the
    /// platform's Mozilla-derived trust roots (via `webpki-roots`) are
    /// used.
    pub ca_file: Option<PathBuf>,
    /// PEM client certificate, for mutual TLS.
    pub cert_file: Option<PathBuf>,
    /// PEM private key matching `cert_file`.
    pub key_file: Option<PathBuf>,
    /// Overrides the server name used for SNI and certificate
    /// verification (defaults to the connection host).
    pub server_name: Option<String>,
    /// Disables certificate verification entirely. For tests only; never
    /// enable this against a production server.
    pub insecure_skip_verify: bool,
}

/// Connection and pool configuration for a [`crate::Client`].
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub host: String,
    pub port: u16,
    /// When set, connect over this Unix domain socket instead of TCP
    /// (Unix-like platforms only).
    pub unix_socket: Option<PathBuf>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Logical database index selected with `SELECT` right after
    /// connecting. `None` skips `SELECT` entirely.
    pub db: Option<i64>,
    /// Name registered with `CLIENT SETNAME` on every new connection.
    pub client_name: Option<String>,
    pub connect_timeout: Duration,
    /// Per command timeout. `None` disables it (wait indefinitely).
    pub command_timeout: Option<Duration>,
    pub tls: TlsOptions,
    /// Maximum number of pooled connections.
    pub pool_size: usize,
    /// Idle connections older than this are closed instead of reused.
    pub idle_timeout: Duration,
    /// How many times an idempotent read is retried, on a fresh
    /// connection, after a connection level failure. Writes are never
    /// retried automatically.
    pub max_retries: u32,
    /// Negotiate RESP3 with `HELLO 3` on connect (the default). The client
    /// still falls back to RESP2 automatically when the server rejects
    /// `HELLO`; set this to `false` to use RESP2 even against a server
    /// that supports RESP3.
    pub resp3: bool,
}

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            host: "127.0.0.1".to_string(),
            port: DEFAULT_PORT,
            unix_socket: None,
            username: None,
            password: None,
            db: None,
            client_name: None,
            connect_timeout: Duration::from_secs(5),
            command_timeout: None,
            tls: TlsOptions::default(),
            pool_size: 10,
            idle_timeout: Duration::from_secs(60),
            max_retries: 1,
            resp3: true,
        }
    }
}

impl ClientOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses a `hotkv://`, `hotkvs://`, `redis://`, `rediss://` or
    /// `unix://` connection URL. `hotkvs`/`rediss` enable TLS.
    /// `redis`/`rediss` are accepted, and behave identically to
    /// `hotkv`/`hotkvs`, for drop-in migration from other clients.
    ///
    /// Form: `scheme://[user[:pass]@]host[:port][/db][?opt=val&...]`.
    /// Recognized query options: `client_name`, `connect_timeout_ms`,
    /// `command_timeout_ms`, `pool_size`, `insecure_skip_verify`.
    pub fn from_url(url: &str) -> Result<Self> {
        let (scheme, remainder) = url
            .split_once("://")
            .ok_or_else(|| Error::Url("missing '://' after the scheme".into()))?;
        let scheme_lc = scheme.to_ascii_lowercase();
        let tls_enabled = matches!(scheme_lc.as_str(), "hotkvs" | "rediss");
        if !matches!(scheme_lc.as_str(), "hotkv" | "hotkvs" | "redis" | "rediss" | "unix") {
            return Err(Error::Url(format!(
                "unsupported scheme '{}' (want hotkv, hotkvs, redis, rediss or unix)",
                scheme
            )));
        }

        let mut opts = ClientOptions::default();
        opts.tls.enabled = tls_enabled;

        if scheme_lc == "unix" {
            let (path, query) = split_query(remainder);
            if path.is_empty() {
                return Err(Error::Url("unix:// URL is missing a socket path".into()));
            }
            opts.unix_socket = Some(PathBuf::from(path));
            apply_query(&mut opts, query)?;
            return Ok(opts);
        }

        let (authority_and_path, query) = split_query(remainder);
        let (userinfo, hostport_and_path) = match authority_and_path.split_once('@') {
            Some((u, rest)) => (Some(u), rest),
            None => (None, authority_and_path),
        };
        let (hostport, path) = match hostport_and_path.split_once('/') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport_and_path, None),
        };

        if let Some(ui) = userinfo {
            match ui.split_once(':') {
                Some((user, pass)) => {
                    if !user.is_empty() {
                        opts.username = Some(percent_decode(user));
                    }
                    opts.password = Some(percent_decode(pass));
                }
                None if !ui.is_empty() => {
                    // A single userinfo component with no colon is the
                    // legacy single-password AUTH form, e.g.
                    // redis://:secret@host is canonical but accept
                    // redis://secret@host too.
                    opts.password = Some(percent_decode(ui));
                }
                None => {}
            }
        }

        let (host, port) = split_host_port(hostport)?;
        opts.host = if host.is_empty() { "127.0.0.1".to_string() } else { host };
        opts.port = port.unwrap_or(DEFAULT_PORT);

        if let Some(p) = path {
            if !p.is_empty() {
                let db = p
                    .parse::<i64>()
                    .map_err(|_| Error::Url(format!("invalid database index '{}'", p)))?;
                opts.db = Some(db);
            }
        }

        apply_query(&mut opts, query)?;
        Ok(opts)
    }
}

fn split_host_port(hostport: &str) -> Result<(String, Option<u16>)> {
    if let Some(rest) = hostport.strip_prefix('[') {
        let (host, after) = rest
            .split_once(']')
            .ok_or_else(|| Error::Url("unterminated '[' in host".into()))?;
        let port = match after.strip_prefix(':') {
            Some(p) if !p.is_empty() => Some(
                p.parse::<u16>()
                    .map_err(|_| Error::Url(format!("invalid port '{}'", p)))?,
            ),
            _ => None,
        };
        return Ok((host.to_string(), port));
    }
    match hostport.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| Error::Url(format!("invalid port '{}'", port)))?;
            Ok((host.to_string(), Some(port)))
        }
        _ => Ok((hostport.to_string(), None)),
    }
}

fn split_query(s: &str) -> (&str, Option<&str>) {
    match s.split_once('?') {
        Some((head, q)) => (head, Some(q)),
        None => (s, None),
    }
}

fn apply_query(opts: &mut ClientOptions, query: Option<&str>) -> Result<()> {
    let Some(query) = query else { return Ok(()) };
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = match pair.split_once('=') {
            Some((k, v)) => (k, percent_decode(v)),
            None => (pair, String::new()),
        };
        match key {
            "client_name" => opts.client_name = Some(value),
            "connect_timeout_ms" => {
                opts.connect_timeout = Duration::from_millis(parse_query_u64(&value, key)?)
            }
            "command_timeout_ms" => {
                opts.command_timeout = Some(Duration::from_millis(parse_query_u64(&value, key)?))
            }
            "pool_size" => opts.pool_size = parse_query_u64(&value, key)? as usize,
            "insecure_skip_verify" => {
                opts.tls.insecure_skip_verify = value.is_empty() || value == "true" || value == "1"
            }
            "username" => opts.username = Some(value),
            "password" => opts.password = Some(value),
            _ => {} // unknown query options are ignored, not rejected
        }
    }
    Ok(())
}

fn parse_query_u64(value: &str, key: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|_| Error::Url(format!("invalid value for '{}': '{}'", key, value)))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_url() {
        let o = ClientOptions::from_url("hotkv://127.0.0.1:6399").unwrap();
        assert_eq!(o.host, "127.0.0.1");
        assert_eq!(o.port, 6399);
        assert!(!o.tls.enabled);
        assert_eq!(o.db, None);
    }

    #[test]
    fn defaults_port_when_missing() {
        let o = ClientOptions::from_url("hotkv://example.com").unwrap();
        assert_eq!(o.port, DEFAULT_PORT);
    }

    #[test]
    fn enables_tls_for_hotkvs_and_rediss() {
        assert!(ClientOptions::from_url("hotkvs://h:1").unwrap().tls.enabled);
        assert!(ClientOptions::from_url("rediss://h:1").unwrap().tls.enabled);
        assert!(!ClientOptions::from_url("redis://h:1").unwrap().tls.enabled);
    }

    #[test]
    fn parses_userinfo_and_db() {
        let o = ClientOptions::from_url("redis://user:p%40ss@host:6380/3").unwrap();
        assert_eq!(o.username.as_deref(), Some("user"));
        assert_eq!(o.password.as_deref(), Some("p@ss"));
        assert_eq!(o.host, "host");
        assert_eq!(o.port, 6380);
        assert_eq!(o.db, Some(3));
    }

    #[test]
    fn parses_password_only_userinfo() {
        let o = ClientOptions::from_url("redis://:secret@host:1").unwrap();
        assert_eq!(o.password.as_deref(), Some("secret"));
        assert_eq!(o.username, None);
    }

    #[test]
    fn parses_query_options() {
        let o = ClientOptions::from_url(
            "hotkv://h:1?client_name=svc&connect_timeout_ms=250&pool_size=4&insecure_skip_verify=true",
        )
        .unwrap();
        assert_eq!(o.client_name.as_deref(), Some("svc"));
        assert_eq!(o.connect_timeout, Duration::from_millis(250));
        assert_eq!(o.pool_size, 4);
        assert!(o.tls.insecure_skip_verify);
    }

    #[test]
    fn parses_ipv6_host() {
        let o = ClientOptions::from_url("hotkv://[::1]:6399").unwrap();
        assert_eq!(o.host, "::1");
        assert_eq!(o.port, 6399);
    }

    #[test]
    fn parses_unix_scheme() {
        let o = ClientOptions::from_url("unix:///tmp/hotkv.sock").unwrap();
        assert_eq!(o.unix_socket, Some(PathBuf::from("/tmp/hotkv.sock")));
    }

    #[test]
    fn rejects_unknown_scheme() {
        assert!(ClientOptions::from_url("ftp://host:1").is_err());
    }

    #[test]
    fn rejects_missing_scheme() {
        assert!(ClientOptions::from_url("127.0.0.1:6399").is_err());
    }
}
