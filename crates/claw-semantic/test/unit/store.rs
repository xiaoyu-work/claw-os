use super::*;
use crate::Chunk;
use tempfile::tempdir;

#[test]
fn upsert_then_search_roundtrips() {
    let d = tempdir().unwrap();
    let f = d.path().join("s.json");
    let s = MemoryStore::open(f).unwrap();
    let chunks = vec![Chunk {
        path: "/x".into(),
        chunk_id: 0,
        text: "hello world".into(),
    }];
    let vecs = vec![vec![1.0, 0.0, 0.0]];
    // /x doesn't exist on disk → search filters it out.
    s.upsert("/x", &chunks, &vecs).unwrap();
    let hits = s.search(&[1.0, 0.0, 0.0], 5).unwrap();
    assert!(hits.is_empty(), "non-existent path should be filtered");
    let stats = s.stats().unwrap();
    assert_eq!(stats.n_chunks, 1);
    assert_eq!(stats.n_paths, 1);
}

#[test]
fn upsert_replaces_previous_chunks() {
    let d = tempdir().unwrap();
    let f = d.path().join("s.json");
    let s = MemoryStore::open(f).unwrap();
    let p = "/y";
    s.upsert(
        p,
        &[Chunk {
            path: p.into(),
            chunk_id: 0,
            text: "a".into(),
        }],
        &[vec![1.0, 0.0]],
    )
    .unwrap();
    s.upsert(
        p,
        &[
            Chunk {
                path: p.into(),
                chunk_id: 0,
                text: "a2".into(),
            },
            Chunk {
                path: p.into(),
                chunk_id: 1,
                text: "b".into(),
            },
        ],
        &[vec![1.0, 0.0], vec![0.0, 1.0]],
    )
    .unwrap();
    assert_eq!(s.stats().unwrap().n_chunks, 2);
}
