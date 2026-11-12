//! One-use exact package authority from an already consumed protected decision.

use super::{
    Audience, AudienceSet, Binding, Decision, GrantId, Issuance, Issuer,
    Presentation, Principal, Requirement, Uses, authority,
};

pub(crate) struct PackageDecision {
    decision: Decision,
    grant: GrantId,
}

impl PackageDecision {
    pub(crate) fn decision(&self) -> &Decision { &self.decision }
}

impl Drop for PackageDecision {
    fn drop(&mut self) { authority().revoke(self.grant); }
}

pub(crate) fn authorize(
    approved: &crate::clawd::packages::gui::ApprovedIntent,
    client: &crate::clawd::client_identity::ClientIdentity,
) -> Result<PackageDecision, String> {
    let crate::clawd::packages::gui::IntentBinding {
        registration, process, gui, required, deadline, generation,
    } = approved.validate()?;
    let owner = registration.owner();
    let pid = client.pid.ok_or("package intent peer PID is unavailable")?;
    if client.require_uid()? != owner || client.execution_uid.is_some()
        || client.extension_host.is_some()
        || gui.subject.session_id.as_deref() != Some(registration.session_id.as_str())
        || gui.subject.app_id.as_deref() != Some(registration.launch.app_id())
        || !gui.audience.contains(Audience::GuiResource)
        || !gui.audience.contains(Audience::SystemService)
        || required.is_empty()
        || required.iter().any(|cap| cap.verb != crate::caps::Verb::SYS_PACKAGE
            || !matches!(cap.scope, crate::caps::Scope::Name(_)))
    { return Err("protected package decision does not match the live GUI".into()); }
    let principal = Principal::of_process(owner, process.pid() as u32)
        .filter(|principal| principal.start_time_ticks == Some(process.start_ticks()))
        .ok_or("package intent GUI process identity changed")?;
    let session = crate::paths::RoutedPathContext::for_owner(owner, registration.home.clone())
        .scope_sync(|| crate::proc::session_info_by_id(&registration.session_id))
        .ok_or("package intent GUI session is unavailable")?;
    if session.pid != process.pid() as u32
        || session.start_time_ticks != Some(process.start_ticks())
        || session.app_id.as_deref() != Some(registration.launch.app_id())
        || session.pending_bind
    { return Err("package intent registry binding changed".into()); }
    let lifetime = deadline.saturating_duration_since(std::time::Instant::now());
    if lifetime.is_zero() { return Err("package intent GUI authority expired".into()); }
    let (_, view) = authority().issue_with_generation_until(Issuance {
        issuer: Issuer::Approval,
        principal,
        binding: Binding::ProcessTree,
        subject: gui.subject,
        audience: AudienceSet::one(Audience::SystemService),
        caps: crate::caps::CapSet::from_caps(required.iter().cloned()),
        lifetime,
        uses: Uses::Budget(1),
        index_session: false,
    }, u64::from(generation), Some(deadline)).map_err(|error| error.to_string())?;
    super::audit::record_issued(&view, None);
    if let Err(error) = approved.validate() {
        authority().revoke(view.id);
        return Err(error);
    }
    let grant = view.id;
    let decision = Decision::new(
        view, "system.package.control", Audience::SystemService,
        Presentation {
            uid: owner, pid, start_time_ticks: client.start_time_ticks,
            audience: Audience::SystemService, route: "system.package.control",
            session_id: Some(registration.session_id.clone()),
        }, None, Some(session), &Requirement::RouteDerived,
    );
    Ok(PackageDecision { decision, grant })
}
