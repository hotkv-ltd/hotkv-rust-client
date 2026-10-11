// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! A single connection: transport, the RESP2/RESP3 `HELLO` handshake, and
//! the request/response plumbing every higher level API is built on.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::net::TcpStream;
#[cfg(unix)]
use tokio::net::UnixStream;

use crate::args::Cmd;
use crate::error::{Error, Result};
use crate::options::ClientOptions;
use crate::proto::{encode_command, encode_commands, read_value};
use crate::value::Value;

#[cfg(feature = "tls")]
use tokio_rustls::client::TlsStream;

/// The underlying byte transport, one of a few concrete stream types.
pub(crate) enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
    #[cfg(feature = "tls")]
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for Stream {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(unix)]
            Stream::Unix(s) => Pin::new(s).poll_read(cx, buf),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(unix)]
            Stream::Unix(s) => Pin::new(s).poll_write(cx, buf),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_flush(cx),
            #[cfg(unix)]
            Stream::Unix(s) => Pin::new(s).poll_flush(cx),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            #[cfg(unix)]
            Stream::Unix(s) => Pin::new(s).poll_shutdown(cx),
            #[cfg(feature = "tls")]
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

async fn dial(opts: &ClientOptions) -> Result<Stream> {
    if let Some(_path) = &opts.unix_socket {
        #[cfg(unix)]
        {
            let s = UnixStream::connect(_path)
                .await
                .map_err(|e| Error::Connection(format!("connect to {} failed: {}", _path.display(), e)))?;
            return Ok(Stream::Unix(s));
        }
        #[cfg(not(unix))]
        {
            return Err(Error::Connection(
                "unix sockets are not supported on this platform".into(),
            ));
        }
    }

    let addr = format!("{}:{}", opts.host, opts.port);
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| Error::Connection(format!("connect to {} failed: {}", addr, e)))?;
    let _ = tcp.set_nodelay(true);

    if opts.tls.enabled {
        #[cfg(feature = "tls")]
        {
            let tls = crate::tls::wrap(tcp, &opts.host, &opts.tls).await?;
            return Ok(Stream::Tls(Box::new(tls)));
        }
        #[cfg(not(feature = "tls"))]
        {
            return Err(Error::Tls(
                "TLS was requested but this build was not compiled with the 'tls' feature".into(),
            ));
        }
    }

    Ok(Stream::Tcp(tcp))
}

/// One connection to a HotKV server: transport plus negotiated protocol
/// state. Not `Sync`; callers hand out `&mut Connection` from a pool.
pub(crate) struct Connection {
    stream: BufReader<Stream>,
    /// RESP protocol version in use on this connection: 3 if `HELLO 3`
    /// succeeded, 2 if the server does not understand `HELLO` and we fell
    /// back to legacy `AUTH`/`SELECT`.
    pub(crate) protocol: u8,
    command_timeout: Option<Duration>,
    last_used_at: Instant,
}

impl Connection {
    pub(crate) async fn connect(opts: &ClientOptions) -> Result<Self> {
        let stream = tokio::time::timeout(opts.connect_timeout, dial(opts))
            .await
            .map_err(|_| Error::Timeout("connect timed out".into()))??;
        let mut buffered = BufReader::new(stream);
        let protocol = tokio::time::timeout(opts.connect_timeout, handshake(&mut buffered, opts))
            .await
            .map_err(|_| Error::Timeout("handshake timed out".into()))??;
        Ok(Connection {
            stream: buffered,
            protocol,
            command_timeout: opts.command_timeout,
            last_used_at: Instant::now(),
        })
    }

    /// Sends one command and returns its reply.
    pub(crate) async fn command(&mut self, cmd: &Cmd) -> Result<Value> {
        cmd.check()?;
        self.last_used_at = Instant::now();
        let timeout = self.command_timeout;
        with_timeout(timeout, async {
            write_command(&mut self.stream, cmd).await?;
            read_value(&mut self.stream).await
        })
        .await
    }

    /// Sends several commands back to back in one write, then reads that
    /// many replies in order (pipelining).
    pub(crate) async fn pipeline(&mut self, cmds: &[Cmd]) -> Result<Vec<Value>> {
        for cmd in cmds {
            cmd.check()?;
        }
        self.last_used_at = Instant::now();
        let timeout = self.command_timeout;
        let n = cmds.len();
        with_timeout(timeout, async {
            let bytes = encode_commands(cmds);
            self.stream.write_all(&bytes).await?;
            self.stream.flush().await?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                out.push(read_value(&mut self.stream).await?);
            }
            Ok(out)
        })
        .await
    }

    /// Sends a command without waiting for a reply, used by Pub/Sub where
    /// replies arrive interleaved with pushed messages.
    pub(crate) async fn send_only(&mut self, cmd: &Cmd) -> Result<()> {
        cmd.check()?;
        self.last_used_at = Instant::now();
        write_command(&mut self.stream, cmd).await
    }

    /// Reads one reply/pushed message, with no timeout: used to await the
    /// next Pub/Sub message, which may arrive at an arbitrary time.
    pub(crate) async fn recv_value(&mut self) -> Result<Value> {
        let v = read_value(&mut self.stream).await?;
        self.last_used_at = Instant::now();
        Ok(v)
    }

    pub(crate) fn idle_for(&self) -> Duration {
        self.last_used_at.elapsed()
    }

    pub(crate) fn protocol(&self) -> u8 {
        self.protocol
    }
}

async fn with_timeout<T>(timeout: Option<Duration>, fut: impl Future<Output = Result<T>>) -> Result<T> {
    match timeout {
        Some(d) => tokio::time::timeout(d, fut)
            .await
            .map_err(|_| Error::Timeout("command timed out".into()))?,
        None => fut.await,
    }
}

async fn write_command<W: AsyncWrite + Unpin>(stream: &mut W, cmd: &Cmd) -> Result<()> {
    let bytes = encode_command(cmd);
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

fn expect_ok(v: Value) -> Result<()> {
    v.into_result().map(|_| ())
}

/// Runs `HELLO 3` (carrying `AUTH`/`SETNAME` when configured), falling
/// back to legacy `AUTH` + `CLIENT SETNAME` when the server rejects
/// `HELLO` (a RESP2 only server) or `opts.resp3` is `false`. `SELECT`
/// runs last either way. Returns the negotiated protocol version.
async fn handshake<S: AsyncBufRead + AsyncWrite + Unpin + Send>(stream: &mut S, opts: &ClientOptions) -> Result<u8> {
    async fn legacy_auth<S: AsyncBufRead + AsyncWrite + Unpin + Send>(stream: &mut S, opts: &ClientOptions) -> Result<()> {
        if let Some(pass) = &opts.password {
            let mut auth = Cmd::new("AUTH");
            if let Some(user) = &opts.username {
                auth = auth.arg(user);
            }
            auth = auth.arg(pass);
            write_command(stream, &auth).await?;
            expect_ok(read_value(stream).await?)?;
        }
        if let Some(name) = &opts.client_name {
            let cmd = Cmd::new("CLIENT").arg("SETNAME").arg(name);
            write_command(stream, &cmd).await?;
            expect_ok(read_value(stream).await?)?;
        }
        Ok(())
    }

    let protocol = if !opts.resp3 {
        // Caller opted out of RESP3 (e.g. to test RESP2 decoding against a
        // server that does support HELLO 3): skip straight to the legacy
        // handshake instead of negotiating and then downgrading.
        legacy_auth(stream, opts).await?;
        2
    } else {
        let mut hello = Cmd::new("HELLO").arg("3");
        if let Some(pass) = &opts.password {
            hello = hello.arg("AUTH").arg(opts.username.as_deref().unwrap_or("default")).arg(pass);
        }
        if let Some(name) = &opts.client_name {
            hello = hello.arg("SETNAME").arg(name);
        }
        write_command(stream, &hello).await?;
        let reply = read_value(stream).await?;

        if matches!(reply, Value::Error(_)) {
            // RESP2-only server (or a HELLO variant it rejects): fall back
            // to legacy AUTH / CLIENT SETNAME.
            legacy_auth(stream, opts).await?;
            2
        } else {
            expect_ok(reply)?;
            3
        }
    };

    if let Some(db) = opts.db {
        let cmd = Cmd::new("SELECT").arg(db);
        write_command(stream, &cmd).await?;
        expect_ok(read_value(stream).await?)?;
    }

    Ok(protocol)
}
