// Copyright (c) 2026 HotKV Ltd
// Licensed under the Apache License, Version 2.0. See LICENSE in the project root.

mod common;

#[tokio::test]
async fn prompt_registry() {
    let Some(client) = common::connect().await else { return };
    let name = common::unique("prompt");
    let pr = client.prompt();

    let result = pr.put(&name, "You are a helpful assistant.", None).await;
    let v1 = skip_if_not_licensed!(result, "prompt-registry");
    assert_eq!(v1, 1);
    let v2 = pr.put(&name, "You are an even more helpful assistant.", Some("{\"author\":\"test\"}")).await.unwrap();
    assert_eq!(v2, 2);

    let latest = pr.latest(&name).await.unwrap().expect("entry present");
    assert_eq!(latest.version, 2);
    assert_eq!(latest.content, "You are an even more helpful assistant.");

    let v1_entry = pr.get(&name, Some(1)).await.unwrap().expect("v1 present");
    assert_eq!(v1_entry.content, "You are a helpful assistant.");

    let mut versions = pr.versions(&name).await.unwrap();
    versions.sort();
    assert_eq!(versions, vec![1, 2]);

    pr.set_active(&name, 1).await.unwrap();
    assert!(pr.list().await.unwrap().contains(&name));
    assert!(pr.drop(&name).await.unwrap());
}

#[tokio::test]
async fn embed_dedup() {
    let Some(client) = common::connect().await else { return };
    let ns = common::unique("embeddedup");
    let ed = client.embed_dedup();

    let hash = "sha256:abc123";
    let result = ed.put(&ns, hash, "text-embedding-3", "0.1,0.2,0.3").await;
    let first = skip_if_not_licensed!(result, "embed-dedup");
    assert!(first, "first PUT of a new hash is newly stored");

    let second = ed.put(&ns, hash, "text-embedding-3", "0.1,0.2,0.3").await.unwrap();
    assert!(!second, "second PUT of the same hash increments the refcount instead");

    assert_eq!(ed.get(&ns, hash).await.unwrap(), Some("0.1,0.2,0.3".to_string()));

    let stats = ed.stats(&ns).await.unwrap();
    assert!(stats.entries >= 1);

    assert!(ed.release(&ns, hash).await.unwrap());
    assert!(ed.release(&ns, hash).await.unwrap());
    ed.purge(&ns).await.unwrap();
    assert!(ed.list().await.unwrap().contains(&ns));
    ed.drop(&ns).await.unwrap();
}

#[tokio::test]
async fn amem_conversation_and_working_memory() {
    let Some(client) = common::connect().await else { return };
    let session = common::unique("amem-session");
    let am = client.amem();

    let result = am.conv_create(&session, Some(10), None).await;
    let () = skip_if_not_licensed!(result, "agent-memory");

    am.conv_append(&session, "user", "hello").await.unwrap();
    am.conv_append(&session, "assistant", "hi there").await.unwrap();

    let turns = am.conv_get(&session).await.unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].role, "user");
    assert_eq!(turns[0].content, "hello");
    assert_eq!(turns[1].role, "assistant");

    let windowed = am.conv_window(&session, 1).await.unwrap();
    assert_eq!(windowed.len(), 1);
    assert_eq!(windowed[0].content, "hi there");

    assert!(am.conv_clear(&session).await.unwrap());
    assert!(am.conv_drop(&session).await.unwrap());

    let wk_session = common::unique("amem-work");
    am.work_set(&wk_session, "scratch", "value1", None).await.unwrap();
    assert_eq!(am.work_get(&wk_session, "scratch").await.unwrap(), Some("value1".to_string()));
    assert!(am.work_list(&wk_session).await.unwrap().contains(&"scratch".to_string()));
    assert!(am.work_del(&wk_session, "scratch").await.unwrap());
    am.work_clear(&wk_session).await.unwrap();
}

#[tokio::test]
async fn amem_semantic_episodic_and_search() {
    let Some(client) = common::connect().await else { return };
    let agent = common::unique("amem-agent");
    let am = client.amem();

    let result = am.sem_create(&agent, 3).await;
    let () = skip_if_not_licensed!(result, "agent-memory");
    am.sem_store(&agent, "fact1", "the sky is blue", &[1.0, 0.0, 0.0], None).await.unwrap();
    am.sem_store(&agent, "fact2", "grass is green", &[0.0, 1.0, 0.0], None).await.unwrap();

    let recalled = am.sem_recall(&agent, 1, &[1.0, 0.0, 0.01]).await.unwrap();
    assert_eq!(recalled.len(), 1);
    assert_eq!(recalled[0].key, "fact1");

    let mut listed = am.sem_list(&agent).await.unwrap();
    listed.sort();
    assert_eq!(listed, vec!["fact1".to_string(), "fact2".to_string()]);
    assert!(am.sem_forget(&agent, "fact2").await.unwrap());

    am.epis_log(&agent, "tool_call", "called search()", Some("{\"ok\":true}")).await.unwrap();
    am.epis_log(&agent, "tool_call", "called fetch()", None).await.unwrap();

    let events = am.epis_query(&agent, Some("tool_call"), None, None, None).await.unwrap();
    assert_eq!(events.len(), 2);

    let epis_stats = am.epis_stats(&agent).await.unwrap();
    assert_eq!(epis_stats.total_events, 2);

    let cross = am.search(&agent, 5, &[1.0, 0.0, 0.0]).await.unwrap();
    assert!(!cross.is_empty());

    am.epis_clear(&agent).await.unwrap();
}

#[tokio::test]
async fn amem_summary() {
    let Some(client) = common::connect().await else { return };
    let agent = common::unique("amem-summary");
    let am = client.amem();

    let result = am.summary_set(&agent, "User is debugging a Rust SDK.").await;
    let v1 = skip_if_not_licensed!(result, "amem-summary");
    assert_eq!(v1, 1);

    let v2 = am.summary_append(&agent, " Prefers concise answers.").await.unwrap();
    assert_eq!(v2, 2);

    let summary = am.summary_get(&agent).await.unwrap().expect("summary present");
    assert!(summary.summary.contains("debugging"));
    assert!(summary.summary.contains("concise"));
    assert_eq!(summary.version, 2);

    assert!(am.summary_list().await.unwrap().contains(&agent));
    assert!(am.summary_drop(&agent).await.unwrap());
}

#[tokio::test]
async fn rag_pipeline() {
    let Some(client) = common::connect().await else { return };
    let pipeline = common::unique("rag");
    let rag = client.rag();

    let result = rag.create(&pipeline, 3, Some("fixed"), Some(512), Some(64)).await;
    let () = skip_if_not_licensed!(result, "rag-pipeline");

    let ingested = rag
        .ingest(
            &pipeline,
            "doc1",
            "HotKV is a fast in-memory data store.",
            &[("source", "manual")],
            &[&[0.1, 0.2, 0.3]],
        )
        .await
        .unwrap();
    assert!(ingested >= 1);

    let results = rag.search(&pipeline, "fast data store", Some(5), Some(0.5), Some(&[0.1, 0.2, 0.3]), &[]).await.unwrap();
    assert!(!results.is_empty());
    assert_eq!(results[0].doc_id, "doc1");

    let stats = rag.stats(&pipeline).await.unwrap();
    assert_eq!(stats.name, pipeline);
    assert_eq!(stats.dimensions, 3);
    assert!(stats.total_docs >= 1);

    assert!(rag.list().await.unwrap().contains(&pipeline));
    rag.drop(&pipeline).await.unwrap();
}
