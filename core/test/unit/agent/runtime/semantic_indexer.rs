use super::*;

#[tokio::test]
async fn empty_text_does_not_panic_and_does_not_spawn() {
    // No embedder → if we DID try to index, it'd error. The empty
    // guard means we short-circuit before touching the store.
    let store = SemanticStore::open_in_memory(None).unwrap();
    let ix = SemanticIndexer::from_store(store);
    ix.spawn_index("s1".into(), "user", 1, "".into());
    ix.spawn_index("s1".into(), "user", 2, "   \n\t  ".into());
    // Yield once so any (incorrectly) spawned task gets a chance to run.
    tokio::task::yield_now().await;
    let count = ix.store.count(None).unwrap();
    assert_eq!(count, 0, "no rows should have been indexed");
}
