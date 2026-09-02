//! Debian version ordering.
//!
//! Freshness decisions are made about Debian package versions, so they
//! have to use Debian's ordering — epochs, `~` sorting *before* the
//! empty string, digit runs compared numerically. Lexicographic string
//! comparison gets `0.2.0+git99` vs `0.2.0+git100` backwards and would
//! silently accept a downgrade, so it is never used here.
//!
//! This is a direct transcription of `dpkg`'s `verrevcmp`/`order`
//! (Debian Policy 5.6.12). `core/tests/security_floor_process.rs`
//! cross-checks it against the real `dpkg --compare-versions` when
//! `dpkg` is installed, so a divergence is a test failure rather than
//! a silent policy hole.

use std::cmp::Ordering;

/// A parsed `[epoch:]upstream[-revision]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    epoch: u64,
    upstream: String,
    revision: String,
}

impl Version {
    /// Parse and validate, refusing anything `dpkg --validate-version`
    /// would refuse.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("version is empty".to_string());
        }
        if raw.trim() != raw {
            return Err("version has surrounding whitespace".to_string());
        }
        let (epoch, rest) = match raw.split_once(':') {
            Some((head, tail)) => {
                if head.is_empty() || !head.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err("version epoch is not a number".to_string());
                }
                let epoch = head
                    .parse::<u64>()
                    .map_err(|_| "version epoch is out of range".to_string())?;
                (epoch, tail)
            }
            None => (0, raw),
        };
        let (upstream, revision) = match rest.rsplit_once('-') {
            Some((head, tail)) => (head, tail),
            None => (rest, ""),
        };
        if upstream.is_empty() {
            return Err("version has no upstream part".to_string());
        }
        if !upstream
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            return Err("upstream version must start with a digit".to_string());
        }
        if !upstream.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-' | b'~' | b':')
        }) {
            return Err("upstream version has an invalid character".to_string());
        }
        if !revision
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'~'))
        {
            return Err("package revision has an invalid character".to_string());
        }
        Ok(Self {
            epoch,
            upstream: upstream.to_string(),
            revision: revision.to_string(),
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.epoch
            .cmp(&other.epoch)
            .then_with(|| verrevcmp(&self.upstream, &other.upstream))
            .then_with(|| verrevcmp(&self.revision, &other.revision))
    }
}

/// `true` when `raw` is a well-formed Debian version.
pub fn is_valid(raw: &str) -> bool {
    Version::parse(raw).is_ok()
}

/// The Debian epoch `raw` declares, or 0 when it declares none.
///
/// Claw OS publishes the release-security epoch *as* the Debian epoch,
/// so that APT's own candidate ordering prefers an emergency release
/// even when its upstream version is lower. An unparseable version
/// yields 0, which is never accepted by the callers that compare it
/// against a non-zero security epoch.
pub fn epoch_of(raw: &str) -> u64 {
    Version::parse(raw)
        .map(|version| version.epoch)
        .unwrap_or(0)
}

/// Compare two Debian versions, refusing to guess about malformed
/// input: an unparseable version is an error, never "equal".
pub fn compare(left: &str, right: &str) -> Result<Ordering, String> {
    Ok(Version::parse(left)?.cmp(&Version::parse(right)?))
}

/// `dpkg`'s single-part comparison.
fn verrevcmp(left: &str, right: &str) -> Ordering {
    let a = left.as_bytes();
    let b = right.as_bytes();
    let (mut i, mut j) = (0usize, 0usize);

    while i < a.len() || j < b.len() {
        let mut first_diff = 0i32;

        while (i < a.len() && !a[i].is_ascii_digit()) || (j < b.len() && !b[j].is_ascii_digit()) {
            let ac = order(a.get(i).copied());
            let bc = order(b.get(j).copied());
            if ac != bc {
                return ac.cmp(&bc);
            }
            i += 1;
            j += 1;
        }
        while i < a.len() && a[i] == b'0' {
            i += 1;
        }
        while j < b.len() && b[j] == b'0' {
            j += 1;
        }
        while i < a.len() && j < b.len() && a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            if first_diff == 0 {
                first_diff = i32::from(a[i]) - i32::from(b[j]);
            }
            i += 1;
            j += 1;
        }
        if i < a.len() && a[i].is_ascii_digit() {
            return Ordering::Greater;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            return Ordering::Less;
        }
        if first_diff != 0 {
            return first_diff.cmp(&0);
        }
    }
    Ordering::Equal
}

/// Weight of one character. `~` sorts before the end of string, plain
/// letters before every other punctuation, digits are handled by the
/// numeric pass.
fn order(byte: Option<u8>) -> i32 {
    match byte {
        None => 0,
        Some(value) if value.is_ascii_digit() => 0,
        Some(value) if value.is_ascii_alphabetic() => i32::from(value),
        Some(b'~') => -1,
        Some(value) => i32::from(value) + 256,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/debver.rs"
    ));
}
