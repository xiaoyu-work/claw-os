use super::*;
use crate::Chunk;
use tempfile::tempdir;

#[test]
fn existing_json_format_opens_without_migration() {
    let directory = tempdir().unwrap();
    let file = directory.path().join("store.json");
    std::fs::write(
        &file,
        r#"[{"path":"/legacy","chunk_id":7,"text":"existing","vec":[1.0,0.0]}]"#,
    )
    .unwrap();

    let store = MemoryStore::open(file).unwrap();
    let stats = store.stats().unwrap();
    assert_eq!(stats.n_paths, 1);
    assert_eq!(stats.n_chunks, 1);
    assert_eq!(stats.dim, 2);
}

#[test]
fn upsert_then_search_roundtrips() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source.txt");
    std::fs::write(&source, "hello world").unwrap();
    let file = directory.path().join("store.json");
    let store = MemoryStore::open(file).unwrap();
    let path = source.to_string_lossy().into_owned();
    let chunks = vec![Chunk {
        path: path.clone(),
        chunk_id: 0,
        text: "hello world".into(),
    }];
    store
        .upsert(&path, &chunks, &[vec![1.0, 0.0, 0.0]])
        .unwrap();

    let hits = store.search(&[1.0, 0.0, 0.0], 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, path);
    assert_eq!(hits[0].snippet, "hello world");
    let stats = store.stats().unwrap();
    assert_eq!(stats.n_chunks, 1);
    assert_eq!(stats.n_paths, 1);
}

#[test]
fn upsert_replaces_previous_chunks() {
    let directory = tempdir().unwrap();
    let file = directory.path().join("store.json");
    let store = MemoryStore::open(file).unwrap();
    let path = "/document";
    store
        .upsert(
            path,
            &[Chunk {
                path: path.into(),
                chunk_id: 0,
                text: "a".into(),
            }],
            &[vec![1.0, 0.0]],
        )
        .unwrap();
    store
        .upsert(
            path,
            &[
                Chunk {
                    path: path.into(),
                    chunk_id: 0,
                    text: "a2".into(),
                },
                Chunk {
                    path: path.into(),
                    chunk_id: 1,
                    text: "b".into(),
                },
            ],
            &[vec![1.0, 0.0], vec![0.0, 1.0]],
        )
        .unwrap();
    assert_eq!(store.stats().unwrap().n_chunks, 2);
}

#[test]
fn mismatched_chunks_and_vectors_are_rejected() {
    let directory = tempdir().unwrap();
    let store = MemoryStore::open(directory.path().join("store.json")).unwrap();
    let error = store
        .upsert(
            "/document",
            &[Chunk {
                path: "/document".into(),
                chunk_id: 0,
                text: "a".into(),
            }],
            &[],
        )
        .unwrap_err();
    assert!(matches!(
        error,
        DocumentStoreError::LengthMismatch {
            chunks: 1,
            vectors: 0
        }
    ));
}

#[test]
fn failed_persistence_does_not_publish_in_memory_changes() {
    let directory = tempdir().unwrap();
    let mut store = MemoryStore::open(directory.path().join("store.json")).unwrap();
    store.file = directory.path().join("missing").join("store.json");
    let chunk = Chunk {
        path: "/document".into(),
        chunk_id: 0,
        text: "content".into(),
    };

    let error = store
        .upsert("/document", &[chunk], &[vec![1.0, 0.0]])
        .unwrap_err();
    assert!(matches!(error, DocumentStoreError::Io { .. }));
    assert_eq!(store.stats().unwrap().n_chunks, 0);
}
