//! The closed registry of everything that can become model-visible.
//!
//! Every ingestion adapter names its [`SourceKind`]. The kind — not the
//! caller, not the text, not the chat role — decides the segment's
//! [`TrustClass`], where it may be persisted, which provider channel it
//! is projected into, and what audit records about it.
//!
//! [`SourceKind::profile`] is a single exhaustive `match`. A new
//! model-visible source therefore cannot be added without declaring all
//! four properties, and [`SourceKind::ordinal`] plus
//! [`SourceKind::ALL`] keep the registry enumerable for the coverage
//! test. Anything the runtime cannot name arrives as
//! [`SourceKind::Unknown`], which is [`TrustClass::LegacyUnknown`].

use super::class::TrustClass;

/// Where a labelled segment is allowed to come to rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Persistence {
    /// Captured once into the session's content-addressed prompt
    /// snapshot and replayed verbatim for the life of the session.
    FrozenPrompt,
    /// Written to the owner-private conversation store and replayed on
    /// later turns.
    SessionHistory,
    /// Recorded as an `injected` audit row but never replayed as
    /// conversation content.
    InjectedAuditRow,
    /// Rebuilt every request; nothing durable is written.
    RequestLocal,
}

/// Which provider channel the segment is projected into.
///
/// Chat role is a transport detail. This says which channel the runtime
/// is *allowed* to use, and whether the bytes must be fenced first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Projection {
    /// The provider's immutable `system`/`developer` channel, verbatim.
    /// Only [`TrustClass::SystemPolicy`] reaches this.
    PolicyChannel,
    /// The user channel, verbatim — the owner's own words this turn.
    UserChannelVerbatim,
    /// The user channel, inside a sealed data envelope.
    UserChannelEnvelope,
    /// A provider tool-result block, inside a sealed data envelope.
    ToolChannelEnvelope,
    /// The assistant channel; model text is replayed as the model's own
    /// prior output and is never fenced as data.
    AssistantChannel,
    /// Carried as a provider tool *definition* (name/description/schema)
    /// rather than as a message. Bounded and sanitised at ingestion.
    ToolDefinition,
}

/// What the audit and journal surfaces record about the segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AuditStrategy {
    /// A content-addressed reference into an owner-private store.
    ContentRef,
    /// The bounded source label plus a length and digest — used where
    /// the bytes themselves may not be stored.
    LabelAndDigest,
    /// The source label only; the payload is reconstructable from the
    /// registry or the extension package it came from.
    LabelOnly,
}

/// The declared behaviour of one model-input source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceProfile {
    pub kind: SourceKind,
    /// Stable tag written into envelopes, audit rows and journal labels.
    pub tag: &'static str,
    pub class: TrustClass,
    pub persistence: Persistence,
    pub projection: Projection,
    pub audit: AuditStrategy,
}

/// Every distinct way bytes reach a model request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SourceKind {
    // -- operator policy -------------------------------------------------
    /// The compiled system scaffold.
    SystemScaffold,
    /// A prompt file that verification proved is root-owned and not
    /// writable by the session owner. Only this variant may carry
    /// non-compiled bytes into the policy channel; see
    /// `prompt::read_operator_prompt_file`.
    RootOperatorPolicyFile,

    // -- authenticated owner ---------------------------------------------
    /// The prompt the session owner submitted this turn.
    UserMessage,
    /// A file/URL the owner referenced by name in their message.
    UserReference,

    // -- owner-controlled durable context --------------------------------
    /// `AgentConfig::system_prompt_path` when the file is owner-writable.
    ///
    /// This is user configuration, not administrator policy: an owner —
    /// or anything running as the owner, including a model-driven file
    /// write — can rewrite it. It is therefore owner-controlled context,
    /// and it does not reach the policy channel.
    OperatorPromptFile,
    /// `USER.md`.
    UserProfileNotes,
    /// `MEMORY.md`.
    MemoryNotes,
    /// A lexical or semantic recall hit returned to the model.
    RecalledMemory,
    /// Memory an App wrote on the owner's behalf.
    AppMemory,
    /// A due reminder injected into the turn.
    DueNudge,
    /// Todo list state surfaced to the model.
    TodoList,
    /// Filesystem markers describing the working directory.
    ProjectContext,
    /// Per-session extras attached by a local surface.
    SessionExtras,
    /// A prior user turn replayed as history. Owner-controlled, but no
    /// longer the request being served, so it does not carry this
    /// turn's instruction authority.
    ReplayedUserTurn,

    // -- extension metadata ----------------------------------------------
    /// Skill catalogue entries in the prompt (level-1 disclosure).
    SkillCatalogMetadata,
    /// A Skill's `SKILL.md` body (level-2 disclosure).
    SkillInstructions,
    /// A Skill resource file (level-3 disclosure).
    SkillResource,
    /// A built-in tool's own name/description/schema.
    BuiltinToolMetadata,
    /// An App-declared tool's name/description/schema.
    AppToolMetadata,
    /// A remote MCP server's tool name/description/`inputSchema`.
    McpToolMetadata,

    // -- untrusted external content ---------------------------------------
    /// Output of a kernel built-in tool.
    BuiltinToolResult,
    /// Output of an App operation.
    AppToolResult,
    /// Output of a remote MCP tool call.
    McpToolResult,
    /// Page text fetched from the network.
    WebPageContent,
    /// OCR, vision or speech transcription of an artefact.
    MediaTranscript,
    /// Context a desktop surface attached to the turn.
    TransientAppContext,
    /// A context event, trigger or watcher payload.
    ContextEvent,
    /// Text a hook wrote into the turn.
    HookOutput,

    // -- model output ------------------------------------------------------
    /// An assistant turn replayed as history.
    ModelResponse,
    /// A model-authored compression summary standing in for dropped
    /// history.
    ModelCompressionSummary,
    /// Provider reasoning summary text.
    ModelReasoning,

    // -- fallbacks ---------------------------------------------------------
    /// A stored row written before labelling existed.
    LegacyStoredRow,
    /// A source the runtime could not name.
    Unknown,
}

impl SourceKind {
    /// The registry, in ordinal order.
    pub const ALL: &'static [SourceKind] = &[
        SourceKind::SystemScaffold,
        SourceKind::RootOperatorPolicyFile,
        SourceKind::UserMessage,
        SourceKind::UserReference,
        SourceKind::OperatorPromptFile,
        SourceKind::UserProfileNotes,
        SourceKind::MemoryNotes,
        SourceKind::RecalledMemory,
        SourceKind::AppMemory,
        SourceKind::DueNudge,
        SourceKind::TodoList,
        SourceKind::ProjectContext,
        SourceKind::SessionExtras,
        SourceKind::ReplayedUserTurn,
        SourceKind::SkillCatalogMetadata,
        SourceKind::SkillInstructions,
        SourceKind::SkillResource,
        SourceKind::BuiltinToolMetadata,
        SourceKind::AppToolMetadata,
        SourceKind::McpToolMetadata,
        SourceKind::BuiltinToolResult,
        SourceKind::AppToolResult,
        SourceKind::McpToolResult,
        SourceKind::WebPageContent,
        SourceKind::MediaTranscript,
        SourceKind::TransientAppContext,
        SourceKind::ContextEvent,
        SourceKind::HookOutput,
        SourceKind::ModelResponse,
        SourceKind::ModelCompressionSummary,
        SourceKind::ModelReasoning,
        SourceKind::LegacyStoredRow,
        SourceKind::Unknown,
    ];

    /// Dense index into [`SourceKind::ALL`].
    ///
    /// The exhaustive match is the compile-time half of the coverage
    /// guarantee: a new variant does not build until it is given an
    /// ordinal. `registry_is_exhaustive` is the runtime half — it fails
    /// until the variant is also listed in `ALL` and given a profile.
    pub const fn ordinal(self) -> usize {
        match self {
            Self::SystemScaffold => 0,
            Self::RootOperatorPolicyFile => 1,
            Self::UserMessage => 2,
            Self::UserReference => 3,
            Self::OperatorPromptFile => 4,
            Self::UserProfileNotes => 5,
            Self::MemoryNotes => 6,
            Self::RecalledMemory => 7,
            Self::AppMemory => 8,
            Self::DueNudge => 9,
            Self::TodoList => 10,
            Self::ProjectContext => 11,
            Self::SessionExtras => 12,
            Self::ReplayedUserTurn => 13,
            Self::SkillCatalogMetadata => 14,
            Self::SkillInstructions => 15,
            Self::SkillResource => 16,
            Self::BuiltinToolMetadata => 17,
            Self::AppToolMetadata => 18,
            Self::McpToolMetadata => 19,
            Self::BuiltinToolResult => 20,
            Self::AppToolResult => 21,
            Self::McpToolResult => 22,
            Self::WebPageContent => 23,
            Self::MediaTranscript => 24,
            Self::TransientAppContext => 25,
            Self::ContextEvent => 26,
            Self::HookOutput => 27,
            Self::ModelResponse => 28,
            Self::ModelCompressionSummary => 29,
            Self::ModelReasoning => 30,
            Self::LegacyStoredRow => 31,
            Self::Unknown => 32,
        }
    }

    /// The declared behaviour of this source.
    ///
    /// Adding a variant without a profile is a compile error, which is
    /// the point: a new model-visible source cannot register without
    /// stating its provenance.
    pub const fn profile(self) -> SourceProfile {
        use AuditStrategy::*;
        use Persistence::*;
        use Projection::*;
        use TrustClass::*;

        let (tag, class, persistence, projection, audit) = match self {
            Self::SystemScaffold => (
                "system_scaffold",
                SystemPolicy,
                FrozenPrompt,
                PolicyChannel,
                LabelOnly,
            ),
            Self::RootOperatorPolicyFile => (
                "root_operator_policy_file",
                SystemPolicy,
                FrozenPrompt,
                PolicyChannel,
                ContentRef,
            ),
            Self::UserMessage => (
                "user_message",
                UserInstruction,
                SessionHistory,
                UserChannelVerbatim,
                ContentRef,
            ),
            Self::UserReference => (
                "user_reference",
                UserControlledContext,
                RequestLocal,
                UserChannelEnvelope,
                LabelAndDigest,
            ),
            Self::OperatorPromptFile => (
                "prompt_extra",
                UserControlledContext,
                RequestLocal,
                UserChannelEnvelope,
                ContentRef,
            ),
            Self::UserProfileNotes => (
                "user_profile_notes",
                UserControlledContext,
                FrozenPrompt,
                UserChannelEnvelope,
                ContentRef,
            ),
            Self::MemoryNotes => (
                "memory_notes",
                UserControlledContext,
                FrozenPrompt,
                UserChannelEnvelope,
                ContentRef,
            ),
            Self::RecalledMemory => (
                "recalled_memory",
                UserControlledContext,
                RequestLocal,
                ToolChannelEnvelope,
                LabelAndDigest,
            ),
            Self::AppMemory => (
                "app_memory",
                UserControlledContext,
                RequestLocal,
                ToolChannelEnvelope,
                LabelAndDigest,
            ),
            Self::DueNudge => (
                "due_nudges",
                UserControlledContext,
                InjectedAuditRow,
                UserChannelEnvelope,
                ContentRef,
            ),
            Self::TodoList => (
                "todo_list",
                UserControlledContext,
                RequestLocal,
                ToolChannelEnvelope,
                LabelAndDigest,
            ),
            Self::ProjectContext => (
                "project_context",
                UserControlledContext,
                RequestLocal,
                UserChannelEnvelope,
                LabelAndDigest,
            ),
            Self::SessionExtras => (
                "session_extras",
                UserControlledContext,
                InjectedAuditRow,
                UserChannelEnvelope,
                ContentRef,
            ),
            Self::ReplayedUserTurn => (
                "replayed_user_turn",
                UserControlledContext,
                SessionHistory,
                UserChannelEnvelope,
                ContentRef,
            ),
            Self::SkillCatalogMetadata => (
                "skills_catalog",
                ExtensionMetadata,
                FrozenPrompt,
                UserChannelEnvelope,
                ContentRef,
            ),
            Self::SkillInstructions => (
                "skill_instructions",
                ExtensionMetadata,
                RequestLocal,
                ToolChannelEnvelope,
                LabelAndDigest,
            ),
            Self::SkillResource => (
                "skill_resource",
                ExtensionMetadata,
                RequestLocal,
                ToolChannelEnvelope,
                LabelAndDigest,
            ),
            Self::BuiltinToolMetadata => (
                "builtin_tool_metadata",
                ExtensionMetadata,
                RequestLocal,
                ToolDefinition,
                LabelOnly,
            ),
            Self::AppToolMetadata => (
                "app_tool_metadata",
                ExtensionMetadata,
                RequestLocal,
                ToolDefinition,
                LabelOnly,
            ),
            Self::McpToolMetadata => (
                "mcp_tool_metadata",
                ExtensionMetadata,
                RequestLocal,
                ToolDefinition,
                LabelOnly,
            ),
            Self::BuiltinToolResult => (
                "builtin_tool_result",
                UntrustedExternalContent,
                SessionHistory,
                ToolChannelEnvelope,
                ContentRef,
            ),
            Self::AppToolResult => (
                "app_tool_result",
                UntrustedExternalContent,
                SessionHistory,
                ToolChannelEnvelope,
                ContentRef,
            ),
            Self::McpToolResult => (
                "mcp_tool_result",
                UntrustedExternalContent,
                SessionHistory,
                ToolChannelEnvelope,
                ContentRef,
            ),
            Self::WebPageContent => (
                "web_page_content",
                UntrustedExternalContent,
                SessionHistory,
                ToolChannelEnvelope,
                ContentRef,
            ),
            Self::MediaTranscript => (
                "media_transcript",
                UntrustedExternalContent,
                SessionHistory,
                ToolChannelEnvelope,
                LabelAndDigest,
            ),
            Self::TransientAppContext => (
                "transient_app_context",
                UntrustedExternalContent,
                InjectedAuditRow,
                UserChannelEnvelope,
                ContentRef,
            ),
            Self::ContextEvent => (
                "context_event",
                UntrustedExternalContent,
                RequestLocal,
                UserChannelEnvelope,
                LabelAndDigest,
            ),
            Self::HookOutput => (
                "hook_output",
                UntrustedExternalContent,
                RequestLocal,
                UserChannelEnvelope,
                LabelAndDigest,
            ),
            Self::ModelResponse => (
                "model_response",
                ModelGenerated,
                SessionHistory,
                AssistantChannel,
                ContentRef,
            ),
            Self::ModelCompressionSummary => (
                "model_compression_summary",
                ModelGenerated,
                SessionHistory,
                AssistantChannel,
                LabelAndDigest,
            ),
            Self::ModelReasoning => (
                "model_reasoning",
                ModelGenerated,
                SessionHistory,
                AssistantChannel,
                LabelOnly,
            ),
            Self::LegacyStoredRow => (
                "legacy_stored_row",
                LegacyUnknown,
                SessionHistory,
                UserChannelEnvelope,
                LabelAndDigest,
            ),
            Self::Unknown => (
                "unknown_source",
                LegacyUnknown,
                RequestLocal,
                UserChannelEnvelope,
                LabelAndDigest,
            ),
        };

        SourceProfile {
            kind: self,
            tag,
            class,
            persistence,
            projection,
            audit,
        }
    }

    /// The trust class this source confers. The only constructor of a
    /// class above [`TrustClass::parse_ceiling`].
    pub const fn class(self) -> TrustClass {
        self.profile().class
    }

    pub const fn tag(self) -> &'static str {
        self.profile().tag
    }

    pub const fn persistence(self) -> Persistence {
        self.profile().persistence
    }

    pub const fn projection(self) -> Projection {
        self.profile().projection
    }

    pub const fn audit(self) -> AuditStrategy {
        self.profile().audit
    }

    /// Recover a kind from a persisted tag. An unrecognised tag is
    /// [`SourceKind::Unknown`], never a trusted source.
    pub fn from_tag(raw: &str) -> Self {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.tag() == raw)
            .unwrap_or(Self::Unknown)
    }

    /// Classify the result of a registered tool by the tool's identity.
    ///
    /// Tool *identity* is decided by the registry before the model call
    /// and is not attacker-chosen, so it is a sound basis for a label —
    /// unlike the result body, which is. Every arm returns a class at
    /// or below [`TrustClass::UntrustedExternalContent`] except the two
    /// metadata arms, because no tool result is ever owner instruction
    /// or operator policy.
    ///
    /// The fallback is [`SourceKind::BuiltinToolResult`], which is
    /// still untrusted: a kernel primitive faithfully reports process
    /// names, file contents and network responses that a third party
    /// may control.
    pub fn for_tool_result(tool_name: &str) -> Self {
        // Remote MCP tools are registered as `mcp_<server>_<remote>`.
        if tool_name.starts_with("mcp_") {
            return Self::McpToolResult;
        }
        // The progressive bridge surfaces App/MCP tool definitions.
        if matches!(
            tool_name,
            "cos_tool_search" | "cos_tool_describe" | "cos_tool_call"
        ) {
            return Self::McpToolMetadata;
        }
        match tool_name {
            "cos_app_memory" => Self::AppMemory,
            "cos_app_catalog" => Self::AppToolMetadata,
            "cos_browser" => Self::WebPageContent,
            "cos_tts" | "cos_stt" | "cos_imagegen" | "cos_vision" => Self::MediaTranscript,
            "cos_skill" => Self::SkillInstructions,
            "cos_recall" | "cos_recall_semantic" | "cos_memory" => Self::RecalledMemory,
            "cos_todo" => Self::TodoList,
            // Every App surface: the generic gateway, session tools and
            // per-App proxies. The App, not the kernel, authored the
            // bytes, and a worker broker result is App output too.
            name if name.starts_with("cos_app_") || name.starts_with("app_") => Self::AppToolResult,
            _ => Self::BuiltinToolResult,
        }
    }
}

impl std::fmt::Display for SourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tag())
    }
}

/// A bounded, non-secret pointer back to where a segment came from.
///
/// The locator is projected through [`crate::audit_policy::safe_reference`],
/// so it can hold a tool name, an App id or an MCP server prefix but
/// never a credential, a raw URL with a query string, or a filesystem
/// path outside the identifier grammar.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceRef {
    kind: SourceKind,
    locator: Option<String>,
}

impl SourceRef {
    /// A reference naming only the kind.
    pub const fn new(kind: SourceKind) -> Self {
        Self {
            kind,
            locator: None,
        }
    }

    /// A reference naming the kind and a bounded locator.
    pub fn with_locator(kind: SourceKind, locator: &str) -> Self {
        let bounded = crate::audit_policy::safe_reference(locator);
        Self {
            kind,
            locator: Some(bounded),
        }
    }

    pub const fn kind(&self) -> SourceKind {
        self.kind
    }

    pub fn locator(&self) -> Option<&str> {
        self.locator.as_deref()
    }

    /// The class this reference confers. Derived from the kind, never
    /// from the locator.
    pub const fn class(&self) -> TrustClass {
        self.kind.class()
    }

    /// Single-line, secret-safe rendering for envelopes and audit rows.
    pub fn label(&self) -> String {
        match &self.locator {
            Some(locator) => format!("{}:{locator}", self.kind.tag()),
            None => self.kind.tag().to_string(),
        }
    }
}

impl std::fmt::Display for SourceRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}
