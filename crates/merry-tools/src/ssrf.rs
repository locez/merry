//! Dual-gate SSRF URL and IP validation for outbound web and browser tools.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Returns whether an IP address is loopback, including IPv4-mapped IPv6 forms.
#[must_use]
pub fn is_loopback_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                mapped.is_loopback()
            } else {
                v6.is_loopback()
            }
        }
    }
}

/// Returns whether an IP address belongs to a non-public, reserved, or metadata range.
#[must_use]
pub fn is_non_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_non_public_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_non_public_ipv4(mapped);
            }
            if is_deprecated_ipv4_compatible_v6(v6) {
                return true;
            }
            is_non_public_ipv6(v6)
        }
    }
}

fn is_deprecated_ipv4_compatible_v6(v6: Ipv6Addr) -> bool {
    let segments = v6.segments();
    segments[..6] == [0; 6]
}

fn is_non_public_ipv4(v4: Ipv4Addr) -> bool {
    let octets = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_unspecified()
        || v4.is_multicast()
        // Current network 0.0.0.0/8
        || octets[0] == 0
        // Carrier-grade NAT & cloud metadata (e.g. 100.64.0.0/10, 100.100.100.200)
        || (octets[0] == 100 && (octets[1] & 0xc0) == 64)
        // IETF protocol assignments (192.0.0.0/24)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        // TEST-NET-1 (192.0.2.0/24)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        // Benchmarking (198.18.0.0/15)
        || (octets[0] == 198 && (octets[1] & 0xfe) == 18)
        // TEST-NET-2 (198.51.100.0/24)
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        // TEST-NET-3 (203.0.113.0/24)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        // Reserved / future use (240.0.0.0/4)
        || octets[0] >= 240
}

fn is_non_public_ipv6(v6: Ipv6Addr) -> bool {
    let segments = v6.segments();
    v6.is_loopback()
        || v6.is_unspecified()
        || v6.is_multicast()
        // Unique local addresses (fc00::/7)
        || (segments[0] & 0xfe00) == 0xfc00
        // Link-local addresses (fe80::/10)
        || (segments[0] & 0xffc0) == 0xfe80
        // Documentation range (2001:db8::/32)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
}

/// Returns whether a hostname explicitly names a local loopback target.
///
/// Only `"localhost"` or a loopback IP literal returns `true`. External
/// hostnames never match, preventing DNS-rebinding access to loopback services
/// even when `allow_local` is enabled.
#[must_use]
pub fn is_explicit_local_host(host: &str) -> bool {
    let unbracketed = strip_brackets(host);
    if unbracketed
        .trim_end_matches('.')
        .eq_ignore_ascii_case("localhost")
    {
        return true;
    }
    unbracketed.parse::<IpAddr>().is_ok_and(is_loopback_ip)
}

/// Maximum accepted length of a `browser_tab` `data:` document URL.
pub(crate) const MAX_BROWSER_DATA_URL_BYTES: usize = 512 * 1024;

/// Returns whether `ip` must be blocked when connecting to `host`.
///
/// A public hostname that resolves into the RFC 2544 benchmarking range
/// `198.18.0.0/15` is allowed. Local TUN proxies (Clash, sing-box, Surge)
/// answer DNS with that range and route the later connection by the original
/// hostname. A literal address in the range stays blocked, as does every other
/// reserved address, including DNS that also returns loopback, private, or
/// link-local.
#[must_use]
pub fn is_blocked_for_host(ip: IpAddr, host: &str, allow_local: bool) -> bool {
    if allow_local && is_explicit_local_host(host) && is_loopback_ip(ip) {
        return false;
    }
    if is_benchmarking_fake_ip(ip) && !host_is_ip_literal(host) {
        return false;
    }
    is_non_public_ip(ip)
}

fn is_benchmarking_fake_ip(ip: IpAddr) -> bool {
    let v4 = match ip {
        IpAddr::V4(v4) => v4,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(mapped) => mapped,
            None => return false,
        },
    };
    let octets = v4.octets();
    octets[0] == 198 && (octets[1] & 0xfe) == 18
}

fn host_is_ip_literal(host: &str) -> bool {
    strip_brackets(host).parse::<IpAddr>().is_ok()
}

/// Accepts a self-contained `data:text/html` or `data:text/plain` document.
///
/// `browser_tab` can open this instead of an HTTP server. Other media types,
/// including scripts, stay rejected. The document is still page content: a
/// script inside it is not a Merry instruction.
pub(crate) fn validate_browser_data_url(raw: &str) -> Result<(), String> {
    let raw = raw.trim();
    if raw.len() > MAX_BROWSER_DATA_URL_BYTES {
        return Err(format!(
            "data URL is {} bytes; the limit is {MAX_BROWSER_DATA_URL_BYTES}",
            raw.len()
        ));
    }
    let Some(rest) = raw.strip_prefix("data:") else {
        return Err(
            "only a data:text/html or data:text/plain document can be opened without http(s)"
                .to_owned(),
        );
    };
    let Some((meta, body)) = rest.split_once(',') else {
        return Err("data URL must be data:text/html,... or data:text/plain,...".to_owned());
    };
    if body.is_empty() {
        return Err("data URL document is empty".to_owned());
    }
    let media = meta.split(';').next().unwrap_or("").trim();
    let media = if media.is_empty() {
        "text/plain"
    } else {
        media
    };
    if !media.eq_ignore_ascii_case("text/html") && !media.eq_ignore_ascii_case("text/plain") {
        return Err(format!(
            "unsupported data URL media type {media:?}: only text/html and text/plain are allowed"
        ));
    }
    Ok(())
}

/// Validates a set of resolved socket addresses for `host` against SSRF rules.
pub fn check_ssrf_for_addrs(
    addrs: &[SocketAddr],
    host: &str,
    allow_local: bool,
) -> Result<(), String> {
    if addrs.is_empty() {
        return Err(format!(
            "DNS resolution for host {host:?} returned no addresses"
        ));
    }
    for addr in addrs {
        if is_blocked_for_host(addr.ip(), host, allow_local) {
            return Err(format!(
                "blocked non-public address {} for host {:?}",
                addr.ip(),
                host
            ));
        }
    }
    Ok(())
}

/// Validates URL scheme, hostname, and resolved IP addresses against SSRF policy.
///
/// Returns the validated socket addresses so HTTP callers can pin resolution
/// or verify each redirect hop cleanly.
pub async fn check_ssrf_url(url: &url::Url, allow_local: bool) -> Result<Vec<SocketAddr>, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "unsupported URL scheme {:?}: only http and https are allowed",
            url.scheme()
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| "URL must contain a host".to_owned())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "URL must specify a valid port".to_owned())?;
    let unbracketed = strip_brackets(host);

    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        let addrs = vec![SocketAddr::new(ip, port)];
        check_ssrf_for_addrs(&addrs, host, allow_local)?;
        return Ok(addrs);
    }

    let addrs = tokio::net::lookup_host((unbracketed, port))
        .await
        .map_err(|error| format!("failed to resolve host {host:?}: {error}"))?
        .collect::<Vec<_>>();
    check_ssrf_for_addrs(&addrs, host, allow_local)?;
    Ok(addrs)
}

pub(crate) fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_private_metadata_and_mapped_ipv6_addresses() {
        assert!(!is_non_public_ip("1.1.1.1".parse().expect("ip")));
        assert!(!is_non_public_ip(
            "2606:4700:4700::1111".parse().expect("ip")
        ));

        for blocked in [
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.100.200",
            "100.64.0.1",
            "0.0.0.0",
            "198.18.3.161",
            "240.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:198.18.0.60",
            "fe80::1",
            "fd00::1",
            "2001:db8::1",
        ] {
            let ip: IpAddr = blocked.parse().expect("valid ip");
            assert!(is_non_public_ip(ip), "expected {blocked} to be blocked");
        }
    }

    #[test]
    fn allow_local_permits_explicit_loopback_but_blocks_dns_rebinding_and_metadata() {
        let loopback = SocketAddr::new("127.0.0.1".parse().expect("ip"), 8080);
        let mapped_loopback = SocketAddr::new("::ffff:127.0.0.1".parse().expect("ip"), 8080);
        let metadata = SocketAddr::new("169.254.169.254".parse().expect("ip"), 80);

        assert!(check_ssrf_for_addrs(&[loopback], "127.0.0.1", false).is_err());
        assert!(check_ssrf_for_addrs(&[loopback], "localhost", false).is_err());
        assert!(check_ssrf_for_addrs(&[loopback], "127.0.0.1", true).is_ok());
        assert!(check_ssrf_for_addrs(&[loopback], "localhost", true).is_ok());
        assert!(check_ssrf_for_addrs(&[mapped_loopback], "[::ffff:127.0.0.1]", true).is_ok());

        // DNS rebinding of external host to loopback stays blocked even when allow_local=true.
        assert!(check_ssrf_for_addrs(&[loopback], "evil.example.com", true).is_err());
        // Cloud metadata stays blocked regardless of host or allow_local.
        assert!(check_ssrf_for_addrs(&[metadata], "169.254.169.254", true).is_err());
        assert!(check_ssrf_for_addrs(&[metadata], "localhost", true).is_err());
    }

    #[test]
    fn benchmarking_fake_ip_is_allowed_for_a_hostname_and_blocked_as_a_literal() {
        let fake = SocketAddr::new("198.18.3.161".parse().expect("ip"), 443);
        let mapped_fake = SocketAddr::new("::ffff:198.18.0.60".parse().expect("ip"), 443);
        let literal_private = SocketAddr::new("10.0.0.1".parse().expect("ip"), 443);

        assert!(check_ssrf_for_addrs(&[fake], "example.com", false).is_ok());
        assert!(check_ssrf_for_addrs(&[mapped_fake], "example.org", false).is_ok());
        assert!(check_ssrf_for_addrs(&[fake], "198.18.3.161", false).is_err());
        assert!(check_ssrf_for_addrs(&[fake], "[::ffff:198.18.3.161]", false).is_err());
        // A hostname that also resolves to a real private address stays blocked.
        assert!(check_ssrf_for_addrs(&[fake, literal_private], "example.com", true).is_err());
    }

    #[test]
    fn browser_data_url_accepts_html_and_plain_documents_only() {
        assert!(
            validate_browser_data_url(
                "data:text/html;charset=utf-8,<html><button id=\"btn\">Verify Button</button></html>"
            )
            .is_ok()
        );
        assert!(validate_browser_data_url("data:text/plain,hello").is_ok());
        assert!(validate_browser_data_url("data:,hello").is_ok());
        assert!(validate_browser_data_url("data:text/javascript,alert(1)").is_err());
        assert!(validate_browser_data_url("data:text/html;base64,").is_err());
        let oversized = format!("data:text/html,{}", "a".repeat(MAX_BROWSER_DATA_URL_BYTES));
        assert!(validate_browser_data_url(&oversized).is_err());
    }
}
