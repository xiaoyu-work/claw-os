use super::*;

#[test]
fn legacy_module_paths_reexport_shared_primitives() {
    fn assert_embedder<T: embed::Embedder>() {}
    fn assert_extractor<T: extract::Extractor>() {}
    fn assert_store<T: store::VectorStore>() {}

    assert_embedder::<embed::StubEmbedder>();
    assert_extractor::<extract::TextExtractor>();
    assert_store::<store::MemoryStore>();

    let chunks = chunk::chunks_for("/document", "content", 128, 16);
    let _: &claw_embed::Chunk = &chunks[0];
    let _: Option<watch::FsEvent> = None;
}
