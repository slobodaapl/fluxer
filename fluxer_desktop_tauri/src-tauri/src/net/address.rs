// SPDX-License-Identifier: AGPL-3.0-or-later

//! IP literal parsing and address scope, ported from
//! `packages/ip_utils/src/IpAddress.ts` (`parseIpAddress`, `isPublicIpAddress`).

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::js::js_trim;

/// `(base, prefix length)` IPv4 ranges that are never publicly routable.
const RESERVED_IPV4_RANGES: [(u32, u32); 14] = [
    (0x0000_0000, 8),
    (0x0a00_0000, 8),
    (0x6440_0000, 10),
    (0x7f00_0000, 8),
    (0xa9fe_0000, 16),
    (0xac10_0000, 12),
    (0xc000_0000, 24),
    (0xc000_0200, 24),
    (0xc0a8_0000, 16),
    (0xc612_0000, 15),
    (0xc633_6400, 24),
    (0xcb00_7100, 24),
    (0xe000_0000, 4),
    (0xf000_0000, 4),
];

/// Parses an IP literal the way `normalizeIpString` + `parseIpAddress` do:
/// trims, strips one pair of enclosing brackets, drops an IPv6 zone
/// identifier, then accepts strict dotted-quad IPv4 or IPv6.
pub fn parse_ip_literal(host: &str) -> Option<IpAddr> {
    let trimmed = js_trim(host);
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(trimmed);
    let address = match unbracketed.split_once('%') {
        Some((address, _zone)) if address.contains(':') => address,
        _ => unbracketed,
    };
    if let Ok(v4) = address.parse::<Ipv4Addr>() {
        return Some(IpAddr::V4(v4));
    }
    address.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
}

fn is_public_ipv4(addr: Ipv4Addr) -> bool {
    let value = u32::from(addr);
    !RESERVED_IPV4_RANGES.iter().any(|&(base, prefix)| {
        let mask = u32::MAX << (32 - prefix);
        value & mask == base & mask
    })
}

/// Public means globally routable unicast: IPv4 outside the reserved ranges,
/// IPv6 inside 2000::/3 except 2001:db8::/32. IPv4-mapped IPv6 uses the IPv4 rule.
pub fn is_public_ip(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => is_public_ipv4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_ipv4(v4);
            }
            let [first, second, ..] = v6.segments();
            first & 0xe000 == 0x2000 && !(first == 0x2001 && second == 0x0db8)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    fn public(value: &str) -> bool {
        parse_ip_literal(value).is_some_and(is_public_ip)
    }

    // IpAddress.test.ts: normalizeIpString / parseIpAddress / isValidIp.
    #[test]
    fn parses_ipv4_and_trims_whitespace() {
        assert_eq!(parse_ip_literal("192.168.1.1"), Some(ip("192.168.1.1")));
        assert_eq!(parse_ip_literal("10.0.0.1"), Some(ip("10.0.0.1")));
        assert_eq!(parse_ip_literal("172.16.0.1"), Some(ip("172.16.0.1")));
        assert_eq!(parse_ip_literal("  192.168.1.1  "), Some(ip("192.168.1.1")));
        assert_eq!(parse_ip_literal("\t10.0.0.1\n"), Some(ip("10.0.0.1")));
    }

    #[test]
    fn parses_ipv6_brackets_zones_case_and_compact_forms() {
        assert_eq!(parse_ip_literal("2001:db8::1"), Some(ip("2001:db8::1")));
        assert_eq!(parse_ip_literal("::1"), Some(ip("::1")));
        assert_eq!(parse_ip_literal("::"), Some(ip("::")));
        assert_eq!(parse_ip_literal("[2001:db8::1]"), Some(ip("2001:db8::1")));
        assert_eq!(parse_ip_literal("fe80::1%eth0"), Some(ip("fe80::1")));
        assert_eq!(parse_ip_literal("[fe80::1%en0]"), Some(ip("fe80::1")));
        assert_eq!(parse_ip_literal("2001:DB8::1"), Some(ip("2001:db8::1")));
        assert_eq!(
            parse_ip_literal("2001:0db8:0000:0000:0000:0000:0000:0001"),
            Some(ip("2001:db8::1"))
        );
        assert_eq!(parse_ip_literal("[2001:DB8::1]"), Some(ip("2001:db8::1")));
        assert_eq!(
            parse_ip_literal("::ffff:192.0.2.1"),
            Some(ip("::ffff:c000:201"))
        );
        assert_eq!(parse_ip_literal("[::1]"), Some(ip("::1")));
    }

    #[test]
    fn rejects_non_literals() {
        for value in [
            "",
            "   ",
            "not-an-ip",
            "[2001:db8::1]:8080",
            "256.256.256.256",
            "gggg::1",
            "example.com",
            // A zone suffix is only stripped from IPv6 text.
            "1.2.3.4%eth0",
            // Node's isIPv4 rejects octal-looking octets.
            "01.2.3.4",
        ] {
            assert_eq!(parse_ip_literal(value), None, "{value:?}");
        }
    }

    #[test]
    fn trims_like_ecmascript() {
        assert_eq!(parse_ip_literal("\u{FEFF}8.8.8.8"), Some(ip("8.8.8.8")));
        assert_eq!(parse_ip_literal("\u{0085}8.8.8.8"), None);
    }

    // IpAddress.test.ts: isPublicIpAddress.
    #[test]
    fn accepts_public_unicast() {
        assert!(public("8.8.8.8"));
        assert!(public("2606:4700:4700::1111"));
    }

    #[test]
    fn rejects_loopback_private_and_reserved_ipv4() {
        for value in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.0.5",
            "192.168.0.5",
            "100.64.0.5",
            "198.51.100.42",
        ] {
            assert!(!public(value), "{value}");
        }
    }

    #[test]
    fn rejects_mapped_loopback_and_reserved_ipv6() {
        for value in [
            "::ffff:127.0.0.1",
            "::1",
            "::2",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!public(value), "{value}");
        }
    }

    #[test]
    fn rejects_invalid_values() {
        assert!(!public("not-an-ip"));
    }

    #[test]
    fn reserved_range_boundaries() {
        for (inside, outside) in [
            ("0.255.255.255", "1.0.0.0"),
            ("100.127.255.255", "100.128.0.0"),
            ("172.31.255.255", "172.32.0.0"),
            ("192.0.0.255", "192.0.1.0"),
            ("198.19.255.255", "198.20.0.0"),
            ("203.0.113.255", "203.0.114.0"),
            ("255.255.255.255", "223.255.255.255"),
        ] {
            assert!(!public(inside), "{inside}");
            assert!(public(outside), "{outside}");
        }
        assert!(public("::ffff:8.8.8.8"));
        assert!(public("3fff:ffff::1"));
        assert!(!public("4000::1"));
        assert!(!public("1fff:ffff::1"));
        assert!(public("2001:db9::1"));
    }
}
