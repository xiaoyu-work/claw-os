use super::*;

#[test]
fn successful_memory_reads_expose_references_without_contents_or_queries() {
    let kind = crate::agent::trust::SourceKind::RecalledMemory;
    let body = serde_json::json!({"name":"USER.md","revision":"0".repeat(64),"content":"PRIVATE_MEMORY_CONTENT"});
    let envelope = crate::agent::trust::envelope::render(
        crate::agent::trust::envelope::process_seal(),
        &crate::agent::trust::SourceRef::new(kind), kind.class(), &body.to_string(),
    );
    let references = references("cos_memory", &envelope);
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].source, "notes/USER.md");
    assert!(!serde_json::to_string(&references).unwrap().contains("PRIVATE_MEMORY_CONTENT"));
    assert!(super::references("untrusted_app_tool", &envelope).is_empty());
}
