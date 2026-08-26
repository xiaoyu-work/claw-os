use super::*;

fn read(scope: Scope) -> Cap {
    Cap::new(Verb::FS_READ, scope)
}

#[test]
fn cap_covers_same_verb_and_scope_cover() {
    let granted = read(Scope::path("/home/jay/**"));
    let requested = read(Scope::path("/home/jay/notes.md"));
    assert!(granted.covers(&requested));
}

#[test]
fn cap_does_not_cover_different_verb() {
    let granted = read(Scope::Wild);
    let requested = Cap::new(Verb::FS_WRITE, Scope::Wild);
    assert!(!granted.covers(&requested));
}

#[test]
fn capset_dedupes_inserts() {
    let mut set = CapSet::new();
    set.insert(read(Scope::Wild));
    set.insert(read(Scope::Wild));
    assert_eq!(set.len(), 1);
}

#[test]
fn capset_covers_any_matching_member() {
    let set = CapSet::from_caps([
        read(Scope::path("/home/jay/docs/**")),
        Cap::new(Verb::NET_DIAL, Scope::host("*.github.com:443")),
    ]);
    assert!(set.covers(&read(Scope::path("/home/jay/docs/a.txt"))));
    assert!(set.covers(&Cap::new(Verb::NET_DIAL, Scope::host("api.github.com:443"))));
    assert!(!set.covers(&Cap::new(Verb::FS_DELETE, Scope::path("/home/jay/docs/a.txt"))));
}

#[test]
fn covers_all_requires_every_requested_cap() {
    let parent = CapSet::from_caps([
        read(Scope::path("/home/jay/**")),
        Cap::new(Verb::FS_WRITE, Scope::path("/home/jay/inbox/**")),
    ]);
    let allowed_child = CapSet::from_caps([
        read(Scope::path("/home/jay/inbox/x.txt")),
        Cap::new(Verb::FS_WRITE, Scope::path("/home/jay/inbox/x.txt")),
    ]);
    let escalating_child = CapSet::from_caps([
        read(Scope::path("/home/jay/inbox/x.txt")),
        Cap::new(Verb::FS_DELETE, Scope::path("/home/jay/inbox/x.txt")),
    ]);
    assert!(parent.covers_all(&allowed_child));
    assert!(!parent.covers_all(&escalating_child));
}

#[test]
fn intersect_returns_only_covered_caps() {
    let parent = CapSet::from_caps([
        read(Scope::path("/home/jay/**")),
    ]);
    let requested = CapSet::from_caps([
        read(Scope::path("/home/jay/notes.md")),
        Cap::new(Verb::FS_DELETE, Scope::path("/home/jay/notes.md")), // not granted
    ]);
    let allowed = parent.intersect(&requested);
    assert_eq!(allowed.len(), 1);
    assert!(allowed.covers(&read(Scope::path("/home/jay/notes.md"))));
}

#[test]
fn verbs_returns_distinct_verbs() {
    let set = CapSet::from_caps([
        read(Scope::path("/a/**")),
        read(Scope::path("/b/**")),
        Cap::new(Verb::NET_DIAL, Scope::host("*.example.com")),
    ]);
    let verbs = set.verbs();
    assert_eq!(verbs.len(), 2);
    assert!(verbs.contains(&Verb::FS_READ));
    assert!(verbs.contains(&Verb::NET_DIAL));
}

#[test]
fn serde_round_trip() {
    let set = CapSet::from_caps([
        read(Scope::path("/home/jay/**")),
        Cap::new(Verb::NET_DIAL, Scope::host("*.github.com:443")),
        Cap::unscoped(Verb::UI_NOTIFY),
    ]);
    let json = serde_json::to_string(&set).unwrap();
    let back: CapSet = serde_json::from_str(&json).unwrap();
    assert_eq!(back, set);
}
