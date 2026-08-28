//! Session-scoped projection of the model-visible tool catalogue.
//!
//! Tool descriptors are immutable data cached by the registry. Whether a
//! descriptor is visible is recomputed from one [`ToolExposureContext`] for
//! every agent request. The context is built by trusted runtime entry points;
//! model input, request JSON and ambient environment variables never select
//! authority.

use std::collections::BTreeSet;
use std::future::Future;

use sha2::{Digest, Sha256};

use crate::agent::tools::guardrails::Guardrails;
use crate::caps::{Cap, CapSet, Verb};
use crate::proc::SessionInfo;
use crate::session::{SessionClient, SessionPresence, SessionSource};

use super::SYSTEM_AGENT_MEMORY_SCOPE;

/// Execution boundary a tool needs in order to be reachable.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ToolTransport {
    LocalProcess,
    AppSession,
    McpStdio,
    McpHttp,
    InteractiveAuthorization,
}

impl ToolTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalProcess => "local-process",
            Self::AppSession => "app-session",
            Self::McpStdio => "mcp-stdio",
            Self::McpHttp => "mcp-http",
            Self::InteractiveAuthorization => "interactive-authorization",
        }
    }
}

/// Where the model/tool loop itself executes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionHost {
    Direct,
    AgentWorker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryExposure {
    SystemAgent,
    SystemAgentOrSession,
    SystemAgentOrApp,
}

/// Trusted identity and authority facts for one model request.
#[derive(Clone, Debug)]
pub struct ToolExposureContext {
    authority_session_id: String,
    conversation_session_id: Option<String>,
    task_id: Option<String>,
    owner_uid: u32,
    app_id: Option<String>,
    client: SessionClient,
    presence: Option<SessionPresence>,
    capabilities: CapSet,
    capability_generation: String,
    host: ExecutionHost,
    transports: BTreeSet<ToolTransport>,
    enabled_extensions: BTreeSet<String>,
    guardrails: Guardrails,
}

impl ToolExposureContext {
    /// Build from a session whose process identity has already been installed
    /// or authenticated by the runtime.
    pub(crate) fn from_trusted_session(
        session: &SessionInfo,
        conversation_session_id: Option<&str>,
        task_id: Option<&str>,
        owner_uid: u32,
        host: ExecutionHost,
        guardrails: Guardrails,
    ) -> Self {
        Self::from_trusted_session_with_presence(
            session,
            conversation_session_id,
            task_id,
            owner_uid,
            host,
            guardrails,
            None,
        )
    }

    pub(crate) fn from_trusted_session_with_presence(
        session: &SessionInfo,
        conversation_session_id: Option<&str>,
        task_id: Option<&str>,
        owner_uid: u32,
        host: ExecutionHost,
        guardrails: Guardrails,
        presence: Option<SessionPresence>,
    ) -> Self {
        let capabilities = session.caps.clone().unwrap_or_default();
        let mut transports = BTreeSet::from([
            ToolTransport::LocalProcess,
            ToolTransport::McpStdio,
            ToolTransport::McpHttp,
        ]);
        if host == ExecutionHost::Direct && session.client.local {
            transports.insert(ToolTransport::AppSession);
        }
        if session.client.local && (session.client.attended || presence.is_some()) {
            transports.insert(ToolTransport::InteractiveAuthorization);
        }
        Self {
            authority_session_id: session.session_id.clone(),
            conversation_session_id: conversation_session_id.map(str::to_string),
            task_id: task_id.map(str::to_string),
            owner_uid,
            app_id: session.app_id.clone(),
            client: session.client,
            presence,
            capability_generation: capability_generation(&capabilities),
            capabilities,
            host,
            transports,
            enabled_extensions: BTreeSet::new(),
            guardrails,
        }
    }

    /// Resolve and verify the current process's registered session before
    /// constructing an exposure context. `COS_SESSION` can select a candidate
    /// row, but the pid/start-time/ancestry check decides whether it is usable.
    /// A task-local trusted override was already checked against the signed
    /// `agentd` assignment and is consumed directly.
    pub(crate) fn from_current_session(
        conversation_session_id: Option<&str>,
        task_id: Option<&str>,
        host: ExecutionHost,
        guardrails: Guardrails,
    ) -> Result<Self, String> {
        let session = match crate::proc::current_trusted_session_for_caps() {
            Some(session) => session,
            None => {
                let session = crate::proc::current_session_info_for_caps().ok_or_else(|| {
                    "tool exposure requires an authenticated session".to_string()
                })?;
                crate::caps::enforcement::require_current_session_identity(
                    &session.session_id,
                    session.pid,
                )
                .map_err(|error| format!("tool exposure session identity failed: {error}"))?;
                session
            }
        };
        let owner_uid = crate::paths::current_owner_uid_override()
            .or_else(current_euid)
            .ok_or_else(|| "tool exposure owner uid is unavailable".to_string())?;
        Ok(Self::from_trusted_session(
            &session,
            conversation_session_id,
            task_id,
            owner_uid,
            host,
            guardrails,
        ))
    }

    pub(crate) fn from_current_session_with_presence(
        conversation_session_id: Option<&str>,
        task_id: Option<&str>,
        host: ExecutionHost,
        guardrails: Guardrails,
        presence: Option<SessionPresence>,
    ) -> Result<Self, String> {
        let mut context =
            Self::from_current_session(conversation_session_id, task_id, host, guardrails)?;
        context.presence = presence;
        if context.client.local && context.attended_now() {
            context
                .transports
                .insert(ToolTransport::InteractiveAuthorization);
        } else {
            context
                .transports
                .remove(&ToolTransport::InteractiveAuthorization);
        }
        Ok(context)
    }

    /// Fail-closed context for library calls that have no authenticated
    /// session. Only tools without authority/reachability requirements appear.
    pub(crate) fn isolated(guardrails: Guardrails) -> Self {
        Self {
            authority_session_id: String::new(),
            conversation_session_id: None,
            task_id: None,
            owner_uid: 0,
            app_id: None,
            client: SessionClient::default(),
            presence: None,
            capabilities: CapSet::new(),
            capability_generation: capability_generation(&CapSet::new()),
            host: ExecutionHost::Direct,
            transports: BTreeSet::from([ToolTransport::LocalProcess]),
            enabled_extensions: BTreeSet::new(),
            guardrails,
        }
    }

    /// An authenticated web request is attended for that request only. The
    /// long-lived server process remains unattended outside this clone.
    pub(crate) fn for_authenticated_web_request(mut self, local: bool) -> Self {
        self.client.source = SessionSource::LocalWeb;
        self.client.attended = true;
        self.client.local = local;
        self.presence = None;
        if local {
            self.transports
                .insert(ToolTransport::InteractiveAuthorization);
        } else {
            self.transports
                .remove(&ToolTransport::InteractiveAuthorization);
            self.transports.remove(&ToolTransport::AppSession);
        }
        self
    }

    pub(crate) fn for_external_mcp(mut self) -> Self {
        self.client.source = SessionSource::ExternalMcp;
        self.client.attended = false;
        self.presence = None;
        self.transports
            .remove(&ToolTransport::InteractiveAuthorization);
        self
    }

    pub(crate) fn delegated(mut self, guardrails: Guardrails) -> Self {
        self.client.source = SessionSource::DelegatedAgent;
        self.client.attended = false;
        self.presence = None;
        self.transports
            .remove(&ToolTransport::InteractiveAuthorization);
        self.guardrails = guardrails;
        self.task_id = None;
        self
    }

    pub(crate) fn enable_extension(&mut self, extension: impl Into<String>) {
        self.enabled_extensions.insert(extension.into());
    }

    pub(crate) fn set_conversation_session_id(&mut self, session_id: impl Into<String>) {
        self.conversation_session_id = Some(session_id.into());
    }

    #[cfg(test)]
    pub(crate) fn with_capabilities(mut self, capabilities: CapSet) -> Self {
        self.capability_generation = capability_generation(&capabilities);
        self.capabilities = capabilities;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_identity(
        mut self,
        session_id: impl Into<String>,
        owner_uid: u32,
        source: SessionSource,
    ) -> Self {
        self.authority_session_id = session_id.into();
        self.owner_uid = owner_uid;
        self.client.source = source;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_presence(mut self, attended: bool, local: bool) -> Self {
        self.client.attended = attended;
        self.client.local = local;
        if attended && local {
            self.transports
                .insert(ToolTransport::InteractiveAuthorization);
        } else {
            self.transports
                .remove(&ToolTransport::InteractiveAuthorization);
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn with_transport(mut self, transport: ToolTransport, available: bool) -> Self {
        if available {
            self.transports.insert(transport);
        } else {
            self.transports.remove(&transport);
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn with_host(mut self, host: ExecutionHost) -> Self {
        self.host = host;
        match host {
            ExecutionHost::Direct => {
                self.transports.insert(ToolTransport::AppSession);
            }
            ExecutionHost::AgentWorker => {
                self.transports.remove(&ToolTransport::AppSession);
            }
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn with_presence_lease(mut self, presence: SessionPresence) -> Self {
        self.presence = Some(presence);
        self.transports
            .insert(ToolTransport::InteractiveAuthorization);
        self
    }

    pub fn authority_session_id(&self) -> &str {
        &self.authority_session_id
    }

    pub fn conversation_session_id(&self) -> Option<&str> {
        self.conversation_session_id.as_deref()
    }

    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    pub fn owner_uid(&self) -> u32 {
        self.owner_uid
    }

    pub fn app_id(&self) -> Option<&str> {
        self.app_id.as_deref()
    }

    pub fn client(&self) -> SessionClient {
        let mut client = self.client;
        client.attended = self.attended_now();
        client
    }

    pub fn capabilities(&self) -> &CapSet {
        &self.capabilities
    }

    pub fn capability_generation(&self) -> &str {
        &self.capability_generation
    }

    pub fn host(&self) -> ExecutionHost {
        self.host
    }

    pub fn guardrails(&self) -> &Guardrails {
        &self.guardrails
    }

    pub fn has_transport(&self, transport: ToolTransport) -> bool {
        self.transports.contains(&transport)
    }

    pub fn extension_enabled(&self, extension: &str) -> bool {
        self.enabled_extensions.contains(extension)
    }

    pub fn transports(&self) -> impl Iterator<Item = ToolTransport> + '_ {
        self.transports.iter().copied()
    }

    pub fn enabled_extensions(&self) -> impl Iterator<Item = &str> {
        self.enabled_extensions.iter().map(String::as_str)
    }

    pub fn is_attended_local(&self) -> bool {
        self.attended_now()
            && self.client.local
            && self.has_transport(ToolTransport::InteractiveAuthorization)
    }

    fn attended_now(&self) -> bool {
        match self.presence {
            Some(presence) => {
                presence.owner_uid == self.owner_uid
                    && now_ms() <= presence.expires_at_ms
                    && crate::proc::process_identity_is_live(
                        presence.pid,
                        presence.start_time_ticks,
                        presence.owner_uid,
                    )
            }
            None => self.client.attended,
        }
    }

    pub fn permits_interactive_authorization(&self) -> bool {
        self.is_attended_local()
            && matches!(
                self.client.source,
                SessionSource::LocalCli | SessionSource::LocalWeb | SessionSource::BrokerTask
            )
    }
}

/// Immutable exposure requirements cached beside a tool descriptor.
#[derive(Clone, Debug, Default)]
pub struct ToolExposure {
    all_verbs: Vec<Verb>,
    any_verbs: Vec<Verb>,
    exact_caps: Vec<Cap>,
    any_exact_caps: Option<Vec<Cap>>,
    sources: Option<Vec<SessionSource>>,
    transport: Option<ToolTransport>,
    attended_local: bool,
    extension: Option<String>,
    memory: Option<(Vec<Verb>, MemoryExposure)>,
}

impl ToolExposure {
    pub fn always() -> Self {
        Self::default()
    }

    pub fn requiring_all_verbs(mut self, verbs: impl IntoIterator<Item = Verb>) -> Self {
        self.all_verbs.extend(verbs);
        self
    }

    pub fn requiring_any_verb(mut self, verbs: impl IntoIterator<Item = Verb>) -> Self {
        self.any_verbs.extend(verbs);
        self
    }

    pub fn requiring_caps(mut self, caps: impl IntoIterator<Item = Cap>) -> Self {
        self.exact_caps.extend(caps);
        self
    }

    pub fn requiring_any_cap(mut self, caps: impl IntoIterator<Item = Cap>) -> Self {
        self.any_exact_caps = Some(caps.into_iter().collect());
        self
    }

    pub fn from_sources(mut self, sources: impl IntoIterator<Item = SessionSource>) -> Self {
        self.sources = Some(sources.into_iter().collect());
        self
    }

    pub fn requiring_transport(mut self, transport: ToolTransport) -> Self {
        self.transport = Some(transport);
        self
    }

    pub fn requiring_attended_local(mut self) -> Self {
        self.attended_local = true;
        self
    }

    pub fn requiring_extension(mut self, extension: impl Into<String>) -> Self {
        self.extension = Some(extension.into());
        self
    }

    pub fn requiring_memory(
        mut self,
        verbs: impl IntoIterator<Item = Verb>,
        scope: MemoryExposure,
    ) -> Self {
        self.memory = Some((verbs.into_iter().collect(), scope));
        self
    }

    pub fn decide(&self, context: &ToolExposureContext) -> ExposureDecision {
        if self.attended_local && !context.is_attended_local() {
            return ExposureDecision::Hidden("requires an attended local session".to_string());
        }
        if let Some(sources) = &self.sources {
            if !sources.contains(&context.client.source) {
                return ExposureDecision::Hidden(format!(
                    "unavailable from source {:?}",
                    context.client.source
                ));
            }
        }
        if let Some(transport) = self.transport {
            if !context.has_transport(transport) {
                return ExposureDecision::Hidden(format!(
                    "execution transport {transport:?} is unavailable"
                ));
            }
        }
        if let Some(extension) = &self.extension {
            if !context.extension_enabled(extension) {
                return ExposureDecision::Hidden(format!(
                    "extension `{extension}` is not enabled for this session"
                ));
            }
        }
        if let Some((verbs, scope)) = &self.memory {
            let candidates = memory_scope_candidates(context, *scope);
            if !verbs.iter().any(|verb| {
                candidates.iter().any(|scope| {
                    context
                        .capabilities
                        .covers(&Cap::new(*verb, scope.clone()))
                })
            }) {
                return ExposureDecision::Hidden(
                    "required memory scope is not in the effective grant".to_string(),
                );
            }
        }
        let held_verbs = context.capabilities.verbs();
        if self.all_verbs.iter().any(|verb| !held_verbs.contains(verb)) {
            return ExposureDecision::Hidden(
                "required capability verb is not in the effective grant".to_string(),
            );
        }
        if !self.any_verbs.is_empty()
            && !self.any_verbs.iter().any(|verb| held_verbs.contains(verb))
        {
            return ExposureDecision::Hidden(
                "no usable capability verb is in the effective grant".to_string(),
            );
        }
        if self
            .exact_caps
            .iter()
            .any(|cap| !context.capabilities.covers(cap))
        {
            return ExposureDecision::Hidden(
                "required capability scope is not in the effective grant".to_string(),
            );
        }
        if let Some(caps) = &self.any_exact_caps {
            if !caps.iter().any(|cap| context.capabilities.covers(cap)) {
                return ExposureDecision::Hidden(
                    "no usable capability scope is in the effective grant".to_string(),
                );
            }
        }
        ExposureDecision::Visible
    }
}

fn memory_scope_candidates(
    context: &ToolExposureContext,
    exposure: MemoryExposure,
) -> Vec<crate::caps::Scope> {
    let mut scopes = vec![crate::caps::Scope::self_ref(SYSTEM_AGENT_MEMORY_SCOPE)];
    match exposure {
        MemoryExposure::SystemAgent => {}
        MemoryExposure::SystemAgentOrSession => {
            if let Some(session_id) = context.conversation_session_id().or_else(|| {
                (!context.authority_session_id().is_empty())
                    .then(|| context.authority_session_id())
            }) {
                scopes.push(crate::caps::Scope::self_ref(session_id));
            }
        }
        MemoryExposure::SystemAgentOrApp => {
            if let Some(app_id) = context.app_id() {
                scopes.push(crate::caps::Scope::self_ref(app_id));
            }
        }
    }
    scopes
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExposureDecision {
    Visible,
    Hidden(String),
}

impl ExposureDecision {
    pub fn is_visible(&self) -> bool {
        matches!(self, Self::Visible)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Visible => None,
            Self::Hidden(reason) => Some(reason),
        }
    }
}

tokio::task_local! {
    static ACTIVE_CONTEXT: ToolExposureContext;
}

pub(crate) async fn scope<F>(context: ToolExposureContext, future: F) -> F::Output
where
    F: Future,
{
    ACTIVE_CONTEXT.scope(context, future).await
}

pub(crate) fn current() -> Option<ToolExposureContext> {
    ACTIVE_CONTEXT.try_with(Clone::clone).ok()
}

pub fn capability_generation(caps: &CapSet) -> String {
    let mut encoded: Vec<String> = caps
        .iter()
        .map(|cap| serde_json::to_string(cap).unwrap_or_default())
        .collect();
    encoded.sort_unstable();
    let mut digest = Sha256::new();
    for cap in encoded {
        digest.update((cap.len() as u64).to_be_bytes());
        digest.update(cap.as_bytes());
    }
    hex::encode(&digest.finalize()[..8])
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn current_euid() -> Option<u32> {
    Some(unsafe { libc::geteuid() as u32 })
}

#[cfg(not(unix))]
fn current_euid() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/exposure.rs"
    ));
}
