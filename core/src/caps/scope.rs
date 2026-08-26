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

/// Expand `~` in a path. `~` and `~/foo` expand to `$HOME` /
/// `$HOME/foo`. Environment-variable references (`$VAR`) are
/// intentionally **not** expanded — the previous implementation
/// invited a confused-deputy attack where a hostile environment
/// could substitute its own value into a session's path scope at
/// match time and quietly broaden coverage (e.g. `$WORKSPACE/**`
/// becoming `/**` when `WORKSPACE` is unset, or worse, becoming
/// `/etc/**` when `WORKSPACE` is set to `/etc`). Patterns are now
/// matched as literal strings (modulo `~`); set the absolute path
/// you mean.
fn expand_path(p: &str) -> String {
    if p == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
    }
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            let mut out = String::with_capacity(home.len() + 1 + rest.len());
            out.push_str(&home);
            out.push('/');
            out.push_str(rest);
            return out;
        }
    }
    p.to_string()
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
///
/// **Symlink-safe matching.** Pure string normalization (collapse
/// `.`, `..`, duplicate `/`) is *not* enough to keep an attacker
/// from escaping their granted scope — a session that holds
/// `path:/workspace/**` would, without canonicalization, match a
/// request for `/workspace/escape` even if `escape` is a symlink to
/// `/etc/shadow`. We therefore canonicalize the *target* against
/// the live filesystem (resolving symlinks) before comparing, and
/// fall back to the lexically-normalized path if the target does
/// not yet exist (e.g. the caller is about to create a new file).
/// Patterns themselves are normalized lexically only — we wouldn't
/// want a glob like `~/**` to require `$HOME` to exist as a
/// canonicalizable target.
fn path_match(pat: &str, target: &str) -> bool {
    let pat = normalize_path(&expand_path(pat));
    let target_expanded = expand_path(target);
    let target = canonicalize_for_match(&target_expanded);
    // Also canonicalize the *literal* prefix of the pattern (the
    // portion up to the first segment that contains a glob). Without
    // this, platform-level symlinks like `/home` ⇒
    // `/System/Volumes/Data/home` on macOS (or `/tmp` ⇒ `/private/tmp`)
    // would canonicalize the target but not the pattern, leaving an
    // unconditional mismatch.
    let pat = canonicalize_pattern_prefix(&pat);

    // Fast path: identical strings.
    if pat == target {
        return true;
    }

    let pat_segs: Vec<&str> = pat.split('/').filter(|s| !s.is_empty()).collect();
    let tgt_segs: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    glob_segs(&pat_segs, &tgt_segs)
}

/// Canonicalize the literal prefix of a glob pattern (everything up
/// to the first segment containing `*`), then re-append the glob
/// tail unchanged. This is the pattern-side analogue of
/// [`canonicalize_for_match`]: it ensures that a pattern like
/// `path:/home/jay/**` and a target like `/home/jay/docs/x` agree
/// on platform-symlink resolution (e.g. macOS auto-mounts) so the
/// match is decided on real filesystem identity, not on raw prefix
/// strings.
fn canonicalize_pattern_prefix(pat: &str) -> String {
    let segs: Vec<&str> = pat.split('/').collect();
    let split_at = segs
        .iter()
        .position(|s| s.contains('*'))
        .unwrap_or(segs.len());

    let prefix_str: String = if split_at == 0 {
        "/".into()
    } else {
        segs[..split_at].join("/")
    };
    // If the prefix is empty after splitting on the very first slash
    // (e.g. the pattern starts with `*`), there's nothing to
    // canonicalize.
    if prefix_str.is_empty() || prefix_str == "/" {
        return pat.to_string();
    }

    let prefix_canon = canonicalize_for_match(&prefix_str);

    if split_at >= segs.len() {
        return prefix_canon;
    }
    let suffix = segs[split_at..].join("/");
    if prefix_canon.ends_with('/') {
        format!("{prefix_canon}{suffix}")
    } else {
        format!("{prefix_canon}/{suffix}")
    }
}

/// Canonicalize `target` against the live filesystem. Symlinks are
/// resolved. If the leaf does not exist (the caller is about to
/// create it), walk up until we hit an existing ancestor,
/// canonicalize *that*, then rejoin the missing tail. This way a
/// symlink in the existing portion of the path still gets resolved
/// before we make the access decision.
fn canonicalize_for_match(target: &str) -> String {
    let normalized = normalize_path(target);
    let p = std::path::Path::new(&normalized);
    if let Ok(canon) = p.canonicalize() {
        return canon.to_string_lossy().to_string();
    }
    // Path doesn't fully exist yet. Find the deepest existing
    // ancestor, canonicalize it, then re-append the missing tail.
    let mut anc = p;
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    loop {
        if anc.exists() {
            break;
        }
        match anc.file_name() {
            Some(name) => {
                tail.push(name);
                anc = match anc.parent() {
                    Some(parent) => parent,
                    None => return normalized,
                };
            }
            None => return normalized,
        }
    }
    let mut out = match anc.canonicalize() {
        Ok(c) => c,
        Err(_) => return normalized,
    };
    for seg in tail.iter().rev() {
        out.push(seg);
    }
    out.to_string_lossy().to_string()
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
    // DNS labels are case-insensitive (RFC 4343). A scope granted as
    // `host:Example.com` must match a request for `example.com` —
    // refusing it on case grounds is just a bug, not a security win,
    // and would push users to lowercase by hand and forget.
    let pat = pat.to_ascii_lowercase();
    let target = target.to_ascii_lowercase();
    let (ph, pp) = split_host_port(&pat);
    let (th, tp) = split_host_port(&target);

    // Port: if granted has none, any target port matches; else must equal.
    if let Some(pp) = pp {
        if tp != Some(pp) {
            return false;
        }
    }

    // Host: reverse-label glob.
    let pl: Vec<&str> = ph.split('.').collect();
    let tl: Vec<&str> = th.split('.').collect();
    if pl.contains(&"**") {
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/caps/scope.rs"
    ));
}
