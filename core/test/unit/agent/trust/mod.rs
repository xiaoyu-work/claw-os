use super::*;

/// Requirement 11: the registry must be exhaustive.
///
/// [`SourceKind::ordinal`] is an exhaustive `match`, so a new variant
/// fails to compile without an ordinal. This test is the other half: it
/// fails until the variant is also listed in `ALL` and therefore has a
/// declared class, persistence, projection and audit strategy.
#[test]
fn registry_is_exhaustive_and_densely_indexed() {
    for (index, kind) in SourceKind::ALL.iter().enumerate() {
        assert_eq!(
            kind.ordinal(),
            index,
            "{kind} is out of registry order; ALL and ordinal() disagree"
        );
    }
    // Every ordinal in range is claimed exactly once.
    let mut seen = vec![false; SourceKind::ALL.len()];
    for kind in SourceKind::ALL {
        assert!(
            kind.ordinal() < seen.len(),
            "{kind} has an ordinal outside ALL; add it to SourceKind::ALL"
        );
        assert!(!seen[kind.ordinal()], "{kind} duplicates an ordinal");
        seen[kind.ordinal()] = true;
    }
    assert!(
        seen.into_iter().all(|claimed| claimed),
        "a SourceKind variant has an ordinal but is missing from SourceKind::ALL"
    );
}

#[test]
fn every_source_declares_a_unique_stable_tag() {
    let mut tags = std::collections::BTreeSet::new();
    for kind in SourceKind::ALL {
        let tag = kind.tag();
        assert!(!tag.is_empty(), "{kind} has an empty tag");
        assert!(
            crate::audit_policy::is_token(tag),
            "{kind} tag `{tag}` is not audit-safe"
        );
        assert!(tags.insert(tag), "duplicate source tag `{tag}`");
    }
}

#[test]
fn tag_round_trips_and_unknown_tags_are_refused() {
    for kind in SourceKind::ALL {
        assert_eq!(SourceKind::from_tag(kind.tag()), *kind);
    }
    assert_eq!(SourceKind::from_tag("no_such_source"), SourceKind::Unknown);
    assert_eq!(SourceKind::from_tag(""), SourceKind::Unknown);
    assert_eq!(SourceKind::Unknown.class(), TrustClass::LegacyUnknown);
}

/// Only sources the kernel or the operator authored may reach the
/// policy channel, and only they may be projected verbatim into it.
#[test]
fn only_kernel_policy_sources_reach_the_policy_channel() {
    for kind in SourceKind::ALL {
        let profile = kind.profile();
        if profile.projection == Projection::PolicyChannel {
            assert_eq!(
                profile.class,
                TrustClass::SystemPolicy,
                "{kind} projects into the policy channel without being policy"
            );
        }
        if profile.class == TrustClass::SystemPolicy {
            assert_eq!(
                profile.projection,
                Projection::PolicyChannel,
                "{kind} is policy but is not projected into the policy channel"
            );
        }
    }
}

/// Anything the model or a third party can influence must be fenced.
#[test]
fn untrusted_and_extension_sources_are_always_fenced_or_tool_definitions() {
    for kind in SourceKind::ALL {
        let profile = kind.profile();
        match profile.class {
            TrustClass::UntrustedExternalContent
            | TrustClass::ExtensionMetadata
            | TrustClass::LegacyUnknown => assert!(
                matches!(
                    profile.projection,
                    Projection::UserChannelEnvelope
                        | Projection::ToolChannelEnvelope
                        | Projection::ToolDefinition
                ),
                "{kind} is not policy-grade but is projected unfenced"
            ),
            TrustClass::UserControlledContext => assert!(
                matches!(
                    profile.projection,
                    Projection::UserChannelEnvelope | Projection::ToolChannelEnvelope
                ),
                "{kind} is owner-controlled context but is projected unfenced"
            ),
            TrustClass::ModelGenerated => assert_eq!(
                profile.projection,
                Projection::AssistantChannel,
                "{kind} is model output and belongs in the assistant channel"
            ),
            TrustClass::UserInstruction => assert_eq!(
                profile.projection,
                Projection::UserChannelVerbatim,
                "{kind} is the owner's turn and must not be fenced"
            ),
            TrustClass::SystemPolicy => {}
        }
    }
}

#[test]
fn model_and_external_sources_never_freeze_into_the_prompt_snapshot() {
    for kind in SourceKind::ALL {
        let profile = kind.profile();
        if profile.persistence == Persistence::FrozenPrompt {
            assert!(
                profile.class.rank() >= TrustClass::ExtensionMetadata.rank(),
                "{kind} would freeze untrusted or model bytes into the session prompt"
            );
        }
    }
}

#[test]
fn class_lattice_is_totally_ordered_and_least_is_commutative() {
    for (index, class) in TrustClass::ALL.iter().enumerate() {
        assert_eq!(class.rank() as usize, index);
    }
    for left in TrustClass::ALL {
        for right in TrustClass::ALL {
            assert_eq!(left.least(*right), right.least(*left));
            assert!(left.least(*right).rank() <= left.rank());
            assert!(left.least(*right).rank() <= right.rank());
        }
    }
}

#[test]
fn least_of_an_empty_set_is_the_most_restrictive_class() {
    assert_eq!(TrustClass::least_of([]), TrustClass::LegacyUnknown);
}

#[test]
fn only_system_policy_is_policy_and_everything_else_needs_a_fence() {
    for class in TrustClass::ALL {
        assert_eq!(class.is_policy(), *class == TrustClass::SystemPolicy);
        assert_eq!(class.requires_envelope(), !class.is_policy());
    }
}

/// Requirement 1 and 2: a label can only be *asserted* by a trusted
/// adapter naming a source. Any label recovered from bytes is clamped.
#[test]
fn parsed_labels_are_clamped_below_owner_authority() {
    assert_eq!(
        TrustClass::from_stored_label("system-policy"),
        TrustClass::LegacyUnknown
    );
    assert_eq!(
        TrustClass::from_stored_label("user-instruction"),
        TrustClass::LegacyUnknown
    );
    assert_eq!(
        TrustClass::from_stored_label("user-context"),
        TrustClass::UserControlledContext
    );
    assert_eq!(
        TrustClass::from_stored_label("untrusted-external"),
        TrustClass::UntrustedExternalContent
    );
    assert_eq!(
        TrustClass::from_stored_label("anything else"),
        TrustClass::LegacyUnknown
    );
    assert_eq!(TrustClass::from_stored_label(""), TrustClass::LegacyUnknown);
}

#[test]
fn deserialization_cannot_assert_policy() {
    let forged: TrustClass = serde_json::from_str("\"system-policy\"").expect("parses");
    assert_eq!(forged, TrustClass::LegacyUnknown);
    let forged: TrustClass = serde_json::from_str("\"user-instruction\"").expect("parses");
    assert_eq!(forged, TrustClass::LegacyUnknown);
    let kept: TrustClass = serde_json::from_str("\"model-generated\"").expect("parses");
    assert_eq!(kept, TrustClass::ModelGenerated);
}

#[test]
fn serialization_round_trips_below_the_ceiling() {
    for class in TrustClass::ALL {
        let json = serde_json::to_string(class).expect("serializes");
        let back: TrustClass = serde_json::from_str(&json).expect("parses");
        if class.rank() > TrustClass::parse_ceiling().rank() {
            assert_eq!(back, TrustClass::LegacyUnknown);
        } else {
            assert_eq!(back, *class);
        }
    }
}

#[test]
fn default_class_is_the_most_restrictive() {
    assert_eq!(TrustClass::default(), TrustClass::LegacyUnknown);
}

#[test]
fn source_locators_are_bounded_and_never_carry_raw_urls() {
    let reference = SourceRef::with_locator(
        SourceKind::WebPageContent,
        "https://evil.example/path?token=hunter2&x=<script>",
    );
    let label = reference.label();
    assert!(!label.contains("hunter2"));
    assert!(!label.contains("script"));
    assert!(!label.contains("evil.example"));
    assert_eq!(
        label,
        format!(
            "web_page_content:{}",
            crate::audit_policy::UNLOGGABLE
        )
    );
}

#[test]
fn source_locators_keep_safe_identifiers() {
    let reference = SourceRef::with_locator(SourceKind::McpToolResult, "github");
    assert_eq!(reference.label(), "mcp_tool_result:github");
    assert_eq!(reference.class(), TrustClass::UntrustedExternalContent);
}

/// The journal keeps a coarser three-value vocabulary. Widening the
/// lattice must never widen a journal record.
#[test]
fn journal_projection_never_widens_trust() {
    use crate::session::journal::Trust;
    for class in TrustClass::ALL {
        let projected = journal_trust(*class);
        match class {
            TrustClass::SystemPolicy | TrustClass::UserInstruction => {
                assert_eq!(projected, Trust::Trusted)
            }
            TrustClass::LegacyUnknown => assert_eq!(projected, Trust::Unknown),
            _ => assert_eq!(projected, Trust::Untrusted, "{class} must not be trusted"),
        }
    }
}

#[test]
fn journal_projection_is_total_over_the_registry() {
    for kind in SourceKind::ALL {
        // Both projections are total; this fails to compile-and-run only
        // if a new kind is added without a mapping arm.
        let _ = journal_segment_kind(*kind);
        let _ = journal_origin(*kind);
        let _ = journal_trust(kind.class());
    }
}

#[test]
fn journal_origin_attributes_third_party_content_away_from_the_system() {
    use crate::session::journal::Origin;
    assert_eq!(journal_origin(SourceKind::McpToolResult), Origin::Tool);
    assert_eq!(journal_origin(SourceKind::AppToolResult), Origin::App);
    assert_eq!(journal_origin(SourceKind::WebPageContent), Origin::Tool);
    assert_eq!(journal_origin(SourceKind::UserMessage), Origin::User);
    assert_eq!(
        journal_origin(SourceKind::ModelCompressionSummary),
        Origin::Model
    );
}
