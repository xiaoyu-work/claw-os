use super::*;

fn cfg() -> HeartbeatConfig {
    HeartbeatConfig::default()
}

#[test]
fn healthy_vitals_emit_nothing() {
    let v = Vitals {
        load1: Some(0.5),
        cpus: Some(8),
        mem_avail_ratio: Some(0.8),
    };
    assert!(evaluate(&v, &cfg()).is_empty());
}

#[test]
fn high_load_warns_then_criticals() {
    let c = cfg(); // warn at 4.0/core, critical at 8.0/core
    let warn = Vitals {
        load1: Some(8.0 * 4.0),
        cpus: Some(8),
        mem_avail_ratio: Some(0.9),
    }; // 4.0/core → warn
    let sigs = evaluate(&warn, &c);
    assert_eq!(sigs.len(), 1);
    assert_eq!(sigs[0].key, "load_high");
    assert_eq!(sigs[0].severity, Severity::Warn);

    let crit = Vitals {
        load1: Some(8.0 * 8.0),
        cpus: Some(8),
        mem_avail_ratio: Some(0.9),
    }; // 8.0/core → critical
    assert_eq!(evaluate(&crit, &c)[0].severity, Severity::Critical);
}

#[test]
fn low_memory_warns_and_criticals() {
    let c = cfg(); // warn at 0.10, critical at 0.05
    let warn = Vitals {
        load1: Some(0.1),
        cpus: Some(8),
        mem_avail_ratio: Some(0.08),
    };
    let sigs = evaluate(&warn, &c);
    assert_eq!(sigs.len(), 1);
    assert_eq!(sigs[0].key, "memory_low");
    assert_eq!(sigs[0].severity, Severity::Warn);

    let crit = Vitals {
        load1: Some(0.1),
        cpus: Some(8),
        mem_avail_ratio: Some(0.02),
    };
    assert_eq!(evaluate(&crit, &c)[0].severity, Severity::Critical);
}

#[test]
fn missing_vitals_produce_no_signals() {
    // Non-Linux / no-/proc host: every field None ⇒ nothing to emit.
    assert!(evaluate(&Vitals::default(), &cfg()).is_empty());
}

#[test]
fn cooldown_blocks_refire_until_window_elapses() {
    let mut cd = CooldownState::default();
    let cooldown = Duration::from_secs(900);
    let t0 = Instant::now();
    assert!(cd.allow("load_high", cooldown, t0), "first fire allowed");
    assert!(
        !cd.allow("load_high", cooldown, t0 + Duration::from_secs(60)),
        "re-fire within cooldown blocked"
    );
    assert!(
        cd.allow("load_high", cooldown, t0 + Duration::from_secs(901)),
        "re-fire after cooldown allowed"
    );
}

#[test]
fn cooldown_is_per_key() {
    let mut cd = CooldownState::default();
    let cooldown = Duration::from_secs(900);
    let t0 = Instant::now();
    assert!(cd.allow("load_high", cooldown, t0));
    // A different signal key is independent.
    assert!(cd.allow("memory_low", cooldown, t0));
}

#[test]
fn severity_changes_and_recovery_bypass_the_cooldown() {
    let mut state = CooldownState::default();
    let cooldown = Duration::from_secs(900);
    let now = Instant::now();
    assert!(state.enter_or_refire("memory_low", Severity::Warn, cooldown, now));
    assert!(!state.enter_or_refire(
        "memory_low",
        Severity::Warn,
        cooldown,
        now + Duration::from_secs(10)
    ));
    assert!(state.enter_or_refire(
        "memory_low",
        Severity::Critical,
        cooldown,
        now + Duration::from_secs(20)
    ));
    assert!(state.recover("memory_low"));
    assert!(!state.recover("memory_low"));
}

#[test]
fn from_env_default_is_enabled_60s() {
    let c = HeartbeatConfig::default();
    assert!(c.enabled);
    assert_eq!(c.interval, Duration::from_secs(60));
}
