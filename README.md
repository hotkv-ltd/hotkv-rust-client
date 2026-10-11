# HotKV Rust Client

[![CI](https://github.com/hotkv-ltd/hotkv-rust-client/actions/workflows/ci.yml/badge.svg)](https://github.com/hotkv-ltd/hotkv-rust-client/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)

A high-performance Rust client for [HotKV](https://hotkv.com), a RESP2/RESP3-compatible
in-memory data store with built-in AI and LLM-serving features (semantic and prompt
caching, agent memory, RAG, feature store, rate limiting, time series and more).

- Async by default (Tokio), with an optional blocking wrapper.
- RESP2 and RESP3 parsing, including binary safe values.
- Connection pooling, pipelines, transactions (`MULTI`/`EXEC`/`WATCH`), Pub/Sub.
- A cluster client that routes by key and follows `MOVED`/`ASK`.
- TLS support behind a cargo feature.
- Typed methods for the common data structure commands and for the HotKV and AI
  command families listed under [Command coverage](#command-coverage), plus a
  generic `command` method for every other command.

Developed and tested against HotKV 0.3.0.

## Install

```toml
[dependencies]
hotkv-client = "1"

# For TLS (hotkvs:// / rediss://):
hotkv-client = { version = "1", features = ["tls"] }
```

The crate is named `hotkv-client` on crates.io and imported in code as `hotkv`.

MSRV: Rust 1.75 for the default features. The `tls` feature needs Rust 1.85 or
newer with current dependency versions (its dependency tree, `zeroize` among
others, uses edition 2024). Both were checked by building with those toolchains.

## Quick start

```rust
use hotkv::Client;

#[tokio::main]
async fn main() -> hotkv::Result<()> {
    let client = Client::connect("hotkv://127.0.0.1:6379").await?;

    client.set("greeting", "hello").await?;
    let value: Option<String> = client.get("greeting").await?;
    assert_eq!(value.as_deref(), Some("hello"));

    client.hset("user:1", [("name", "Ada"), ("role", "admin")]).await?;
    let name: Option<String> = client.hget("user:1", "name").await?;
    println!("{name:?}");

    Ok(())
}
```

`Client` is a cheap, cloneable handle over a pooled set of connections; clone it
freely and share it across tasks.

## Connecting

`Client::connect` accepts `hotkv://`, `hotkvs://` (TLS), and, for drop-in
migration from Redis clients, `redis://`/`rediss://` URLs:

```text
hotkv://[user[:pass]@]host[:port][/db][?opt=val&...]
```

Recognized query options: `client_name`, `connect_timeout_ms`,
`command_timeout_ms`, `pool_size`, `insecure_skip_verify`.

For full control, build a `ClientOptions` directly:

```rust
use std::time::Duration;
use hotkv::{Client, ClientOptions};

# async fn run() -> hotkv::Result<()> {
let opts = ClientOptions {
    host: "cache.internal".into(),
    port: 6379,
    username: Some("app".into()),
    password: Some("secret".into()),
    db: Some(2),
    client_name: Some("my-service".into()),
    connect_timeout: Duration::from_secs(3),
    command_timeout: Some(Duration::from_secs(5)),
    pool_size: 20,
    ..Default::default()
};
let client = Client::with_options(opts).await?;
# Ok(())
# }
```

A Unix domain socket works the same way: set `unix_socket` (or use a
`unix:///path/to/socket` URL).

### TLS

Enable the `tls` cargo feature (off by default) and set `tls.enabled` (or use
`hotkvs://`/`rediss://`):

```rust
use hotkv::{ClientOptions, TlsOptions};

let opts = ClientOptions {
    host: "cache.example.com".into(),
    tls: TlsOptions {
        enabled: true,
        // ca_file, cert_file, key_file, server_name are all optional.
        ..Default::default()
    },
    ..Default::default()
};
```

Trusted roots default to the bundled Mozilla trust store (`webpki-roots`); set
`ca_file` to trust a specific CA instead, and `cert_file`/`key_file` together for
mutual TLS. `insecure_skip_verify` disables certificate verification entirely and
must never be used against a production server.

### Pooling

Every `Client` owns a bounded connection pool (`ClientOptions::pool_size`, default
10). Connections are dialed lazily, reused when idle, health checked with `PING`
once they have sat idle for a couple of seconds, and dropped once
`ClientOptions::idle_timeout` has passed. `client.pool_stats()` returns a
snapshot of idle/in-use/max connection counts.

Reads issued through the typed command methods (and `Client::command_idempotent`)
are retried once, on a fresh connection, after a connection level failure
(`ClientOptions::max_retries`); writes are never retried automatically.

## Pipelines

Batch several commands into one round trip. Each command's reply is its own
`Result`, so one failing command does not stop the rest:

```rust
use hotkv::args::Cmd;

# async fn run(client: hotkv::Client) -> hotkv::Result<()> {
let mut pipe = client.pipeline();
pipe.queue(Cmd::new("SET").arg("a").arg("1"));
pipe.queue(Cmd::new("INCR").arg("a"));
pipe.queue(Cmd::new("GET").arg("a"));
let results = pipe.execute().await?;
for r in results {
    match r {
        Ok(v) => println!("{v:?}"),
        Err(e) => eprintln!("command failed: {e}"),
    }
}
# Ok(())
# }
```

## Transactions

`MULTI`/`EXEC`/`DISCARD` and `WATCH`/`UNWATCH`, on a dedicated connection held for
the transaction's lifetime:

```rust
use hotkv::args::Cmd;

# async fn run(client: hotkv::Client) -> hotkv::Result<()> {
let mut tx = client.transaction().await?;
tx.watch(["balance"]).await?;
tx.multi().await?;
tx.queue(Cmd::new("DECRBY").arg("balance").arg(10)).await?;
tx.queue(Cmd::new("INCRBY").arg("spent").arg(10)).await?;
match tx.exec().await? {
    Some(_results) => println!("committed"),
    None => println!("aborted: a watched key changed"),
}
# Ok(())
# }
```

## Pub/Sub

```rust
use hotkv::PubSubMessage;

# async fn run(client: hotkv::Client) -> hotkv::Result<()> {
let mut sub = client.subscribe(["news"]).await?;
while let Some(msg) = sub.next().await {
    match msg? {
        PubSubMessage::Message { channel, payload } => {
            println!("{channel}: {}", String::from_utf8_lossy(&payload));
        }
        _ => {}
    }
}
# Ok(())
# }
```

`PubSub` also implements `futures_core::Stream`, so it works with `select!`,
`StreamExt` combinators, and so on. `psubscribe`/`ssubscribe` (pattern and
sharded subscriptions) work the same way.

## Cluster

```rust
use hotkv::{ClientOptions, ClusterClient, ClusterOptions};

# async fn run() -> hotkv::Result<()> {
let cluster = ClusterClient::connect(
    &["10.0.0.1:6379", "10.0.0.2:6379", "10.0.0.3:6379"],
    ClientOptions::default(),
    ClusterOptions::default(),
).await?;

let v = cluster.execute("some-key", hotkv::args::Cmd::new("GET").arg("some-key")).await?;
# let _ = v;
# Ok(())
# }
```

`ClusterClient` discovers the slot map with `CLUSTER SLOTS`, routes each command
by the key's slot (honoring `{hash tag}`), and follows `MOVED` (refreshing the
map) and `ASK` (sending `ASKING` first) redirects, up to
`ClusterOptions::max_redirects`. `execute_readonly` prefers a replica when
`ClusterOptions::read_from_replicas` is set.

## HotKV commands

Typed methods cover the HotKV exclusive commands (`TAG.*`, `NEXPIRE`/`NTTL`/`NEXPIREAT`,
`HOTKEYS`, `GCRA.*`, `VSIM.*`) and the enterprise families `RATELIMIT.*`,
`TS.*`, `FEATURE.*`, `OBSERVE`/`ALERTS` and the AI commands
below. Small families are plain `Client` methods; larger ones are grouped behind an
accessor:

```rust
# async fn run(client: hotkv::Client) -> hotkv::Result<()> {
client.tag().set("doc:1", ["draft", "urgent"]).await?;
let urgent_drafts = client.tag().inter(["draft", "urgent"]).await?;

client.nexpire("doc:1", 60_000_000_000).await?; // nanosecond TTL
let decision = client.gcra().limit("api:user42", 1000, 10, None).await?;

client.vsim().create_index("docs", 384, Some("cosine"), None, None, None).await?;
let results = client.vsim().search("docs", 5, &[0.1; 384], &[], None).await?;

let status = client.ratelimit().check("signups", "203.0.113.4", None).await?;
client.ts().add("cpu.load", "*", 0.42).await?;
client.feature().set("users", "u42", [("age", "30")]).await?;
# let _ = urgent_drafts;
# let _ = decision;
# let _ = results;
# let _ = status;
# Ok(())
# }
```

See the module docs for `hotkv::commands` and `hotkv::cluster::key_slot` for the
full list, and `hotkv::ai` for the AI command groups below.

### Command coverage

Counting each subcommand as its own command, the server (0.3.0) has 444 core
commands and 177 enterprise (licensed) commands. This crate has a typed method for
most of them. Every other command goes through the generic method.

Core commands and families with no typed method:

- `ACL`, `BF.*`, `CMS.*`, `TOPK.*`, `COMMAND`, `DEBUG`, `LATENCY`, `MEMORY`,
  `MODULE`, `SLOWLOG`, `FUNCTION`/`FCALL`, and the `CLUSTER` commands (the cluster
  client uses `CLUSTER SLOTS` internally).
- `CLIENT`, except `CLIENT ID`, `GETNAME` and `SETNAME`: no typed `CLIENT TRACKING`,
  `CACHING`, `GETREDIR`, `TRACKINGINFO`, `KILL`, `LIST`, `PAUSE` and so on.
- `CONFIG`, except `GET` and `SET`; `OBJECT`, except `ENCODING`.
- `PUBLISH`, `SPUBLISH` and `PUBSUB`; the blocking list and sorted set commands
  (`BLPOP`, `BRPOP`, `BLMOVE`, `BLMPOP`, `BZPOPMIN`, `BZPOPMAX`, `BZMPOP`,
  `BRPOPLPUSH`); `SORT`, `LPOS`, `LCS`, `LMPOP`, `ZINTER`, `ZUNION`, `ZDIFF`,
  `ZINTERCARD`, `ZMPOP`, `SINTERCARD`, `BITFIELD`, `GEORADIUS*`, `GEOHASH`,
  `GEOSEARCHSTORE`, `EXPIRETIME`/`PEXPIRETIME`, `SWAPDB`, `MOVE`.
- The newer additions `HGETDEL`, `HGETEX`, `HSETEX`, `MSETEX`, `DELEX`, `DIGEST`,
  `HIMPORT`, `XCFGSET`, `XSETID`, `SFLUSH`, `CLUSTERSCAN` and `TRIMSLOTS`.
- Server administration: `SAVE`, `BGSAVE`, `SHUTDOWN`, `REPLICAOF`, `FAILOVER`,
  `FLUSHALL`, `MONITOR`, `WAIT`, `MIGRATE`.

Enterprise commands with no typed method: `TENANT.*`, `IAM.*`, `AUDIT.*`,
`ENCRYPTION.*`, `REPLICATION.*`, and the internal `*.SYNC` and `PSYNC` commands.
They work through the generic method.

Any of them is one call away with the generic method, and the reply comes back as a
`Value` to convert with `FromValue`:

```rust
use hotkv::args::Cmd;
use hotkv::FromValue;

# async fn run(client: hotkv::Client) -> hotkv::Result<()> {
let old = client.execute("HGETDEL", ["user:1", "FIELDS", "1", "name"]).await?;
let published = i64::from_value(client.execute("PUBLISH", ["news", "hello"]).await?)?;
client.command(Cmd::new("BF.RESERVE").arg("seen").arg(0.01).arg(10_000)).await?;
# let _ = (old, published);
# Ok(())
# }
```

Use a client with `pool_size` 1 (or a `Transaction`) for commands that change the
state of one connection, such as `CLIENT TRACKING`. RESP3 (the default) replies some
commands with booleans, doubles and maps where RESP2 uses integers, bulk strings and
flat arrays; set `ClientOptions::resp3` to `false` for the RESP2 shapes.

### Licensed commands

Enterprise commands (everything under `hotkv::ai` plus `RATELIMIT.*`, `TS.*`,
`FEATURE.*`, `OBSERVE`/`ALERTS`, `VSIM.SAVE`/`LOAD`/`LIST`/`DROP`) need a server that
has a license for them. Without one the server replies with a `NOLICENSE` error,
which this crate returns as `Error::NotLicensed` (`Error::is_not_licensed`). The core
data store works either way.

## AI caching

`client.pcache()`, `client.scache()`, `client.llmstats()`, `client.llmbudget()`,
`client.embed_dedup()`, `client.prompt()`, `client.amem()`, `client.rag()`, and the
keyed caches (`client.tool_cache()`, `client.ncache()`, `client.guard()`,
`client.rerank()`) wrap the commands of those families directly. On top of them,
`hotkv::ai` provides two idiomatic get-or-call caches.

Entries and sessions that take a time to live also take an absolute expiry: the
`set_with_expiry` methods of the semantic, prompt and keyed caches, and
`AmemCommands::conv_create_with_expiry` and `work_set_with_expiry`, accept
`hotkv::ai::Expiry::Ttl(seconds)` or `Expiry::Pxat(unix_milliseconds)` (the server
takes one or the other, never both). The `set` methods keep the `ttl_seconds`
argument.

### PromptCache: only call the LLM on a cache miss

`PromptCache` builds a deterministic cache key from the provider, model, system
prompt, messages and generation parameters (see [`hotkv::ai::prompt_cache_key`]
for the exact canonical form, byte-identical across every official HotKV SDK),
and calls your function only when that key misses. Give the returned
[`LlmUsage`] a `model` and its call reports token/cost savings on every later
hit (see `PcacheCommands::stats`' `usage_saved` field):

```rust
use hotkv::ai::{LlmCallResult, LlmUsage, PromptCache, PromptCacheKeyInput, PromptMessage};

# async fn run(client: hotkv::Client) -> hotkv::Result<()> {
let cache = PromptCache::new(client, "chat-completions", Some(3600))
    .with_stats_bucket("chat-completions"); // optional: LLMSTATS.RECORD accounting
cache.create_namespace(None, Some(0.002)).await?;

let input = PromptCacheKeyInput {
    provider: "anthropic".to_string(),
    model: "claude-sonnet-5".to_string(),
    system: "Answer tersely.".to_string(),
    messages: vec![PromptMessage::new("user", "What is 2 + 2?")],
    params: vec![("temperature".to_string(), "0".to_string())],
};

let (response, hit) = cache
    .get_or_call(&input, || async {
        // This closure only runs on a cache miss: replace it with your
        // actual provider call (any provider's SDK works, HotKV never
        // depends on one).
        Ok(LlmCallResult {
            response: "4".to_string(),
            usage: LlmUsage {
                model: "claude-sonnet-5".to_string(), // required for usage to be stored
                input_tokens: 12,
                output_tokens: 1,
                cost_usd: 0.0001,
                latency_ms: 40,
                ..Default::default()
            },
        })
    })
    .await?;
println!("{response} (cache hit: {hit})");

// Every later hit against the same key attributes the miss's usage here.
let stats = cache.stats().await?;
println!("saved {} input tokens, ${:.6} so far", stats.usage_saved.input_tokens, stats.usage_saved.cost_usd);
# Ok(())
# }
```

### SemanticCache: match on meaning, not exact text

`SemanticCache` looks up the nearest cached answer to a caller supplied embedding
vector above a similarity threshold; it never computes embeddings itself, so it
works with any embedding provider. Its `get_or_call` closure returns the same
`LlmCallResult` as `PromptCache`, so a hit's savings show up in `stats()` too:

```rust
use hotkv::ai::{LlmCallResult, LlmUsage, SemanticCache};

# async fn run(client: hotkv::Client, embed: impl Fn(&str) -> Vec<f64>) -> hotkv::Result<()> {
let cache = SemanticCache::new(client, "faq", Some(86400));
cache.create_namespace(1536, Some("cosine"), Some(0.9)).await?;

let query = "What is your return policy?";
let vector = embed(query);
let (answer, hit) = cache
    .get_or_call(query, &vector, || async {
        Ok(LlmCallResult {
            response: "Returns are accepted within 30 days.".to_string(),
            usage: LlmUsage { model: "gpt-6-sol".to_string(), input_tokens: 24, output_tokens: 9, ..Default::default() },
        })
    })
    .await?;
println!("{answer} (cache hit: {hit})");
# Ok(())
# }
```

### LLM cost tracking

`LLMSTATS.RECORD` with a `MODEL` prices a call from the server's price table
instead of a caller-supplied `cost_usd`; `LLMSTATS.PRICE SET` teaches the
server a model's price (or overrides a built-in one), and `LLMBUDGET.CONSUME`
with `MODEL` enforces a token/cost cap priced the same way:

```rust
use hotkv::ai::{LlmPriceOptions, LlmStatsRecordOptions, Usage};

# async fn run(client: hotkv::Client) -> hotkv::Result<()> {
// Teach the server a custom (or overridden) model price, USD per 1M tokens.
// Omitted cache prices default to 0.1x/1.25x/2x the input price.
let price_opts = LlmPriceOptions { cache_read_usd_per_mtok: Some(0.3), ..Default::default() };
client.llmstats().price_set("claude-sonnet-5", 3.0, 15.0, Some(&price_opts)).await?;

client.llmstats().bucket("chat-completions").await?;
let record_opts = LlmStatsRecordOptions { model: Some("claude-sonnet-5".to_string()), ..Default::default() };
// cost_usd (here 0.0) is ignored once MODEL is given: the server prices it.
client.llmstats().record("chat-completions", 1200, 340, 0, 0.0, 610, Some(&record_opts)).await?;
let stats = client.llmstats().get("chat-completions").await?;
println!("spent ${} so far", stats.cost_usd);

client.llmbudget().set("chat-completions-daily", 1_000_000, 25.0, 86400).await?;
let usage = Usage::new("claude-sonnet-5").input(1200).output(340);
let allowed = client.llmbudget().consume("chat-completions-daily", 0, 0.0, Some(&usage)).await?;
println!("request allowed: {allowed}");
# Ok(())
# }
```

Both caches treat a `NOLICENSE` reply (the `semantic-cache`/`prompt-cache`
license feature not enabled on the connected server) as "the cache is
unavailable", not as a call failure, and fall through to calling your function.

## Error handling

Every fallible call returns `hotkv::Result<T>` (`Result<T, hotkv::Error>`).
`Error` distinguishes I/O, timeout, protocol, server reply, licensing and
invalid argument errors:

```rust
# async fn run(client: hotkv::Client) -> hotkv::Result<()> {
match client.pcache().get("chat-completions", "some-prompt").await {
    Ok(Some(response)) => println!("hit: {response}"),
    Ok(None) => println!("miss"),
    Err(e) if e.is_not_licensed() => println!("prompt cache not enabled on this server"),
    Err(e) => return Err(e),
}
# Ok(())
# }
```

`Error::server_error()` exposes the parsed `ServerError` (an error `code` like
`WRONGTYPE`, `NOAUTH`, `MOVED`, `NOLICENSE`, plus the message), for callers that
need to branch on specific server error codes. A reply with no leading code token
(`-TopK: key already exists`) has the placeholder code `ERR` and is shown exactly as
the server sent it.

## Numbers and floats

Every floating point argument is written as a plain decimal: digits, an optional
`.`, an optional leading `-`, never an exponent, never a leading `+`, and the
shortest digits that parse back to the same value (`1e-5` is sent as `0.00001`,
`1e21` as `1000000000000000000000`, `-0.0` as `0`). The formatting lives in
`hotkv::numfmt` and applies to the typed methods and to `f64` arguments given to
`Cmd::arg`.

Values the server cannot accept are refused before anything is sent, with
`Error::InvalidArgument`:

- NaN and infinity are refused everywhere, except sorted set scores, weights and
  score bounds, which take `inf` and `-inf` (wrap a bound in `hotkv::args::Score`
  to pass an infinity as a number; `"-inf"` as a string also works).
- Vector components and similarity thresholds are narrowed to the 32 bit float the
  server stores, and a value outside the `f32` range is refused.
- USD amounts for `LLMBUDGET.SET` and `LLMSTATS.PRICE SET` are rounded to at most
  six fractional digits (the server rejects more) and must be non-negative and fit
  in `u64` millionths. Costs for `LLMBUDGET.CONSUME`, `LLMSTATS.RECORD` and
  `PCACHE.CREATE` must be finite and non-negative.

Core commands were verified against a live server with tiny (`1e-300`), huge
(`f64::MAX`) and infinite values, and the enterprise ones against a licensed server:
`LLMBUDGET.SET` limits of `1e-5`, `0.1 + 0.2`, `25` and `1e12` are stored as
`0.00001`, `0.3`, `25` and `1000000000000`, and the budget enforces the limit to the
micro-dollar. On an unlicensed server those tests skip and only the exact arguments
sent are tested (see Testing).

## Binary safety

`GET`/`DUMP`/similar have a `_bytes` counterpart (`get_bytes`) that returns raw
`Vec<u8>` instead of `String`, for values that may not be valid UTF-8. Anything
without a dedicated `_bytes` method is still reachable through the generic
escape hatch: `client.command(...).await?.into_bytes()`.

## The escape hatch

`Client::command`/`Client::execute` send any command this crate does not (yet)
wrap:

```rust
use hotkv::args::Cmd;

# async fn run(client: hotkv::Client) -> hotkv::Result<()> {
let value = client.command(Cmd::new("OBJECT").arg("ENCODING").arg("mykey")).await?;
let value2 = client.execute("DEBUG", ["SLEEP", "0"]).await?;
# let _ = value; let _ = value2;
# Ok(())
# }
```

## Blocking API

`hotkv::blocking::BlockingClient` wraps the async client for callers outside a
Tokio runtime; each instance owns its own small runtime and blocks the calling
thread for each call. It is not safe to share across threads without a mutex.

```rust
use hotkv::blocking::BlockingClient;

fn run() -> hotkv::Result<()> {
    let client = BlockingClient::connect("hotkv://127.0.0.1:6379")?;
    client.set("key", "value")?;
    println!("{:?}", client.get("key")?);

    // Anything without a dedicated method is one call away:
    client.block_on(client.async_client().scache().stats("faq"))?;
    Ok(())
}
```

## Testing

Unit tests, doc tests and `tests/wire_args.rs` need no server. `wire_args.rs` calls
every enterprise command wrapper against an in-process server that records what it
receives and compares the arguments with the server's grammar:

```sh
cargo test --lib
cargo test --doc
cargo test --test wire_args
```

Integration tests run against a live HotKV server, gated on the `HOTKV_TEST_URL`
environment variable (skipped, with a printed reason, when it is unset):

```sh
export HOTKV_TEST_URL=hotkv://127.0.0.1:6379
cargo test
```

The tests use database 0 with unique key names, and a few run `FLUSHDB` on databases 9
to 11 over a private connection. Do not point the tests at a server whose data you
need.

Enterprise commands (prompt cache, semantic cache, LLM stats and budget, prompt
registry, the keyed caches, embedding dedup, agent memory, RAG, rate limiting, time
series, feature store, observability, vector snapshots) answer `NOLICENSE` on an
unlicensed server. Against one, their live tests skip with a printed reason and only
`wire_args.rs` checks them. Set `HOTKV_TEST_LICENSED=1` when the server carries an
enterprise license: a missing license then fails the test instead of skipping it. All
typed enterprise methods have live tests (`ai_cache.rs`, `ai_memory.rs`, `enterprise.rs`,
`licensed.rs`) that pass against a licensed HotKV 0.3.0 server.

More environment variables enable the remaining suites, each skipped when its
variable is unset:

| Variable | Enables |
| --- | --- |
| `HOTKV_TEST_LICENSED` | declares the server licensed, so `NOLICENSE` fails the enterprise tests |
| `HOTKV_TEST_AUTH_ADDR`, `HOTKV_TEST_AUTH_PASSWORD`, `HOTKV_TEST_AUTH_USER`, `HOTKV_TEST_AUTH_USER_PASSWORD` | `tests/auth.rs`: a server with `requirepass` and an ACL user limited to `sdk:*` keys |
| `HOTKV_TEST_TLS_DIR`, `HOTKV_TEST_TLS_MTLS_PORT`, `HOTKV_TEST_TLS_PW_PORT`, `HOTKV_TEST_TLS_PASSWORD` | `tests/tls.rs` (build with `--features tls`): a mutual TLS server and a TLS server with a password |
| `HOTKV_TEST_CLUSTER_SEEDS` | `tests/cluster.rs`: a cluster of three masters with a replica each |
| `HOTKV_TEST_HOTKEYS_URL` | the strict `HOTKEYS` test: a server started with `hotkey-tracking yes` |

Clippy runs in CI with warnings as errors for the default and the `tls` features.

## Contributing

Bug reports and pull requests are welcome. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Support

- Bugs and feature requests: [GitHub issues](https://github.com/hotkv-ltd/hotkv-rust-client/issues)
- Questions and commercial support: support@hotkv.com
- Security vulnerabilities: see [SECURITY.md](SECURITY.md)

## License

Licensed under the Apache License, Version 2.0: see [LICENSE](LICENSE) and [NOTICE](NOTICE).

## Trademarks and affiliation

"HotKV" is a trademark of HotKV Ltd; the license does not grant rights to use it.
The HotKV server is a separate commercial product, and its enterprise commands need a HotKV license.

HotKV is an independent product of HotKV Ltd. It speaks the RESP protocol and implements
many Redis commands so that existing tools and client habits carry over, but it is not Redis,
Valkey, Dragonfly or KeyDB, and this SDK is built for and tested against HotKV. HotKV Ltd is not
affiliated with, endorsed by or sponsored by Redis Ltd., the Valkey project, DragonflyDB or KeyDB.
Redis is a registered trademark of Redis Ltd. Valkey, Dragonfly, KeyDB and all other product and
company names are trademarks of their respective owners; they are used here only to describe
protocol and command compatibility.
