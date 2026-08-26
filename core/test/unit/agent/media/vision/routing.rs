use super::*;

fn desc(bytes: usize, mime: ImageMime, intent: ImageIntent) -> ImageDescriptor {
    ImageDescriptor {
        bytes_len: bytes,
        mime,
        intent,
    }
}

#[test]
fn mime_from_str_normalises() {
    assert_eq!(ImageMime::from_str("image/PNG"), ImageMime::Png);
    assert_eq!(ImageMime::from_str(" jpeg "), ImageMime::Jpeg);
    assert_eq!(ImageMime::from_str("image/heif"), ImageMime::Heic);
    assert_eq!(ImageMime::from_str("application/x-foo"), ImageMime::Other);
}

#[test]
fn widely_supported_set() {
    assert!(ImageMime::Png.is_widely_supported());
    assert!(ImageMime::Jpeg.is_widely_supported());
    assert!(ImageMime::Webp.is_widely_supported());
    assert!(ImageMime::Gif.is_widely_supported());
    assert!(!ImageMime::Heic.is_widely_supported());
    assert!(!ImageMime::Bmp.is_widely_supported());
    assert!(!ImageMime::Tiff.is_widely_supported());
    assert!(!ImageMime::Other.is_widely_supported());
}

#[test]
fn vision_disabled_always_skips() {
    let policy = RoutingPolicy {
        provider_supports_vision: true,
        vision_enabled: false,
        ocr_available: true,
        ..RoutingPolicy::default()
    };
    let d = desc(1024, ImageMime::Png, ImageIntent::General);
    assert!(matches!(route(&d, &policy), RoutingDecision::Skip { .. }));
}

#[test]
fn empty_image_skips() {
    let policy = RoutingPolicy {
        provider_supports_vision: true,
        ..RoutingPolicy::default()
    };
    let d = desc(0, ImageMime::Png, ImageIntent::General);
    assert!(matches!(route(&d, &policy), RoutingDecision::Skip { .. }));
}

#[test]
fn extract_text_intent_prefers_ocr_when_available() {
    let policy = RoutingPolicy {
        provider_supports_vision: true,
        ocr_available: true,
        ..RoutingPolicy::default()
    };
    let d = desc(1024, ImageMime::Png, ImageIntent::ExtractText);
    assert_eq!(route(&d, &policy), RoutingDecision::Ocr);
}

#[test]
fn extract_text_intent_falls_back_to_native_without_ocr() {
    let policy = RoutingPolicy {
        provider_supports_vision: true,
        ocr_available: false,
        ..RoutingPolicy::default()
    };
    let d = desc(1024, ImageMime::Png, ImageIntent::ExtractText);
    assert_eq!(route(&d, &policy), RoutingDecision::Native);
}

#[test]
fn native_when_provider_supports_and_size_under_cap() {
    let policy = RoutingPolicy {
        provider_supports_vision: true,
        max_native_bytes: 1024,
        ..RoutingPolicy::default()
    };
    let d = desc(512, ImageMime::Jpeg, ImageIntent::Caption);
    assert_eq!(route(&d, &policy), RoutingDecision::Native);
}

#[test]
fn over_size_cap_falls_back_to_ocr_when_available() {
    let policy = RoutingPolicy {
        provider_supports_vision: true,
        max_native_bytes: 1024,
        ocr_available: true,
        ..RoutingPolicy::default()
    };
    let d = desc(2048, ImageMime::Jpeg, ImageIntent::General);
    assert_eq!(route(&d, &policy), RoutingDecision::Ocr);
}

#[test]
fn over_size_cap_with_no_ocr_skips() {
    let policy = RoutingPolicy {
        provider_supports_vision: true,
        max_native_bytes: 1024,
        ocr_available: false,
        ..RoutingPolicy::default()
    };
    let d = desc(2048, ImageMime::Jpeg, ImageIntent::General);
    match route(&d, &policy) {
        RoutingDecision::Skip { reason } => assert!(reason.contains("exceeds native cap")),
        other => panic!("expected Skip, got {other:?}"),
    }
}

#[test]
fn unsupported_mime_routes_to_ocr_if_available() {
    let policy = RoutingPolicy {
        provider_supports_vision: true,
        ocr_available: true,
        ..RoutingPolicy::default()
    };
    let d = desc(1024, ImageMime::Heic, ImageIntent::General);
    assert_eq!(route(&d, &policy), RoutingDecision::Ocr);
}

#[test]
fn unsupported_mime_no_ocr_skips() {
    let policy = RoutingPolicy {
        provider_supports_vision: true,
        ocr_available: false,
        ..RoutingPolicy::default()
    };
    let d = desc(1024, ImageMime::Heic, ImageIntent::General);
    assert!(matches!(route(&d, &policy), RoutingDecision::Skip { .. }));
}

#[test]
fn no_vision_no_ocr_skips_with_reason() {
    let policy = RoutingPolicy {
        provider_supports_vision: false,
        ocr_available: false,
        ..RoutingPolicy::default()
    };
    let d = desc(1024, ImageMime::Png, ImageIntent::General);
    match route(&d, &policy) {
        RoutingDecision::Skip { reason } => {
            assert!(reason.contains("no vision") || reason.contains("OCR"));
        }
        other => panic!("expected Skip, got {other:?}"),
    }
}

#[test]
fn no_vision_but_ocr_available_routes_ocr() {
    let policy = RoutingPolicy {
        provider_supports_vision: false,
        ocr_available: true,
        ..RoutingPolicy::default()
    };
    let d = desc(1024, ImageMime::Png, ImageIntent::General);
    assert_eq!(route(&d, &policy), RoutingDecision::Ocr);
}
