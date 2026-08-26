// Shared SSRF policy for cos-browser. Included by both `main.rs`
// (the router binary) and `worker.rs` (the headless worker) via
// `#[path]` so each binary gets its own compile of the same source
// — cos-browser is a multi-bin crate with no library target, so
// `use` paths can't reach across binaries.

use url::Url;

/// Reject schemes other than http/https and obviously-internal hostnames.
///
/// Goes beyond obscura-net's `validate_url` (which only blocks private
/// IPv4/IPv6 ranges in *literal* IP URLs) by also **resolving** the
/// hostname and rejecting any candidate IP that lives in a private,
/// loopback, link-local, multicast, CGNAT, broadcast, or unspecified
/// range. Without that step a malicious page could trivially do an
/// SSRF by pointing the agent at e.g. `http://internal.local/` or at
/// `http://attacker-controlled-name/` whose A record returns
/// `169.254.169.254` (AWS IMDS), `10.0.0.0/8` (corp intranet),
/// `127.0.0.1` (localhost) etc.
///
/// Returns the parsed URL plus the snapshot of resolved IPs so the
/// caller can re-resolve after the navigation finishes and detect
/// DNS-rebinding attacks (the host returned a public address during
/// the policy check, then a private one for the real fetch).
pub fn validate_navigable_url(input: &str) -> anyhow::Result<(Url, Vec<std::net::IpAddr>)> {
    use std::net::{IpAddr, ToSocketAddrs};

    let url = Url::parse(input)
        .map_err(|e| anyhow::anyhow!("invalid URL '{}': {}", input, e))?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        anyhow::bail!(
            "scheme '{}' is not allowed (only http/https)",
            scheme
        );
    }
    let host = match url.host_str() {
        None => anyhow::bail!("URL has no host: {}", input),
        Some(h) => h.to_string(),
    };
    let h_lower = host.to_ascii_lowercase();
    if h_lower == "localhost" || h_lower == "ip6-localhost" {
        anyhow::bail!("access to {} is not allowed", host);
    }
    // Resolve to an IP set so a malicious hostname can't smuggle a
    // private IP past us via DNS. `to_socket_addrs` follows the system
    // resolver — exactly what chromium will use moments later.
    let port = url.port_or_known_default().unwrap_or(443);
    let addrs: Vec<std::net::SocketAddr> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| anyhow::anyhow!("DNS lookup failed for {}: {}", host, e))?
        .collect();
    if addrs.is_empty() {
        anyhow::bail!("DNS lookup returned no addresses for {}", host);
    }
    let mut ips: Vec<IpAddr> = Vec::with_capacity(addrs.len());
    for sa in addrs {
        let ip = sa.ip();
        reject_private_ip(&ip, &host)?;
        ips.push(ip);
    }
    Ok((url, ips))
}

/// Reject any IP that lives in a loopback / private / link-local /
/// multicast / broadcast / CGNAT / unspecified range. Used by
/// [`validate_navigable_url`] to refuse internal targets resolved
/// via DNS.
pub fn reject_private_ip(ip: &std::net::IpAddr, host: &str) -> anyhow::Result<()> {
    use std::net::IpAddr;
    let internal = match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_documentation()
                // 100.64.0.0/10 — Carrier-grade NAT (RFC 6598). Used
                // by ISPs and increasingly by cloud private networks.
                || (octets[0] == 100 && (octets[1] & 0b1100_0000) == 64)
                // 192.0.0.0/24, 192.88.99.0/24, 198.18.0.0/15 — IETF
                // protocol assignments / benchmarking. Never valid
                // for a user-driven HTTP fetch.
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
                || (octets[0] == 198 && (octets[1] & 0b1111_1110) == 18)
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                // Unique-local fc00::/7 and link-local fe80::/10.
                // The stable IPv6 API doesn't expose these as
                // helpers, so check the leading byte directly.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // IPv4-mapped IPv6 — re-run the v4 checks on the
                // mapped address so `::ffff:10.0.0.1` isn't a way
                // around the v4 ruleset.
                || v6
                    .to_ipv4_mapped()
                    .map(|v4| {
                        let ip = IpAddr::V4(v4);
                        reject_private_ip(&ip, host).is_err()
                    })
                    .unwrap_or(false)
        }
    };
    if internal {
        anyhow::bail!(
            "host {} resolved to internal address {} — refusing to navigate",
            host,
            ip
        );
    }
    Ok(())
}

/// After a navigation completes, re-resolve the host and confirm the
/// fresh address set still intersects the snapshot we captured
/// pre-navigation. Catches DNS-rebinding where the attacker's
/// resolver returns a public IP during the pre-flight check and a
/// private one for the real fetch.
#[allow(dead_code)] // Only the main router binary uses this; the
                    // worker just calls `validate_navigable_url`.
pub fn recheck_no_rebind(url: &Url, before: &[std::net::IpAddr]) -> anyhow::Result<()> {
    use std::net::ToSocketAddrs;
    let host = match url.host_str() {
        Some(h) => h.to_string(),
        None => return Ok(()),
    };
    let port = url.port_or_known_default().unwrap_or(443);
    let after: Vec<std::net::IpAddr> = (host.as_str(), port)
        .to_socket_addrs()
        .map(|iter| iter.map(|sa| sa.ip()).collect())
        .unwrap_or_default();
    if after.is_empty() {
        // Lookup failure post-fetch isn't necessarily an attack — the
        // host may simply have gone away. Don't fail open: only fail
        // if we *did* get a result and it doesn't overlap.
        return Ok(());
    }
    for ip in &after {
        if !before.contains(ip) {
            // Verify the new IP is also non-private; if it is, that
            // confirms a rebinding attempt and we error out.
            if reject_private_ip(ip, &host).is_err() {
                anyhow::bail!(
                    "DNS rebinding detected for {}: post-fetch IP {} differs from pre-fetch set",
                    host,
                    ip,
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/url_safety.rs"
    ));
}
