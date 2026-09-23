use super::*;

fn input(name: &str, media_type: &str, bytes: &[u8]) -> AttachmentInput {
    AttachmentInput {
        name: name.to_string(),
        media_type: media_type.to_string(),
        data: STANDARD.encode(bytes),
    }
}

#[test]
fn supported_images_are_normalized_with_verified_metadata() {
    let png = b"\x89PNG\r\n\x1a\nfixture";
    let attachments = normalize(vec![input("screen.png", "image/png", png)]).unwrap();
    assert_eq!(attachments[0].bytes, png.len() as u64);
    assert_eq!(
        attachments[0].sha256,
        format!("sha256:{}", crate::crypto::sha256_hex(png))
    );
    assert!(recorded_prompt("Inspect this", &attachments).contains("image/png"));
    assert!(!recorded_prompt("Inspect this", &attachments).contains("screen.png"));
}

#[test]
fn attachment_content_and_metadata_are_closed_and_consistent() {
    assert!(normalize(vec![input("fake.png", "image/png", b"GIF89a-not-a-png",)]).is_err());
    assert!(normalize(vec![AttachmentInput {
        name: "../escape.png".to_string(),
        media_type: "image/png".to_string(),
        data: STANDARD.encode(b"\x89PNG\r\n\x1a\nfixture"),
    }])
    .is_err());

    let attachment = normalize(vec![input(
        "screen.png",
        "image/png",
        b"\x89PNG\r\n\x1a\nfixture",
    )])
    .unwrap()
    .remove(0);
    let mut value = serde_json::to_value(&attachment).unwrap();
    value["bytes"] = serde_json::json!(1);
    assert!(serde_json::from_value::<ImageAttachment>(value).is_err());
}

#[test]
fn count_and_total_size_are_bounded() {
    let png = b"\x89PNG\r\n\x1a\nfixture";
    assert!(normalize(
        (0..=MAX_ATTACHMENTS)
            .map(|index| input(&format!("{index}.png"), "image/png", png))
            .collect(),
    )
    .is_err());

    let mut oversized = b"\x89PNG\r\n\x1a\n".to_vec();
    oversized.resize(MAX_TOTAL_BYTES + 1, 0);
    assert!(normalize(vec![input("large.png", "image/png", &oversized)]).is_err());
}
