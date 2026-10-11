// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! `ClusterClient` redirect handling against scripted in-process nodes (no
//! HotKV server or cluster needed): `MOVED`, `ASK` with `ASKING` on the same
//! connection as the command, `READONLY` for replica reads, and the redirect
//! limit.

use std::sync::{Arc, Mutex};

use hotkv::args::Cmd;
use hotkv::{ClientOptions, ClusterClient, ClusterOptions, Error, FromValue};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

/// (connection number, command) in arrival order.
type Log = Arc<Mutex<Vec<(usize, Vec<String>)>>>;

type Script = Arc<dyn Fn(&[String]) -> String + Send + Sync>;

struct FakeNode {
    port: u16,
    log: Log,
}

impl FakeNode {
    fn commands(&self) -> Vec<(usize, Vec<String>)> {
        self.log.lock().unwrap().clone()
    }
    fn names(&self) -> Vec<String> {
        self.commands().into_iter().map(|(_, a)| a[0].to_uppercase()).collect()
    }
}

async fn read_command<R: AsyncBufReadExt + AsyncReadExt + Unpin>(r: &mut R) -> Option<Vec<String>> {
    let mut line = String::new();
    if r.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let n: usize = line.trim_end().strip_prefix('*')?.parse().ok()?;
    let mut out = Vec::new();
    for _ in 0..n {
        line.clear();
        r.read_line(&mut line).await.ok()?;
        let len: usize = line.trim_end().strip_prefix('$')?.parse().ok()?;
        let mut buf = vec![0u8; len + 2];
        r.read_exact(&mut buf).await.ok()?;
        buf.truncate(len);
        out.push(String::from_utf8_lossy(&buf).into_owned());
    }
    Some(out)
}

/// Binds a node on a free port before its script is known, so the scripts can
/// name each other's ports.
async fn bind() -> (TcpListener, u16) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = l.local_addr().unwrap().port();
    (l, p)
}

fn serve(listener: TcpListener, port: u16, script: Script) -> FakeNode {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let shared = log.clone();
    tokio::spawn(async move {
        let mut conn_no = 0usize;
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            conn_no += 1;
            let (shared, script, id) = (shared.clone(), script.clone(), conn_no);
            tokio::spawn(async move {
                let (rd, mut wr) = stream.into_split();
                let mut rd = BufReader::new(rd);
                while let Some(argv) = read_command(&mut rd).await {
                    let reply = if argv[0].eq_ignore_ascii_case("HELLO") {
                        "-ERR unknown command 'HELLO'\r\n".to_string()
                    } else {
                        shared.lock().unwrap().push((id, argv.clone()));
                        script(&argv)
                    };
                    if wr.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    FakeNode { port, log }
}

/// `CLUSTER SLOTS` for one range 0-16383 owned by `master`, optionally with a replica.
fn slots(master: u16, replica: Option<u16>) -> String {
    let node = |p: u16| format!("*3\r\n$9\r\n127.0.0.1\r\n:{p}\r\n$40\r\n{:0>40}\r\n", p);
    let mut out = format!("*1\r\n*{}\r\n:0\r\n:16383\r\n{}", 3 + replica.is_some() as usize, node(master));
    if let Some(r) = replica {
        out.push_str(&node(r));
    }
    out
}

fn is(argv: &[String], name: &str) -> bool {
    argv[0].eq_ignore_ascii_case(name) || (argv[0].eq_ignore_ascii_case("CLUSTER") && argv.get(1).is_some_and(|s| s.eq_ignore_ascii_case(name)))
}

async fn connect(seed: u16, opts: ClusterOptions) -> ClusterClient {
    let addr = format!("127.0.0.1:{seed}");
    ClusterClient::connect(&[addr.as_str()], ClientOptions::default(), opts).await.unwrap()
}

#[tokio::test]
async fn moved_refreshes_the_slot_map_and_retries_on_the_new_owner() {
    let (la, pa) = bind().await;
    let (lb, pb) = bind().await;
    // A owns everything in the first map, then reports B as the owner.
    let a_slots_calls = Arc::new(Mutex::new(0));
    let calls = a_slots_calls.clone();
    let a = serve(la, pa, Arc::new(move |argv: &[String]| {
        if is(argv, "SLOTS") {
            let mut n = calls.lock().unwrap();
            *n += 1;
            return slots(if *n == 1 { pa } else { pb }, None);
        }
        format!("-MOVED 12182 127.0.0.1:{pb}\r\n")
    }));
    let b = serve(lb, pb, Arc::new(|argv: &[String]| if is(argv, "SLOTS") { slots(0, None) } else { "$5\r\nvalue\r\n".to_string() }));

    let cluster = connect(a.port, ClusterOptions::default()).await;
    let got = cluster.execute("foo", Cmd::new("GET").arg("foo")).await.unwrap();
    assert_eq!(String::from_value(got).unwrap(), "value");
    assert_eq!(a.names(), vec!["CLUSTER", "GET", "CLUSTER"], "A: slots, the redirected GET, then a refresh");
    assert_eq!(b.names(), vec!["GET"]);
    // The next command goes straight to the new owner.
    cluster.execute("foo", Cmd::new("GET").arg("foo")).await.unwrap();
    assert_eq!(a.names().len(), 3);
    assert_eq!(b.names(), vec!["GET", "GET"]);
}

#[tokio::test]
async fn ask_sends_asking_and_the_command_on_one_connection() {
    let (la, pa) = bind().await;
    let (lb, pb) = bind().await;
    let a = serve(la, pa, Arc::new(move |argv: &[String]| {
        if is(argv, "SLOTS") { slots(pa, None) } else { format!("-ASK 12182 127.0.0.1:{pb}\r\n") }
    }));
    let b = serve(lb, pb, Arc::new(|argv: &[String]| {
        if argv[0].eq_ignore_ascii_case("ASKING") { "+OK\r\n".into() } else { "$3\r\nask\r\n".into() }
    }));

    let cluster = connect(a.port, ClusterOptions::default()).await;
    let got = cluster.execute("foo", Cmd::new("GET").arg("foo")).await.unwrap();
    assert_eq!(String::from_value(got).unwrap(), "ask");
    let seen = b.commands();
    assert_eq!(seen.iter().map(|(_, a)| a[0].to_uppercase()).collect::<Vec<_>>(), vec!["ASKING", "GET"]);
    assert_eq!(seen[0].0, seen[1].0, "ASKING and the command must share a connection");
    // ASK does not change the slot map: A is asked first again.
    cluster.execute("foo", Cmd::new("GET").arg("foo")).await.unwrap();
    assert_eq!(a.names().iter().filter(|n| *n == "GET").count(), 2);
    assert_eq!(a.names().iter().filter(|n| *n == "CLUSTER").count(), 1, "no slot refresh for ASK");
}

#[tokio::test]
async fn replica_reads_send_readonly_with_the_command() {
    let (la, pa) = bind().await;
    let (lb, pb) = bind().await;
    let a = serve(la, pa, Arc::new(move |argv: &[String]| if is(argv, "SLOTS") { slots(pa, Some(pb)) } else { "$6\r\nmaster\r\n".into() }));
    let b = serve(lb, pb, Arc::new(|argv: &[String]| if argv[0].eq_ignore_ascii_case("READONLY") { "+OK\r\n".into() } else { "$7\r\nreplica\r\n".into() }));

    let cluster = connect(a.port, ClusterOptions { read_from_replicas: true, ..Default::default() }).await;
    for _ in 0..3 {
        let got = cluster.execute_readonly("foo", Cmd::new("GET").arg("foo")).await.unwrap();
        assert_eq!(String::from_value(got).unwrap(), "replica");
    }
    // Writes and plain execute still go to the master.
    let got = cluster.execute("foo", Cmd::new("GET").arg("foo")).await.unwrap();
    assert_eq!(String::from_value(got).unwrap(), "master");
    let seen = b.commands();
    assert_eq!(seen.len(), 6, "{seen:?}");
    for pair in seen.chunks(2) {
        assert_eq!(pair[0].1[0].to_uppercase(), "READONLY");
        assert_eq!(pair[1].1[0].to_uppercase(), "GET");
        assert_eq!(pair[0].0, pair[1].0, "READONLY and GET share a connection");
    }
    assert_eq!(a.names().iter().filter(|n| *n == "GET").count(), 1);
}

#[tokio::test]
async fn redirect_loops_stop_at_max_redirects() {
    let (la, pa) = bind().await;
    let (lb, pb) = bind().await;
    let a = serve(la, pa, Arc::new(move |argv: &[String]| if is(argv, "SLOTS") { slots(pa, None) } else { format!("-MOVED 1 127.0.0.1:{pb}\r\n") }));
    let b = serve(lb, pb, Arc::new(move |_: &[String]| format!("-MOVED 1 127.0.0.1:{pa}\r\n")));
    let cluster = connect(a.port, ClusterOptions { max_redirects: 3, ..Default::default() }).await;
    let err = cluster.execute("foo", Cmd::new("GET").arg("foo")).await.unwrap_err();
    assert!(matches!(err, Error::TooManyRedirects), "{err}");
    let gets = |n: &FakeNode| n.names().iter().filter(|c| *c == "GET").count();
    assert_eq!(gets(&a) + gets(&b), 4, "the first attempt plus three redirects");
}

#[tokio::test]
async fn other_server_errors_are_returned_unchanged() {
    let (la, pa) = bind().await;
    let a = serve(la, pa, Arc::new(move |argv: &[String]| if is(argv, "SLOTS") { slots(pa, None) } else { "-CROSSSLOT Keys in request don't hash to the same slot\r\n".into() }));
    let cluster = connect(a.port, ClusterOptions::default()).await;
    let err = cluster.execute("a", Cmd::new("MGET").arg("a").arg("b")).await.unwrap_err();
    assert!(err.server_error().is_some_and(|e| e.is_code("CROSSSLOT")), "{err}");
    assert_eq!(a.names(), vec!["CLUSTER", "MGET"], "no retry");
}
