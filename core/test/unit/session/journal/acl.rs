use super::*;
use crate::session::journal::event::{Label, OperationId, Origin, RecoverySource};

fn mutation_started() -> JournalEvent {
    JournalEvent::MutationStarted {
        operation: OperationId::generate(),
        route: Label::new("system.package.install"),
        idempotency: crate::audit_policy::text_digest("key"),
        grant: None,
        session_mutation: None,
    }
}

fn tool_started() -> JournalEvent {
    JournalEvent::ToolStarted {
        turn: 1,
        tool: Label::new("cos_todo"),
        tool_use_id: Label::new("t1"),
        known: true,
    }
}

#[test]
fn kernel_may_record_every_kind() {
    assert!(EventSource::Kernel.may_write(&mutation_started()));
    assert!(EventSource::Kernel.may_write(&tool_started()));
    assert!(
        EventSource::Kernel.may_write(&JournalEvent::CapabilityExhausted {
            grant: crate::session::journal::event::Reference::new("g-1"),
            reason: crate::session::journal::event::GrantEnd::UsesExhausted,
        })
    );
}

#[test]
fn a_worker_may_record_only_model_and_tool_lifecycle() {
    assert!(EventSource::Worker.may_write(&tool_started()));
    assert!(
        EventSource::Worker.may_write(&JournalEvent::ModelTurnCompleted {
            turn: 0,
            provider: Label::new("anthropic"),
            model: Label::new("claude"),
            success: true,
            latency_ms: 1,
            input_tokens: 1,
            output_tokens: 1,
            tool_calls: 0,
            stop_reason: Label::new("end_turn"),
            error: None,
        })
    );
}

#[test]
fn a_worker_may_not_forge_a_privileged_kind() {
    // The whole point of the private channel: a compromised worker can
    // say what its model did, and nothing about what the system did.
    assert!(!EventSource::Worker.may_write(&mutation_started()));
    assert!(
        !EventSource::Worker.may_write(&JournalEvent::CapabilityIssued {
            grant: crate::session::journal::event::Reference::new("g-1"),
            audience: Label::new("daemon"),
            issuer: Label::new("daemon"),
            caps: 1,
            uses: None,
        })
    );
    assert!(
        !EventSource::Worker.may_write(&JournalEvent::ApprovalDecided {
            approval: crate::session::journal::event::Reference::new("ap-1"),
            verb: Label::new("fs.write"),
            outcome: crate::session::journal::event::ApprovalOutcome::Approved,
            generation: 0,
        })
    );
    assert!(
        !EventSource::Worker.may_write(&JournalEvent::SessionStarted {
            owner_uid: 0,
            origin: Origin::System,
            delegation: None,
            parent: None,
        })
    );
}

#[test]
fn recovery_may_record_only_what_it_concludes() {
    assert!(
        EventSource::Recovery.may_write(&JournalEvent::MutationOrphaned {
            operation: OperationId::generate(),
            route: Label::new("system.service.control"),
            detected_by: RecoverySource::DaemonStart,
            opened_in_epoch: 1,
        })
    );
    assert!(!EventSource::Recovery.may_write(&mutation_started()));
    assert!(!EventSource::Recovery.may_write(&tool_started()));
}

#[test]
fn only_privileged_sources_may_spend_the_reserve() {
    assert!(EventSource::Kernel.is_reserved_capacity());
    assert!(EventSource::Recovery.is_reserved_capacity());
    assert!(
        !EventSource::Worker.is_reserved_capacity(),
        "worker volume must never be able to consume the space that closes a mutation"
    );
}
