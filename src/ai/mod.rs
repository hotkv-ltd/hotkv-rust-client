// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

//! Enterprise AI commands and helpers: prompt/semantic caching, agent
//! memory, RAG, LLM stats and budgets. Every raw command family is
//! reached through an accessor on [`crate::Client`] (`client.scache()`,
//! `client.pcache()`, `client.amem()`, `client.rag()`,
//! `client.llmstats()`, and so on); [`PromptCache`] and [`SemanticCache`]
//! are the idiomatic get-or-call layers on top described in the SDK
//! specification section 4.

mod cache;
mod memory;
mod prompt_cache;
mod semantic_cache;

pub use cache::{
    Expiry, KeyedCacheCommands, KeyedCacheStats, LlmBudgetCommands, LlmBudgetStatus, LlmPrice, LlmPriceOptions, LlmStats, LlmStatsCommands,
    LlmStatsRecordOptions, PcacheCommands, PcacheEntry, PcacheStats, ScacheCommands, ScacheStats, Usage, UsageSavings,
};
pub use memory::{
    AgentSummary, AmemCommands, ConvTurn, CrossSearchResult, EmbedDedupCommands, EmbedDedupStats, EpisodicEvent, EpisodicStats, PromptCommands,
    PromptEntry, RagCommands, RagResult, RagStats, SemRecall,
};
pub use prompt_cache::{canonical_json, prompt_cache_key, LlmCallResult, LlmUsage, PromptCache, PromptCacheKeyInput, PromptMessage};
pub use semantic_cache::SemanticCache;
