use std::net::{IpAddr, ToSocketAddrs};
use std::process::Command;

use url::Url;

use crate::client::ObscuraNetError;

pub(crate) fn authorize_http_url(
    url: &Url,
) -> Result<Vec<std::net::SocketAddr>, ObscuraNetError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ObscuraNetError::Network(format!(
            "forbidden URL scheme '{}'",
            url.scheme()
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ObscuraNetError::Network(
            "URL userinfo is not allowed".to_string(),
        ));
    }
    let host = normalized_host(url)?;
    if matches!(host.to_ascii_lowercase().as_str(), "localhost" | "ip6-localhost") {
        return Err(ObscuraNetError::Network(format!(
            "access to {host} is not allowed"
        )));
    }
    let port = url.port_or_known_default().unwrap_or(443);
    if std::env::var_os("COS_SESSION").is_some() {
        authorize_kernel_scope(url)?;
    }
    let addresses: Vec<std::net::SocketAddr> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|error| ObscuraNetError::Network(format!("DNS lookup failed: {error}")))?
        .collect();
    for address in &addresses {
        reject_private_ip(address.ip(), &host)?;
    }
    if addresses.is_empty() {
        return Err(ObscuraNetError::Network(format!(
            "DNS lookup returned no addresses for {host}"
        )));
    }

    Ok(addresses)
}

fn authorize_kernel_scope(url: &Url) -> Result<(), ObscuraNetError> {
    let scope = effective_host_scope(url)?;
    let output = Command::new(cos_binary())
        .args(["__policy", "check", "net.dial", "--host", &scope])
        .output()
        .map_err(|error| {
            ObscuraNetError::Network(format!("net.dial authorization failed: {error}"))
        })?;
    if !output.status.success() {
        return Err(ObscuraNetError::Blocked(scope));
    }

    let value: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        ObscuraNetError::Network(format!("invalid net.dial authorization response: {error}"))
    })?;
    if value.get("decision").and_then(serde_json::Value::as_str) != Some("allow") {
        return Err(ObscuraNetError::Blocked(scope));
    }
    Ok(())
}

pub fn effective_host_scope(url: &Url) -> Result<String, ObscuraNetError> {
    let host = normalized_host(url)?;
    let port = url.port_or_known_default().ok_or_else(|| {
        ObscuraNetError::Network(format!("URL scheme '{}' has no known port", url.scheme()))
    })?;
    if host.contains(':') {
        Ok(format!("[{host}]:{port}"))
    } else {
        Ok(format!("{host}:{port}"))
    }
}

fn normalized_host(url: &Url) -> Result<String, ObscuraNetError> {
    match url.host() {
        Some(url::Host::Domain(host)) => Ok(host.to_string()),
        Some(url::Host::Ipv4(ip)) => Ok(ip.to_string()),
        Some(url::Host::Ipv6(ip)) => Ok(ip.to_string()),
        None => Err(ObscuraNetError::Network("URL has no host".to_string())),
    }
}

fn cos_binary() -> std::path::PathBuf {
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            let sibling = parent.join("cos");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    "/usr/local/bin/cos".into()
}

fn reject_private_ip(ip: IpAddr, host: &str) -> Result<(), ObscuraNetError> {
    let blocked = match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_unspecified()
                || ip.is_documentation()
                || (octets[0] == 100 && (octets[1] & 0b1100_0000) == 64)
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 192 && octets[1] == 88 && octets[2] == 99)
                || (octets[0] == 198 && (octets[1] & 0b1111_1110) == 18)
        }

        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_multicast()
                || ip.is_unspecified()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| {
                        reject_private_ip(IpAddr::V4(mapped), host).is_err()
                    })
        }
    };
    if blocked {
        return Err(ObscuraNetError::Blocked(format!("{host} -> {ip}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/url_policy.rs"
    ));
}
