// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use hotkv::ai::{LlmCallResult, LlmPriceOptions, LlmStatsRecordOptions, LlmUsage, PromptCache, PromptCacheKeyInput, PromptMessage, SemanticCache, Usage};

#[tokio::test]
async fn scache_commands() {
    let Some(client) = common::connect().await else { return };
    let ns = common::unique("scache");
    let sc = client.scache();

    let result = sc.create(&ns, 3, Some("cosine"), Some(0.8), None).await;
    let () = skip_if_not_licensed!(result, "semantic-cache");
    let usage = Usage::new("claude-sonnet-5").input(20).output(5);
    sc.set(&ns, "what is the capital of France?", "Paris", &[1.0, 0.0, 0.0], None, Some(&usage)).await.unwrap();

    let hit = sc.get(&ns, &[1.0, 0.0, 0.01], None).await.unwrap();
    let (response, similarity) = hit.expect("similar enough to hit");
    assert_eq!(response, "Paris");
    assert!(similarity > 0.8);

    let miss = sc.get(&ns, &[0.0, 1.0, 0.0], None).await.unwrap();
    assert!(miss.is_none());

    let stats = sc.stats(&ns).await.unwrap();
    assert_eq!(stats.entries, 1);
    assert!(stats.hits >= 1);
    // The hit above attributed the usage recorded on SET.
    assert_eq!(stats.usage_saved.requests, stats.hits);
    assert_eq!(stats.usage_saved.input_tokens, 20 * stats.hits);

    sc.set_threshold(&ns, 0.5).await.unwrap();
    assert!(sc.list().await.unwrap().contains(&ns));
    assert!(sc.flush(&ns).await.unwrap());
    assert!(sc.drop(&ns).await.unwrap());
}

#[tokio::test]
async fn pcache_commands() {
    let Some(client) = common::connect().await else { return };
    let ns = common::unique("pcache");
    let pc = client.pcache();

    let result = pc.create(&ns, Some(1000), None, Some(0.002)).await;
    let () = skip_if_not_licensed!(result, "prompt-cache");

    let usage = Usage::new("gpt-6-sol").input(30).output(10);
    pc.set(&ns, "prompt1", "response1", None, Some("{\"model\":\"test\"}"), Some(&usage)).await.unwrap();
    assert_eq!(pc.get(&ns, "prompt1").await.unwrap(), Some("response1".to_string()));
    assert_eq!(pc.get(&ns, "prompt-missing").await.unwrap(), None);

    let entry = pc.getex(&ns, "prompt1").await.unwrap().expect("entry present");
    assert_eq!(entry.response, "response1");
    assert!(entry.hit_count >= 1);

    let stats = pc.stats(&ns).await.unwrap();
    assert_eq!(stats.entries, 1);
    // GET and GETEX above both hit, each attributing the SET's usage.
    assert_eq!(stats.usage_saved.requests, 2);
    assert_eq!(stats.usage_saved.input_tokens, 60);
    assert_eq!(stats.usage_saved.output_tokens, 20);

    pc.config_set(&ns, "TTL", 3600).await.unwrap();
    assert!(pc.list().await.unwrap().contains(&ns));
    assert!(pc.del(&ns, "prompt1").await.unwrap());
    pc.drop(&ns).await.unwrap();
}

#[tokio::test]
async fn keyed_caches() {
    let Some(client) = common::connect().await else { return };

    type Accessor = fn(&hotkv::Client) -> hotkv::ai::KeyedCacheCommands;
    let engines: [(&str, Accessor); 4] = [
        ("tool_cache", hotkv::Client::tool_cache),
        ("ncache", hotkv::Client::ncache),
        ("guard", hotkv::Client::guard),
        ("rerank", hotkv::Client::rerank),
    ];
    for (label, accessor) in engines {
        let ns = common::unique(label);
        let cache = accessor(&client);
        let result = cache.create(&ns, Some(100), None).await;
        let () = skip_if_not_licensed!(result, label);

        let usage = Usage::new("claude-sonnet-5").input(8);
        cache.set(&ns, "k1", "v1", None, None, Some(&usage)).await.unwrap();
        assert_eq!(cache.get(&ns, "k1").await.unwrap(), Some("v1".to_string()), "{label}");
        let stats = cache.stats(&ns).await.unwrap();
        assert_eq!(stats.entries, 1, "{label}");
        assert_eq!(stats.usage_saved.requests, 1, "{label}");
        assert_eq!(stats.usage_saved.input_tokens, 8, "{label}");
        assert!(cache.list().await.unwrap().contains(&ns), "{label}");
        assert!(cache.del(&ns, "k1").await.unwrap(), "{label}");
        cache.drop(&ns).await.unwrap();
    }
}

#[tokio::test]
async fn llmstats_and_llmbudget() {
    let Some(client) = common::connect().await else { return };
    let bucket = common::unique("llmstats");
    let ls = client.llmstats();

    let result = ls.bucket(&bucket).await;
    let () = skip_if_not_licensed!(result, "llm-stats");
    ls.record(&bucket, 100, 50, 0, 0.002, 250, None).await.unwrap();
    let error_opts = LlmStatsRecordOptions { error: Some(false), ..Default::default() };
    ls.record(&bucket, 10, 5, 15, 0.0, 5, Some(&error_opts)).await.unwrap();
    // A MODEL-priced record: cost_usd is ignored (0 is sent) and the
    // server prices it from the table instead.
    let priced_opts = LlmStatsRecordOptions { model: Some("claude-sonnet-5".to_string()), ..Default::default() };
    ls.record(&bucket, 1000, 200, 0, 0.0, 300, Some(&priced_opts)).await.unwrap();

    let stats = ls.get(&bucket).await.unwrap();
    assert_eq!(stats.requests, 3);
    assert_eq!(stats.input_tokens, 1110);
    assert_eq!(stats.cached_tokens, 15);
    assert!(ls.list().await.unwrap().contains(&bucket));

    // LLMSTATS.PRICE: set a custom price, read it back, list it, delete it.
    let price_opts = LlmPriceOptions { cache_read_usd_per_mtok: Some(0.3), ..Default::default() };
    ls.price_set("claude-sonnet-5", 3.0, 15.0, Some(&price_opts)).await.unwrap();
    let price = ls.price_get("claude-sonnet-5").await.unwrap();
    assert_eq!(price.source, "custom");
    assert!(price.priced);
    assert_eq!(price.input_usd_per_mtok, 3.0);
    assert_eq!(price.cache_read_usd_per_mtok, 0.3);
    assert!(ls.price_list().await.unwrap().iter().any(|p| p.model == "claude-sonnet-5"));
    assert!(ls.price_del("claude-sonnet-5").await.unwrap());
    assert!(!ls.price_del("claude-sonnet-5").await.unwrap(), "already deleted");
    // An unknown model is unpriced, not an error.
    let unknown = ls.price_get("no-such-model-xyz").await.unwrap();
    assert_eq!(unknown.source, "unpriced");
    assert!(!unknown.priced);

    assert!(ls.reset(&bucket).await.unwrap());
    ls.drop(&bucket).await.unwrap();

    let budget_name = common::unique("llmbudget");
    let lb = client.llmbudget();
    let result = lb.set(&budget_name, 1000, 1.0, 3600).await;
    let () = skip_if_not_licensed!(result, "llm-budget");

    assert!(lb.consume(&budget_name, 100, 0.1, None).await.unwrap());
    // A MODEL-priced consume: tokens/cost_usd are ignored (0, 0.0 sent);
    // the token count is the sum of usage's counts, priced server side.
    let usage = Usage::new("claude-sonnet-5").input(50).output(10);
    assert!(lb.consume(&budget_name, 0, 0.0, Some(&usage)).await.unwrap());

    let status = lb.status(&budget_name).await.unwrap();
    assert_eq!(status.tokens_used, 160);
    assert!(lb.list().await.unwrap().contains(&budget_name));
    assert!(lb.reset(&budget_name).await.unwrap());
    lb.drop(&budget_name).await.unwrap();
}

/// The AI caching example from the README: an LLM call happens only on a
/// cache miss.
#[tokio::test]
async fn prompt_cache_calls_llm_only_on_miss() {
    let Some(client) = common::connect().await else { return };
    let ns = common::unique("promptcache");
    let cache = PromptCache::new(client.clone(), &ns, Some(3600));

    if let Err(e) = cache.create_namespace(None, None).await {
        if e.is_not_licensed() {
            common::unlicensed("prompt_cache_calls_llm_only_on_miss", &e);
            return;
        }
        panic!("unexpected error: {e}");
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let input = PromptCacheKeyInput {
        provider: "anthropic".to_string(),
        model: "claude-sonnet-5".to_string(),
        system: "Answer tersely.".to_string(),
        messages: vec![PromptMessage::new("user", "What is 2 + 2?")],
        params: vec![("temperature".to_string(), "0".to_string())],
    };

    let simulate_llm_call = |calls: Arc<AtomicUsize>| async move {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmCallResult {
            response: "4".to_string(),
            usage: LlmUsage { model: "claude-sonnet-5".to_string(), input_tokens: 12, output_tokens: 1, cost_usd: 0.0001, latency_ms: 40, ..Default::default() },
        })
    };

    let (r1, hit1) = cache.get_or_call(&input, || simulate_llm_call(calls.clone())).await.unwrap();
    assert_eq!(r1, "4");
    assert!(!hit1, "first call must be a miss");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let (r2, hit2) = cache.get_or_call(&input, || simulate_llm_call(calls.clone())).await.unwrap();
    assert_eq!(r2, "4");
    assert!(hit2, "second call with the same input must hit the cache");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the LLM must not be called again on a cache hit");

    // The miss stored the model's usage with the entry, so the hit
    // above attributed it as savings.
    let stats = cache.stats().await.unwrap();
    assert_eq!(stats.usage_saved.requests, 1);
    assert_eq!(stats.usage_saved.input_tokens, 12);
    assert_eq!(stats.usage_saved.output_tokens, 1);
}

#[tokio::test]
async fn semantic_cache_calls_llm_only_on_miss() {
    let Some(client) = common::connect().await else { return };
    let ns = common::unique("semcache");
    let cache = SemanticCache::new(client.clone(), &ns, Some(3600));
    if let Err(e) = cache.create_namespace(3, Some("cosine"), Some(0.85)).await {
        if e.is_not_licensed() {
            common::unlicensed("semantic_cache_calls_llm_only_on_miss", &e);
            return;
        }
        panic!("unexpected error: {e}");
    }

    let calls = Arc::new(AtomicUsize::new(0));
    let vector = [1.0, 0.0, 0.0];

    let (r1, hit1) = cache
        .get_or_call("capital of France?", &vector, || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(LlmCallResult {
                    response: "Paris".to_string(),
                    usage: LlmUsage { model: "gpt-6-sol".to_string(), input_tokens: 8, output_tokens: 1, ..Default::default() },
                })
            }
        })
        .await
        .unwrap();
    assert_eq!(r1, "Paris");
    assert!(!hit1);

    let (r2, hit2) = cache
        .get_or_call("capital of France?", &[1.0, 0.0, 0.02], || {
            let calls = calls.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(LlmCallResult { response: "should not be called".to_string(), usage: LlmUsage::default() })
            }
        })
        .await
        .unwrap();
    assert_eq!(r2, "Paris");
    assert!(hit2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let stats = cache.stats().await.unwrap();
    assert_eq!(stats.usage_saved.requests, 1);
    assert_eq!(stats.usage_saved.input_tokens, 8);
}
