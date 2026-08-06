use std::net::IpAddr;
use std::sync::RwLock;
use url::Url;

pub struct CookieJar {
    cookies: RwLock<Vec<CookieEntry>>,
}

#[derive(Debug, Clone)]
struct CookieEntry {
    name: String,
    value: String,
    path: String,
    domain: String,
    host_only: bool,
    secure: bool,
    http_only: bool,
    expires: Option<u64>,
}

impl CookieJar {
    pub fn new() -> Self {
        CookieJar {
            cookies: RwLock::new(Vec::new()),
        }
    }

    pub fn set_cookie(&self, set_cookie_str: &str, url: &Url) {
        if let Some(entry) = parse_cookie(set_cookie_str, url, true) {
            self.store_cookie(entry);
        }
    }

    pub fn get_cookie_header(&self, url: &Url) -> String {
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let is_secure = url.scheme() == "https";
        let cookies = self.cookies.read().unwrap();
        let mut matching: Vec<&CookieEntry> = Vec::new();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        for entry in cookies.iter() {
            if !cookie_domain_matches(host, entry) {
                continue;
            }
            if let Some(exp) = entry.expires {
                if exp < now {
                    continue;
                }
            }
            if entry.secure && !is_secure {
                continue;
            }
            if !path_matches(path, &entry.path) {
                continue;
            }
            matching.push(entry);
        }

        matching.sort_by_key(|entry| std::cmp::Reverse(entry.path.len()));
        matching
            .into_iter()
            .map(|entry| format!("{}={}", entry.name, entry.value))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn get_all_cookies(&self) -> Vec<CookieInfo> {
        let cookies = self.cookies.read().unwrap();
        let mut result = Vec::new();
        for entry in cookies.iter() {
            result.push(CookieInfo {
                name: entry.name.clone(),
                value: entry.value.clone(),
                domain: if entry.host_only {
                    entry.domain.clone()
                } else {
                    format!(".{}", entry.domain)
                },
                path: entry.path.clone(),
                secure: entry.secure,
                http_only: entry.http_only,
            });
        }
        result
    }

    pub fn set_cookies_from_cdp(&self, cookies: Vec<CookieInfo>) {
        for cookie in cookies {
            let host_only = !cookie.domain.starts_with('.');
            let Some(domain) = canonical_domain(&cookie.domain) else {
                continue;
            };
            if !host_only && is_public_suffix(&domain) {
                continue;
            }
            let entry = CookieEntry {
                name: cookie.name.clone(),
                value: cookie.value,
                path: normalize_cookie_path(&cookie.path),
                domain,
                host_only,
                secure: cookie.secure,
                http_only: cookie.http_only,
                expires: None,
            };
            self.store_cookie(entry);
        }
    }

    pub fn get_js_visible_cookies(&self, url: &Url) -> String {
        let host = url.host_str().unwrap_or("");
        let path = url.path();
        let is_secure = url.scheme() == "https";
        let cookies = self.cookies.read().unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut matching: Vec<&CookieEntry> = Vec::new();

        for entry in cookies.iter() {
            if !cookie_domain_matches(host, entry) {
                continue;
            }
            if entry.http_only {
                continue;
            }
            if let Some(exp) = entry.expires {
                if exp < now {
                    continue;
                }
            }
            if entry.secure && !is_secure {
                continue;
            }
            if !path_matches(path, &entry.path) {
                continue;
            }
            matching.push(entry);
        }

        matching.sort_by_key(|entry| std::cmp::Reverse(entry.path.len()));
        matching
            .into_iter()
            .map(|entry| format!("{}={}", entry.name, entry.value))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn set_cookie_from_js(&self, cookie_str: &str, url: &Url) {
        if let Some(entry) = parse_cookie(cookie_str, url, false) {
            self.store_cookie(entry);
        }
    }

    pub fn delete_cookie(&self, name: &str, domain: &str) {
        let mut cookies = self.cookies.write().unwrap();
        if domain.is_empty() {
            cookies.retain(|entry| entry.name != name);
        } else if let Some(domain) = canonical_domain(domain) {
            cookies.retain(|entry| {
                !(entry.name == name && entry.domain.eq_ignore_ascii_case(&domain))
            });
        }
    }

    fn store_cookie(&self, entry: CookieEntry) {
        let mut cookies = self.cookies.write().unwrap();
        cookies.retain(|existing| {
            !(existing.name == entry.name
                && existing.domain == entry.domain
                && existing.path == entry.path
                && existing.host_only == entry.host_only)
        });

        let now = unix_time_secs();
        if entry.expires.is_some_and(|expires| expires <= now) {
            return;
        }
        cookies.push(entry);
    }

    pub fn clear(&self) {
        self.cookies.write().unwrap().clear();
    }
}

fn parse_cookie(cookie_str: &str, url: &Url, allow_http_only: bool) -> Option<CookieEntry> {
    let mut parts = cookie_str.split(';');
    let name_value = parts.next()?.trim();
    let (name, value) = name_value.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    let request_host = canonical_domain(url.host_str()?)?;
    let mut domain_attr: Option<String> = None;
    let mut path = default_cookie_path(url);
    let mut secure = false;
    let mut http_only = false;
    let mut expires: Option<u64> = None;
    let mut max_age: Option<i64> = None;

    for attr in parts {
        let attr = attr.trim();
        if let Some((key, val)) = attr.split_once('=') {
            match key.trim().to_ascii_lowercase().as_str() {
                "domain" => domain_attr = Some(val.trim().to_string()),
                "path" => {
                    if val.trim().starts_with('/') {
                        path = normalize_cookie_path(val.trim());
                    }
                }
                "expires" => expires = parse_http_date(val.trim()).ok(),
                "max-age" => max_age = val.trim().parse::<i64>().ok(),
                _ => {}
            }
        } else {
            match attr.to_ascii_lowercase().as_str() {
                "secure" => secure = true,
                "httponly" if allow_http_only => http_only = true,
                _ => {}
            }
        }
    }

    if secure && url.scheme() != "https" {
        return None;
    }

    let (domain, host_only) = match domain_attr {
        Some(raw) => {
            let domain = canonical_domain(&raw)?;
            if request_host.parse::<IpAddr>().is_ok()
                || !domain_matches(&request_host, &domain)
                || is_public_suffix(&domain)
            {
                return None;
            }
            (domain, false)
        }
        None => (request_host, true),
    };

    if let Some(seconds) = max_age {
        expires = if seconds <= 0 {
            Some(0)
        } else {
            Some(unix_time_secs().saturating_add(seconds as u64))
        };
    }

    Some(CookieEntry {
        name: name.to_string(),
        value: value.trim().to_string(),
        path,
        domain,
        host_only,
        secure,
        http_only,
        expires,
    })
}

fn canonical_domain(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_start_matches('.').trim_end_matches('.');
    if raw.is_empty() {
        return None;
    }
    match url::Host::parse(raw).ok()? {
        url::Host::Domain(domain) => Some(domain.to_ascii_lowercase()),
        url::Host::Ipv4(address) => Some(address.to_string()),
        url::Host::Ipv6(address) => Some(address.to_string()),
    }
}

fn is_public_suffix(domain: &str) -> bool {
    psl::suffix_str(domain).is_some_and(|suffix| suffix.eq_ignore_ascii_case(domain))
}

fn default_cookie_path(url: &Url) -> String {
    let request_path = url.path();
    if !request_path.starts_with('/') || request_path == "/" {
        return "/".to_string();
    }
    match request_path.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(index) => request_path[..index].to_string(),
    }
}

fn normalize_cookie_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        "/".to_string()
    }
}

fn cookie_domain_matches(host: &str, entry: &CookieEntry) -> bool {
    if entry.host_only {
        host.eq_ignore_ascii_case(&entry.domain)
    } else {
        domain_matches(host, &entry.domain)
    }
}

fn path_matches(request_path: &str, cookie_path: &str) -> bool {
    if request_path == cookie_path {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    cookie_path.ends_with('/')
        || request_path
            .as_bytes()
            .get(cookie_path.len())
            .is_some_and(|next| *next == b'/')
}

fn unix_time_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Default for CookieJar {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CookieInfo {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    #[serde(rename = "httpOnly")]
    pub http_only: bool,
}

fn parse_http_date(s: &str) -> Result<u64, ()> {
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];

    let s = s.replace('-', " ");
    let parts: Vec<&str> = s.split_whitespace().collect();

    if parts.len() < 5 {
        return Err(());
    }

    let day: u64 = parts[1].parse().map_err(|_| ())?;
    let month = months
        .iter()
        .position(|m| parts[2].to_lowercase().starts_with(m))
        .ok_or(())? as u64
        + 1;
    let year: u64 = parts[3].parse().map_err(|_| ())?;

    let time_parts: Vec<&str> = parts[4].split(':').collect();
    let hour: u64 = time_parts.first().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minute: u64 = time_parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let second: u64 = time_parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    let mut days_total: u64 = 0;
    for y in 1970..year {
        days_total += if y.is_multiple_of(4)
            && (!y.is_multiple_of(100) || y.is_multiple_of(400))
        {
            366
        } else {
            365
        };
    }
    let days_in_month = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let is_leap =
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    for m in 1..month {
        days_total += days_in_month[m as usize] + if m == 2 && is_leap { 1 } else { 0 };
    }
    days_total += day - 1;

    Ok(days_total * 86400 + hour * 3600 + minute * 60 + second)
}

fn domain_matches(host: &str, domain: &str) -> bool {
    let host = host.to_lowercase();
    let domain = domain.trim_start_matches('.').to_lowercase();
    host == domain || host.ends_with(&format!(".{}", domain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/path").unwrap();
        jar.set_cookie("session=abc123; Path=/; Secure; HttpOnly", &url);

        let header = jar.get_cookie_header(&url);
        assert!(header.contains("session=abc123"));
    }

    #[test]
    fn test_cookie_domain_matching() {
        let jar = CookieJar::new();
        let url = Url::parse("https://www.example.com/").unwrap();
        jar.set_cookie("token=xyz; Domain=example.com", &url);

        let header = jar.get_cookie_header(&url);
        assert!(header.contains("token=xyz"));

        let sub_url = Url::parse("https://api.example.com/").unwrap();
        let header2 = jar.get_cookie_header(&sub_url);
        assert!(header2.contains("token=xyz"));

        let other_url = Url::parse("https://other.com/").unwrap();
        let header3 = jar.get_cookie_header(&other_url);
        assert!(header3.is_empty());
    }

    #[test]
    fn rejects_cookie_for_unrelated_domain() {
        let jar = CookieJar::new();
        let attacker = Url::parse("https://attacker.example/").unwrap();
        jar.set_cookie("session=fixed; Domain=victim.example; Path=/", &attacker);

        let victim = Url::parse("https://victim.example/").unwrap();
        assert!(jar.get_cookie_header(&victim).is_empty());
    }

    #[test]
    fn rejects_cookie_for_public_suffix() {
        let jar = CookieJar::new();
        let url = Url::parse("https://shop.example.co.uk/").unwrap();
        jar.set_cookie("session=fixed; Domain=co.uk; Path=/", &url);

        assert!(jar.get_cookie_header(&url).is_empty());
    }

    #[test]
    fn host_only_cookie_is_not_sent_to_subdomains() {
        let jar = CookieJar::new();
        let apex = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("host_only=yes; Path=/", &apex);

        let subdomain = Url::parse("https://api.example.com/").unwrap();
        assert!(jar.get_cookie_header(&subdomain).is_empty());
        assert!(jar.get_cookie_header(&apex).contains("host_only=yes"));
    }

    #[test]
    fn same_name_cookies_on_different_paths_coexist() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/account/login").unwrap();
        jar.set_cookie("session=root; Path=/", &url);
        jar.set_cookie("session=account; Path=/account", &url);

        let header = jar.get_cookie_header(&url);
        assert_eq!(header, "session=account; session=root");
    }

    #[test]
    fn cookie_path_matching_respects_segment_boundaries() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/foo/index").unwrap();
        jar.set_cookie("scoped=yes; Path=/foo", &url);

        let sibling = Url::parse("https://example.com/foobar").unwrap();
        assert!(jar.get_cookie_header(&sibling).is_empty());
    }

    #[test]
    fn default_cookie_path_is_request_directory() {
        let jar = CookieJar::new();
        let source = Url::parse("https://example.com/account/login").unwrap();
        jar.set_cookie("default_path=yes", &source);

        let sibling = Url::parse("https://example.com/account/profile").unwrap();
        let outside = Url::parse("https://example.com/settings").unwrap();
        assert!(jar.get_cookie_header(&sibling).contains("default_path=yes"));
        assert!(jar.get_cookie_header(&outside).is_empty());
    }

    #[test]
    fn test_cdp_cookie_with_leading_dot_domain_matches_requests() {
        let jar = CookieJar::new();
        jar.set_cookies_from_cdp(vec![CookieInfo {
            name: "token".to_string(),
            value: "xyz".to_string(),
            domain: ".example.com".to_string(),
            path: "/".to_string(),
            secure: false,
            http_only: false,
        }]);

        let apex_url = Url::parse("https://example.com/").unwrap();
        let apex_header = jar.get_cookie_header(&apex_url);
        assert!(apex_header.contains("token=xyz"));

        let subdomain_url = Url::parse("https://api.example.com/").unwrap();
        let subdomain_header = jar.get_cookie_header(&subdomain_url);
        assert!(subdomain_header.contains("token=xyz"));

        let other_url = Url::parse("https://other.com/").unwrap();
        let other_header = jar.get_cookie_header(&other_url);
        assert!(other_header.is_empty());
    }

    #[test]
    fn test_secure_cookie_not_sent_over_http() {
        let jar = CookieJar::new();
        let https_url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("secure_token=secret; Secure", &https_url);

        let http_url = Url::parse("http://example.com/").unwrap();
        let header = jar.get_cookie_header(&http_url);
        assert!(header.is_empty());
    }

    #[test]
    fn test_max_age_zero_deletes_cookie() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("session=abc", &url);
        assert!(jar.get_cookie_header(&url).contains("session=abc"));

        jar.set_cookie("session=abc; Max-Age=0", &url);
        assert!(jar.get_cookie_header(&url).is_empty());
    }

    #[test]
    fn test_max_age_sets_expiry() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("token=xyz; Max-Age=3600", &url);
        assert!(jar.get_cookie_header(&url).contains("token=xyz"));
    }

    #[test]
    fn test_expired_cookie_not_sent() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("old=gone; Expires=Thu, 01 Jan 2020 00:00:00 GMT", &url);
        assert!(jar.get_cookie_header(&url).is_empty());
    }

    #[test]
    fn test_samesite_parsed() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("strict_cookie=val; SameSite=Strict", &url);
        assert!(jar.get_cookie_header(&url).contains("strict_cookie=val"));
    }

    #[test]
    fn test_clear_cookies() {
        let jar = CookieJar::new();
        let url = Url::parse("https://example.com/").unwrap();
        jar.set_cookie("a=1", &url);
        assert!(!jar.get_cookie_header(&url).is_empty());

        jar.clear();
        assert!(jar.get_cookie_header(&url).is_empty());
    }
}
