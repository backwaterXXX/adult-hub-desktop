use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceUrl {
    pub normalized: String,
    pub origin: String,
    pub uses_cleartext: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceUrlValidation {
    Valid(ServiceUrl),
    Invalid(String),
}

pub struct ServiceUrlPolicy;

impl ServiceUrlPolicy {
    pub fn inspect(raw_url: &str, allow_query: bool) -> ServiceUrlValidation {
        let trimmed = raw_url.trim();
        if trimmed.is_empty() {
            return ServiceUrlValidation::Invalid("Server URL cannot be empty".into());
        }

        let candidate_url = if !trimmed.contains("://") {
            let host_candidate = if trimmed.starts_with('[') {
                trimmed
                    .split(']')
                    .next()
                    .map(|s| &s[1..])
                    .unwrap_or(trimmed)
            } else {
                trimmed.split(['/', ':']).next().unwrap_or(trimmed)
            };
            let scheme = if Self::is_local_network_host(host_candidate) {
                "http"
            } else {
                "https"
            };
            format!("{scheme}://{trimmed}")
        } else {
            trimmed.to_string()
        };

        let parsed = match Url::parse(&candidate_url) {
            Ok(url) => url,
            Err(_) => return ServiceUrlValidation::Invalid("Enter a valid server URL".into()),
        };

        let scheme = parsed.scheme().to_lowercase();
        if scheme != "http" && scheme != "https" {
            return ServiceUrlValidation::Invalid("Use a complete HTTP or HTTPS URL".into());
        }

        let host_str = match parsed.host_str() {
            Some(h) if !h.trim().is_empty() => h.to_lowercase(),
            _ => return ServiceUrlValidation::Invalid("Use a complete HTTP or HTTPS URL".into()),
        };

        if !parsed.username().is_empty() || parsed.password().is_some() {
            return ServiceUrlValidation::Invalid(
                "Server URLs cannot include credentials, a fragment, or a query".into(),
            );
        }

        if parsed.fragment().is_some() || (!allow_query && parsed.query().is_some()) {
            return ServiceUrlValidation::Invalid(
                "Server URLs cannot include credentials, a fragment, or a query".into(),
            );
        }

        if let Some(port) = parsed.port() {
            if port == 0 {
                return ServiceUrlValidation::Invalid("Enter a valid server port".into());
            }
        }

        let uses_cleartext = scheme == "http";
        if uses_cleartext && !Self::is_local_network_host(&host_str) {
            return ServiceUrlValidation::Invalid(
                "Unencrypted HTTP is allowed only for a private LAN address, a .local host, or localhost. Use HTTPS for public servers.".into(),
            );
        }

        let host_clean = host_str.trim_start_matches('[').trim_end_matches(']');
        let effective_port = match parsed.port() {
            Some(p) => p,
            None => {
                if uses_cleartext {
                    80
                } else {
                    443
                }
            }
        };

        let host_formatted = if host_clean.contains(':') {
            format!("[{host_clean}]")
        } else {
            host_clean.to_string()
        };

        let normalized_port = match parsed.port() {
            Some(p) => format!(":{p}"),
            None => String::new(),
        };

        let raw_path = parsed.path().trim_end_matches('/');
        let normalized_path = if raw_path.is_empty() { "" } else { raw_path };
        let normalized_query = if allow_query {
            parsed.query().map(|q| format!("?{q}")).unwrap_or_default()
        } else {
            String::new()
        };

        let normalized = format!(
            "{scheme}://{host_formatted}{normalized_port}{normalized_path}{normalized_query}"
        );
        let origin = format!("{scheme}://{host_formatted}:{effective_port}");

        ServiceUrlValidation::Valid(ServiceUrl {
            normalized,
            origin,
            uses_cleartext,
        })
    }

    pub fn is_local_network_host(host: &str) -> bool {
        let clean = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_lowercase();
        if clean == "localhost" || clean.ends_with(".localhost") || clean.ends_with(".local") {
            return true;
        }

        if let Ok(ip) = clean.parse::<IpAddr>() {
            return match ip {
                IpAddr::V4(ipv4) => Self::is_local_ipv4(ipv4),
                IpAddr::V6(ipv6) => Self::is_local_ipv6(ipv6),
            };
        }

        false
    }

    fn is_local_ipv4(ip: Ipv4Addr) -> bool {
        let octets = ip.octets();
        let (a, b) = (octets[0], octets[1]);
        a == 10
            || a == 127
            || (a == 172 && (16..=31).contains(&b))
            || (a == 192 && b == 168)
            || (a == 169 && b == 254)
            || (a == 100 && (64..=127).contains(&b))
    }

    fn is_local_ipv6(ip: Ipv6Addr) -> bool {
        ip.is_loopback()
            || ((ip.segments()[0] & 0xfe00) == 0xfc00)
            || ((ip.segments()[0] & 0xffc0) == 0xfe80)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_localhost_and_local_network() {
        match ServiceUrlPolicy::inspect("http://localhost:8096", false) {
            ServiceUrlValidation::Valid(url) => {
                assert_eq!(url.normalized, "http://localhost:8096");
                assert_eq!(url.origin, "http://localhost:8096");
                assert!(url.uses_cleartext);
            }
            ServiceUrlValidation::Invalid(reason) => panic!("Expected valid: {reason}"),
        }

        match ServiceUrlPolicy::inspect("http://192.168.1.100:8096/", false) {
            ServiceUrlValidation::Valid(url) => {
                assert_eq!(url.normalized, "http://192.168.1.100:8096");
                assert_eq!(url.origin, "http://192.168.1.100:8096");
            }
            ServiceUrlValidation::Invalid(reason) => panic!("Expected valid: {reason}"),
        }

        match ServiceUrlPolicy::inspect("http://server.local:8096", false) {
            ServiceUrlValidation::Valid(url) => {
                assert_eq!(url.normalized, "http://server.local:8096");
            }
            ServiceUrlValidation::Invalid(reason) => panic!("Expected valid: {reason}"),
        }
    }

    #[test]
    fn rejects_public_http() {
        match ServiceUrlPolicy::inspect("http://example.com:8096", false) {
            ServiceUrlValidation::Invalid(reason) => {
                assert!(reason.contains("Unencrypted HTTP is allowed only"));
            }
            ServiceUrlValidation::Valid(_) => panic!("Expected invalid for public HTTP"),
        }
    }

    #[test]
    fn allows_public_https() {
        match ServiceUrlPolicy::inspect("https://jellyfin.example.com", false) {
            ServiceUrlValidation::Valid(url) => {
                assert_eq!(url.normalized, "https://jellyfin.example.com");
                assert_eq!(url.origin, "https://jellyfin.example.com:443");
                assert!(!url.uses_cleartext);
            }
            ServiceUrlValidation::Invalid(reason) => panic!("Expected valid: {reason}"),
        }
    }

    #[test]
    fn rejects_query_when_disallowed() {
        match ServiceUrlPolicy::inspect("https://example.com?token=123", false) {
            ServiceUrlValidation::Invalid(reason) => {
                assert!(reason.contains("fragment, or a query"));
            }
            ServiceUrlValidation::Valid(_) => panic!("Expected invalid query"),
        }
    }

    #[test]
    fn auto_prefixes_scheme_for_bare_hostnames_and_ips() {
        // Public domain defaults to https://
        match ServiceUrlPolicy::inspect("adult.ferges.eu", false) {
            ServiceUrlValidation::Valid(url) => {
                assert_eq!(url.normalized, "https://adult.ferges.eu");
                assert_eq!(url.origin, "https://adult.ferges.eu:443");
                assert!(!url.uses_cleartext);
            }
            ServiceUrlValidation::Invalid(err) => panic!("Expected valid: {err}"),
        }

        // Local IP defaults to http://
        match ServiceUrlPolicy::inspect("192.168.1.111:8096", false) {
            ServiceUrlValidation::Valid(url) => {
                assert_eq!(url.normalized, "http://192.168.1.111:8096");
                assert_eq!(url.origin, "http://192.168.1.111:8096");
                assert!(url.uses_cleartext);
            }
            ServiceUrlValidation::Invalid(err) => panic!("Expected valid: {err}"),
        }

        // Localhost defaults to http://
        match ServiceUrlPolicy::inspect("localhost:8096", false) {
            ServiceUrlValidation::Valid(url) => {
                assert_eq!(url.normalized, "http://localhost:8096");
                assert!(url.uses_cleartext);
            }
            ServiceUrlValidation::Invalid(err) => panic!("Expected valid: {err}"),
        }
    }
}
