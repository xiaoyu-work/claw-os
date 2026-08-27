use super::*;

fn limits() -> Limits {
    Limits {
        max_connections: 3,
        max_connections_per_user: 2,
        max_connections_for_root: 3,
        max_in_flight: 3,
        max_in_flight_per_user: 2,
        max_in_flight_for_root: 3,
        read_deadline: Duration::from_millis(10),
        write_deadline: Duration::from_millis(10),
        duplicate_window: Duration::from_secs(60),
    }
}

#[test]
fn connections_are_bounded_per_user_and_globally() {
    let admission = Admission::new(limits());
    let a1 = admission.accept_connection(1000).expect("first");
    let a2 = admission.accept_connection(1000).expect("second");
    assert!(
        admission.accept_connection(1000).is_none(),
        "one user must not be able to hold every slot"
    );

    // Another principal still gets in, up to the global ceiling.
    let b1 = admission.accept_connection(1001).expect("other user");
    assert!(
        admission.accept_connection(1002).is_none(),
        "global ceiling"
    );

    drop(a1);
    let _a3 = admission
        .accept_connection(1000)
        .expect("a released slot is reusable");
    drop((a2, b1));
}

#[test]
fn root_gets_a_larger_but_still_finite_allowance() {
    let admission = Admission::new(limits());
    let _r1 = admission.accept_connection(0).expect("root 1");
    let _r2 = admission.accept_connection(0).expect("root 2");
    let _r3 = admission
        .accept_connection(0)
        .expect("root reaches its own ceiling, not the per-user one");
    assert!(
        admission.accept_connection(0).is_none(),
        "root is bounded too"
    );
}

#[test]
fn in_flight_requests_are_bounded_per_user() {
    let admission = Admission::new(limits());
    let first = admission.accept_request(1000).expect("first");
    let second = admission.accept_request(1000).expect("second");
    assert_eq!(
        admission.accept_request(1000).err(),
        Some(Fault::TooManyRequests)
    );
    drop(first);
    let _third = admission.accept_request(1000).expect("released");
    drop(second);
}

#[test]
fn a_route_cannot_exceed_its_declared_budget() {
    let admission = Admission::new(limits());
    let route = Command::DaemonHealth.route();
    let mut held = Vec::new();
    for _ in 0..route.budget.max_in_flight {
        held.push(admission.accept_route(route).expect("within budget"));
    }
    assert_eq!(admission.accept_route(route).err(), Some(Fault::RouteBusy));

    // A different route has its own budget and is unaffected.
    let other = Command::SystemPackageInstall.route();
    let _other = admission.accept_route(other).expect("independent budget");

    held.pop();
    let _reused = admission.accept_route(route).expect("released");
}

#[test]
fn a_repeated_mutation_id_is_refused_inside_the_window() {
    let admission = Admission::new(limits());
    let key = mutation_key(1000, 42, 99, Command::TaskSubmit, "r1");
    assert_eq!(admission.admit_mutation(key), Ok(()));
    assert_eq!(
        admission.admit_mutation(key),
        Err(Fault::DuplicateRequest),
        "a replayed frame must not repeat a privileged mutation"
    );
}

#[test]
fn a_fresh_id_or_a_different_principal_is_not_a_duplicate() {
    let admission = Admission::new(limits());
    assert_eq!(
        admission.admit_mutation(mutation_key(1000, 42, 99, Command::TaskSubmit, "r1")),
        Ok(())
    );
    // Same principal, next request: a retry after an approval is not a
    // replay, because the client mints a new correlation id.
    assert_eq!(
        admission.admit_mutation(mutation_key(1000, 42, 99, Command::TaskSubmit, "r2")),
        Ok(())
    );
    // Same id, different uid, pid, start time or route: all distinct.
    assert_eq!(
        admission.admit_mutation(mutation_key(1001, 42, 99, Command::TaskSubmit, "r1")),
        Ok(())
    );
    assert_eq!(
        admission.admit_mutation(mutation_key(1000, 43, 99, Command::TaskSubmit, "r1")),
        Ok(())
    );
    assert_eq!(
        admission.admit_mutation(mutation_key(1000, 42, 100, Command::TaskSubmit, "r1")),
        Ok(())
    );
    assert_eq!(
        admission.admit_mutation(mutation_key(1000, 42, 99, Command::TaskCancel, "r1")),
        Ok(())
    );
}

#[test]
fn the_duplicate_record_never_grows_past_its_capacity() {
    let admission = Admission::new(limits());
    for index in 0..(DUPLICATE_CAPACITY * 4) {
        let key = mutation_key(1000, 42, 99, Command::TaskSubmit, &format!("r{index}"));
        assert_eq!(admission.admit_mutation(key), Ok(()));
    }
    let guard = admission.duplicates.lock().unwrap();
    assert_eq!(guard.entries.len(), DUPLICATE_CAPACITY);
}

#[test]
fn an_expired_entry_stops_matching() {
    let mut ring = Duplicates::new();
    let now = Instant::now();
    assert!(ring.admit(7, now, Duration::from_secs(60)));
    assert!(!ring.admit(7, now, Duration::from_secs(60)));
    assert!(ring.admit(7, now + Duration::from_secs(61), Duration::from_secs(60)));
}
