//! Capability scope — the *where* / *to what* half of a capability.
//!
//! Each capability is "verb + scope". The scope says **which** files,
//! **which** hosts, **which** secrets, etc. are covered. There are a
//! few distinct flavours of scope; mixing them makes no sense
//! (a path glob can't cover a hostname), so [`Scope`] is a sum type.
//!
//! ## Cover semantics
//!
//! `granted.covers(&requested) == true` means a session that holds the
//! `granted` scope is allowed to perform an action that needs the
//! `requested` scope. The default rules:
//!
//! - [`Scope::Wild`] covers anything of any kind. It is the explicit
//!   "I trust you with the whole world" answer and must be authored
//!   on purpose — there is no implicit wildcard.
//! - [`Scope::Path`]: glob match with `*` matching a single path
//!   segment and `**` matching any number of segments. Tildes and
//!   `$VAR` are expanded before matching.
//! - [`Scope::Host`]: `*` matches a single DNS label between dots,
//!   `**` matches multiple labels. Port match is exact when both
//!   sides specify one; granted-without-port covers any port.
//! - [`Scope::Name`]: glob, `/`-segmented (used for secrets like
//!   `openai/*` or db topics like `billing.*`).
//! - [`Scope::SelfRef`]: literal equality (so `self.children.*`
//!   never accidentally matches another session's id).
//!
//! Scopes of different kinds never cover each other (except `Wild`).

use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum Scope {
    /// Filesystem glob. `~` and `$VAR` are expanded at match time.
    Path(String),
    /// `host[:port]` glob. `*` matches one DNS label.
    Host(String),
    /// Named resource glob (secret names, db topics, kv prefixes…).
    Name(String),
    /// Self-reference (e.g. `self`, `self.children`, `self.<sid>`).
    SelfRef(String),
    /// Explicit wildcard — covers any other scope of any kind.
    Wild,
}

/// Discriminator without payload — useful for catalog declarations
/// ("this verb only accepts `Path` scopes") and for diagnostics.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeKind {
    Path,
    Host,
    Name,
    SelfRef,
    Wild,
    /// Capability takes no scope (e.g. `ui.notify`). [`Scope::Wild`] is
    /// the only legal value for such verbs.
    None,
}

impl Scope {
    pub fn path<S: Into<String>>(s: S) -> Self {
        Scope::Path(s.into())
    }
    pub fn host<S: Into<String>>(s: S) -> Self {
        Scope::Host(s.into())
    }
    pub fn name<S: Into<String>>(s: S) -> Self {
        Scope::Name(s.into())
    }
    pub fn self_ref<S: Into<String>>(s: S) -> Self {
        Scope::SelfRef(s.into())
    }
    pub fn wild() -> Self {
        Scope::Wild
    }

    pub fn kind(&self) -> ScopeKind {
        match self {
            Scope::Path(_) => ScopeKind::Path,
            Scope::Host(_) => ScopeKind::Host,
            Scope::Name(_) => ScopeKind::Name,
            Scope::SelfRef(_) => ScopeKind::SelfRef,
            Scope::Wild => ScopeKind::Wild,
        }
    }

    /// Does this scope (held by the session) cover the requested scope?
    pub fn covers(&self, requested: &Scope) -> bool {
        if matches!(self, Scope::Wild) {
            return true;
        }
        match (self, requested) {
            (Scope::Path(g), Scope::Path(r)) => path_match(g, r),
            (Scope::Host(g), Scope::Host(r)) => host_match(g, r),
            (Scope::Name(g), Scope::Name(r)) => glob_match(g, r),
            (Scope::SelfRef(g), Scope::SelfRef(r)) => g == r,
            _ => false,
        }
    }

    /// True if this scope represents "no restriction at all" — used by
    /// the UI to print a prominent warning.
    pub fn is_wildcard(&self) -> bool {
        match self {
            Scope::Wild => true,
            Scope::Path(s) => s == "**" || s == "/**" || s == "/",
            Scope::Host(s) => s == "*" || s == "**",
            Scope::Name(s) => s == "*" || s == "**",
            Scope::SelfRef(_) => false,
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Path(s) => write!(f, "path:{s}"),
            Scope::Host(s) => write!(f, "host:{s}"),
            Scope::Name(s) => write!(f, "name:{s}"),
            Scope::SelfRef(s) => write!(f, "self:{s}"),
            Scope::Wild => f.write_str("*"),
        }
    }
}

// ---------------------------------------------------------------------------
// Path matching
// ---------------------------------------------------------------------------

/// Expand `~` and `$VAR` references in a path. `~` expands to the
/// current `$HOME`; `$NAME` expands to the env var `NAME` (empty if
/// unset, matching shell behaviour).
fn expand_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let bytes = p.as_bytes();
    let mut i = 0;
    if p.starts_with("~/") || p == "~" {
        if let Ok(home) = std::env::var("HOME") {
            out.push_str(&home);
            i = 1;
        }
    }
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c == '$' && i + 1 < bytes.len() {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_')
            {
                j += 1;
            }
            if j > i + 1 {
                let name = &p[i + 1..j];
                if let Ok(val) = std::env::var(name) {
                    out.push_str(&val);
                } // empty if unset, matching `set -u` off
                i = j;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Normalize a path: resolve `.` and `..`, collapse duplicate `/`,
/// drop trailing slashes (except root). Pure string operation — does
/// not touch the filesystem.
fn normalize_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for component in p.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        "/".into()
    } else {
        let mut s = String::with_capacity(p.len());
        for part in &parts {
            s.push('/');
            s.push_str(part);
        }
        s
    }
}

/// Does the glob `pat` match the literal path `target`?
///
/// `*` matches one path segment; `**` matches any number of segments
/// (including zero). All other characters match literally.
fn path_match(pat: &str, target: &str) -> bool {
    let pat = normalize_path(&expand_path(pat));
    let target = normalize_path(&expand_path(target));

    // Fast path: identical strings.
    if pat == target {
        return true;
    }

    let pat_segs: Vec<&str> = pat.split('/').filter(|s| !s.is_empty()).collect();
    let tgt_segs: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    glob_segs(&pat_segs, &tgt_segs)
}

/// Segment-wise glob matcher with `*` (one segment) and `**` (zero+).
fn glob_segs(pat: &[&str], tgt: &[&str]) -> bool {
    match (pat.first(), tgt.first()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some(&"**"), _) => {
            // ** matches zero or more segments — try every split.
            let rest = &pat[1..];
            if glob_segs(rest, tgt) {
                return true;
            }
            for i in 0..tgt.len() {
                if glob_segs(rest, &tgt[i + 1..]) {
                    return true;
                }
            }
            false
        }
        (Some(_), None) => false,
        (Some(p), Some(t)) => {
            if segment_match(p, t) {
                glob_segs(&pat[1..], &tgt[1..])
            } else {
                false
            }
        }
    }
}

/// Match a single segment. `*` inside a segment matches any run of
/// non-`/` chars; otherwise literal compare.
fn segment_match(pat: &str, tgt: &str) -> bool {
    if pat == "*" {
        return true;
    }
    if !pat.contains('*') {
        return pat == tgt;
    }
    // General case: split on '*' and ensure parts appear in order.
    let parts: Vec<&str> = pat.split('*').collect();
    let mut cursor = 0;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 {
            if !tgt[cursor..].starts_with(part) {
                return false;
            }
            cursor += part.len();
        } else if i + 1 == parts.len() {
            if !tgt[cursor..].ends_with(part) {
                return false;
            }
        } else if let Some(found) = tgt[cursor..].find(part) {
            cursor += found + part.len();
        } else {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Host matching
// ---------------------------------------------------------------------------

fn split_host_port(s: &str) -> (&str, Option<&str>) {
    if let Some(idx) = s.rfind(':') {
        // Avoid splitting IPv6 (which has multiple colons); only treat as
        // host:port if the part after the colon is all digits.
        let (h, p) = (&s[..idx], &s[idx + 1..]);
        if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) {
            return (h, Some(p));
        }
    }
    (s, None)
}

fn host_match(pat: &str, target: &str) -> bool {
    let (ph, pp) = split_host_port(pat);
    let (th, tp) = split_host_port(target);

    // Port: if granted has none, any target port matches; else must equal.
    if let Some(pp) = pp {
        if tp != Some(pp) {
            return false;
        }
    }

    // Host: reverse-label glob.
    let pl: Vec<&str> = ph.split('.').collect();
    let tl: Vec<&str> = th.split('.').collect();
    if pl.iter().any(|s| *s == "**") {
        // `**` somewhere in pattern — defer to generic glob.
        return glob_segs(&pl, &tl);
    }
    if pl.len() != tl.len() {
        return false;
    }
    pl.iter()
        .zip(tl.iter())
        .all(|(p, t)| segment_match(p, t))
}

// ---------------------------------------------------------------------------
// Name matching (generic `*` / `**` glob)
// ---------------------------------------------------------------------------

fn glob_match(pat: &str, target: &str) -> bool {
    if pat == target {
        return true;
    }
    let pat_segs: Vec<&str> = pat.split('/').collect();
    let tgt_segs: Vec<&str> = target.split('/').collect();
    glob_segs(&pat_segs, &tgt_segs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wild_covers_anything() {
        assert!(Scope::Wild.covers(&Scope::path("/etc/passwd")));
        assert!(Scope::Wild.covers(&Scope::host("evil.com")));
        assert!(Scope::Wild.covers(&Scope::Wild));
    }

    #[test]
    fn different_kinds_never_cover() {
        let p = Scope::path("/etc/**");
        let h = Scope::host("*.github.com");
        assert!(!p.covers(&h));
        assert!(!h.covers(&p));
    }

    #[test]
    fn path_prefix_basic() {
        let granted = Scope::path("/home/jay/docs/**");
        assert!(granted.covers(&Scope::path("/home/jay/docs/a.txt")));
        assert!(granted.covers(&Scope::path("/home/jay/docs/deep/sub/a.txt")));
        assert!(!granted.covers(&Scope::path("/home/jay/other/a.txt")));
    }

    #[test]
    fn path_single_star_is_one_segment() {
        let granted = Scope::path("/home/*/docs");
        assert!(granted.covers(&Scope::path("/home/jay/docs")));
        assert!(granted.covers(&Scope::path("/home/alice/docs")));
        assert!(!granted.covers(&Scope::path("/home/jay/sub/docs")));
    }

    #[test]
    fn path_dotdot_cannot_escape() {
        let granted = Scope::path("/home/jay/docs/**");
        // ../ resolves to /home/jay/secret which is outside the scope.
        assert!(!granted.covers(&Scope::path("/home/jay/docs/../secret/x")));
    }

    #[test]
    fn host_label_wildcard() {
        let granted = Scope::host("*.github.com");
        assert!(granted.covers(&Scope::host("api.github.com")));
        assert!(!granted.covers(&Scope::host("github.com"))); // * needs a label
        assert!(!granted.covers(&Scope::host("api.gitlab.com")));
    }

    #[test]
    fn host_port_match() {
        let granted = Scope::host("*.github.com:443");
        assert!(granted.covers(&Scope::host("api.github.com:443")));
        assert!(!granted.covers(&Scope::host("api.github.com:80")));
        // Granted without port covers any port.
        let any_port = Scope::host("*.github.com");
        assert!(any_port.covers(&Scope::host("api.github.com:443")));
        assert!(any_port.covers(&Scope::host("api.github.com:80")));
    }

    #[test]
    fn name_glob() {
        let granted = Scope::name("openai/*");
        assert!(granted.covers(&Scope::name("openai/api-key")));
        assert!(!granted.covers(&Scope::name("anthropic/api-key")));

        let deep = Scope::name("billing/**");
        assert!(deep.covers(&Scope::name("billing/2026/q1/invoices")));
    }

    #[test]
    fn self_ref_is_literal() {
        let granted = Scope::self_ref("self.children.*");
        // Literal equality — no glob magic on SelfRef.
        assert!(granted.covers(&Scope::self_ref("self.children.*")));
        assert!(!granted.covers(&Scope::self_ref("self.children.123")));
    }

    #[test]
    fn is_wildcard_flags_dangerous_scopes() {
        assert!(Scope::Wild.is_wildcard());
        assert!(Scope::path("/").is_wildcard());
        assert!(Scope::path("**").is_wildcard());
        assert!(Scope::host("*").is_wildcard());
        assert!(!Scope::path("~/Documents/**").is_wildcard());
        assert!(!Scope::host("*.github.com").is_wildcard());
    }

    #[test]
    fn serde_round_trip_each_kind() {
        for s in [
            Scope::path("/a/**"),
            Scope::host("*.example.com:443"),
            Scope::name("ns/*"),
            Scope::self_ref("self"),
            Scope::Wild,
        ] {
            let j = serde_json::to_string(&s).unwrap();
            let back: Scope = serde_json::from_str(&j).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn env_var_expansion() {
        std::env::set_var("COS_TEST_PFX", "/var/tmp/cos-test");
        let granted = Scope::path("$COS_TEST_PFX/**");
        assert!(granted.covers(&Scope::path("/var/tmp/cos-test/file.txt")));
        std::env::remove_var("COS_TEST_PFX");
    }
}
