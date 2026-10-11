// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Error types returned by this crate.

use std::fmt;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A parsed server error reply, split into its leading error code and the
/// remaining message text, e.g. `-WRONGTYPE Operation against a key...`
/// parses to `code = "WRONGTYPE"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerError {
    pub code: String,
    pub message: String,
    /// True when the server sent no leading code token (for example
    /// `-TopK: key already exists`): `code` is then the placeholder `ERR`
    /// and `message` is the server's text exactly as sent, which is also
    /// what `Display` shows.
    implied_code: bool,
}

impl ServerError {
    /// Parses a raw RESP error payload (without the leading `-` or `!`)
    /// into a code and message. Servers conventionally prefix every error
    /// with an all-caps token (`ERR`, `WRONGTYPE`, `MOVED`, `NOAUTH`,
    /// `NOLICENSE`, ...); when no such token is present the whole text is
    /// kept as the message and the code defaults to `ERR`.
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim_end_matches(['\r', '\n']);
        match raw.find(' ') {
            Some(idx) => {
                let (head, rest) = raw.split_at(idx);
                let looks_like_code = !head.is_empty()
                    && head.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
                if looks_like_code {
                    ServerError {
                        code: head.to_string(),
                        message: rest.trim_start().to_string(),
                        implied_code: false,
                    }
                } else {
                    ServerError {
                        code: "ERR".to_string(),
                        message: raw.to_string(),
                        implied_code: true,
                    }
                }
            }
            None => ServerError {
                code: raw.to_string(),
                message: String::new(),
                implied_code: false,
            },
        }
    }

    /// Case-insensitive comparison of the error code.
    pub fn is_code(&self, code: &str) -> bool {
        self.code.eq_ignore_ascii_case(code)
    }

    /// True for a `NOLICENSE` reply: the server rejected the command
    /// because the active license does not include the required feature.
    pub fn is_not_licensed(&self) -> bool {
        self.is_code("NOLICENSE")
    }

    /// True for a `MOVED` cluster redirect.
    pub fn is_moved(&self) -> bool {
        self.is_code("MOVED")
    }

    /// True for an `ASK` cluster redirect.
    pub fn is_ask(&self) -> bool {
        self.is_code("ASK")
    }

    /// True for a `WRONGTYPE` reply.
    pub fn is_wrong_type(&self) -> bool {
        self.is_code("WRONGTYPE")
    }

    /// Parses the `<slot> <host>:<port>` payload carried by `MOVED`/`ASK`
    /// error messages.
    pub fn redirect_target(&self) -> Option<(u16, String)> {
        let mut parts = self.message.split_whitespace();
        let slot = parts.next()?.parse().ok()?;
        let addr = parts.next()?.to_string();
        Some((slot, addr))
    }
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.implied_code {
            write!(f, "{}", self.message)
        } else if self.message.is_empty() {
            write!(f, "{}", self.code)
        } else {
            write!(f, "{} {}", self.code, self.message)
        }
    }
}

/// The error type returned by every fallible operation in this crate.
#[derive(Debug)]
pub enum Error {
    /// Low level I/O failure (connect, read, write).
    Io(std::io::Error),
    /// The server sent bytes that do not form a valid RESP2/RESP3 reply.
    Protocol(String),
    /// A connect or command timeout elapsed.
    Timeout(String),
    /// Failed to establish or maintain a connection (host resolution,
    /// handshake failure, unexpected disconnect while a reply was pending).
    Connection(String),
    /// A normal server error reply (`-ERR ...`, `-WRONGTYPE ...`, and so
    /// on), including cluster `MOVED`/`ASK` redirects.
    Server(ServerError),
    /// A server error reply whose code is `NOLICENSE`: the command needs a
    /// license feature the connected server does not have enabled. Kept
    /// distinct from [`Error::Server`] so callers (and tests) can detect
    /// and skip on it.
    NotLicensed(ServerError),
    /// A cluster operation followed more redirects than
    /// [`crate::cluster::ClusterOptions::max_redirects`] allows.
    TooManyRedirects,
    /// The connection pool could not hand out a connection (pool closed,
    /// or timed out waiting for one).
    Pool(String),
    /// A reply value could not be converted to the requested Rust type.
    Type(String),
    /// TLS configuration or handshake failure.
    Tls(String),
    /// A `hotkv://`/`hotkvs://`/`redis://`/`rediss://` connection URL was
    /// malformed.
    Url(String),
    /// The client (or connection) was already closed.
    Closed,
    /// An argument was rejected on the client before anything was sent, for
    /// example a NaN or infinite float, or a negative USD amount. See
    /// [`crate::numfmt`].
    InvalidArgument(String),
}

impl From<ServerError> for Error {
    fn from(e: ServerError) -> Self {
        if e.is_not_licensed() {
            Error::NotLicensed(e)
        } else {
            Error::Server(e)
        }
    }
}

impl Error {
    /// True if this is a [`Error::NotLicensed`] error.
    pub fn is_not_licensed(&self) -> bool {
        matches!(self, Error::NotLicensed(_))
    }

    /// True if this is a `MOVED` or `ASK` cluster redirect.
    pub fn is_redirect(&self) -> bool {
        self.server_error()
            .map(|e| e.is_moved() || e.is_ask())
            .unwrap_or(false)
    }

    /// Borrows the underlying [`ServerError`], if this is a server reply
    /// error (licensed or not).
    pub fn server_error(&self) -> Option<&ServerError> {
        match self {
            Error::Server(e) | Error::NotLicensed(e) => Some(e),
            _ => None,
        }
    }

    /// Best effort classification of whether retrying the same operation
    /// on a fresh connection could succeed: I/O and connection failures
    /// are retryable, protocol and server replies are not.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Error::Io(_) | Error::Connection(_) | Error::Timeout(_)
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {}", e),
            Error::Protocol(s) => write!(f, "protocol error: {}", s),
            Error::Timeout(s) => write!(f, "timeout: {}", s),
            Error::Connection(s) => write!(f, "connection error: {}", s),
            Error::Server(e) => write!(f, "server error: {}", e),
            Error::NotLicensed(e) => write!(f, "not licensed: {}", e),
            Error::TooManyRedirects => write!(f, "too many cluster redirects"),
            Error::Pool(s) => write!(f, "pool error: {}", s),
            Error::Type(s) => write!(f, "type error: {}", s),
            Error::Tls(s) => write!(f, "tls error: {}", s),
            Error::Url(s) => write!(f, "invalid url: {}", s),
            Error::Closed => write!(f, "client closed"),
            Error::InvalidArgument(s) => write!(f, "invalid argument: {}", s),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_error_code_and_message() {
        let e = ServerError::parse("WRONGTYPE Operation against a key holding the wrong kind of value");
        assert_eq!(e.code, "WRONGTYPE");
        assert_eq!(e.message, "Operation against a key holding the wrong kind of value");
    }

    #[test]
    fn parses_nolicense() {
        let e = ServerError::parse("NOLICENSE enterprise feature 'prompt-cache' requires a valid license");
        assert!(e.is_not_licensed());
        assert!(!e.is_moved());
    }

    #[test]
    fn parses_moved_redirect() {
        let e = ServerError::parse("MOVED 3999 127.0.0.1:6381");
        assert!(e.is_moved());
        assert_eq!(e.redirect_target(), Some((3999, "127.0.0.1:6381".to_string())));
    }

    #[test]
    fn falls_back_to_err_code() {
        let e = ServerError::parse("this reply has no leading code token here");
        assert_eq!(e.code, "ERR");
    }

    #[test]
    fn text_without_a_code_token_is_shown_as_the_server_sent_it() {
        // Bloom filter style replies carry no leading ERR token.
        for raw in ["TopK: key already exists", "Invalid information value", "CMS: key does not exist"] {
            let e = ServerError::parse(raw);
            assert_eq!(e.code, "ERR");
            assert_eq!(e.to_string(), raw);
        }
        assert_eq!(ServerError::parse("ERR unknown command 'x'").to_string(), "ERR unknown command 'x'");
        assert_eq!(ServerError::parse("WRONGTYPE bad").to_string(), "WRONGTYPE bad");
    }

    #[test]
    fn message_only_token() {
        let e = ServerError::parse("OOM");
        assert_eq!(e.code, "OOM");
        assert_eq!(e.message, "");
    }
}
