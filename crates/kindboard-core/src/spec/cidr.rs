//! Minimal IPv4 CIDR parsing used by spec validation.
//!
//! Deliberately hand-rolled (a CIDR is an address + prefix length) instead of
//! pulling in a full IP crate: the only operations needed are parse and
//! overlap-check.

use std::fmt;

/// A parsed IPv4 CIDR block (`a.b.c.d/n`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ipv4Cidr {
    /// Network address as a 32-bit value (host bits already masked off).
    pub(crate) network: u32,
    /// Prefix length 0..=32.
    pub(crate) prefix: u8,
}

impl Ipv4Cidr {
    /// Parse `a.b.c.d/n`.
    pub(crate) fn parse(input: &str) -> Option<Self> {
        let (addr, prefix) = input.split_once('/')?;
        if prefix.is_empty() || prefix.len() > 2 {
            return None;
        }
        let prefix: u8 = prefix.parse().ok()?;
        if prefix > 32 {
            return None;
        }
        let octets: Vec<&str> = addr.split('.').collect();
        if octets.len() != 4 {
            return None;
        }
        let mut value: u32 = 0;
        for octet in octets {
            if octet.is_empty() {
                return None;
            }
            let n: u32 = octet.parse().ok()?;
            if n > 255 {
                return None;
            }
            value = (value << 8) | n;
        }
        let network = if prefix == 0 {
            0
        } else {
            value & (u32::MAX << (32 - prefix))
        };
        Some(Ipv4Cidr { network, prefix })
    }

    /// First address in the block.
    pub(crate) fn first(&self) -> u32 {
        self.network
    }

    /// Last address in the block.
    pub(crate) fn last(&self) -> u32 {
        match self.prefix {
            0 => u32::MAX,
            32 => self.network,
            p => self.network | (u32::MAX >> p),
        }
    }

    /// Whether `self` and `other` share any address.
    pub(crate) fn overlaps(&self, other: &Self) -> bool {
        self.first() <= other.last() && other.first() <= self.last()
    }
}

impl fmt::Display for Ipv4Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let a = (self.network >> 24) & 0xff;
        let b = (self.network >> 16) & 0xff;
        let c = (self.network >> 8) & 0xff;
        let d = self.network & 0xff;
        write!(f, "{a}.{b}.{c}.{d}/{}", self.prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_cidrs() {
        let c = Ipv4Cidr::parse("10.244.0.0/16").unwrap();
        assert_eq!(c.network, 0x0af40000);
        assert_eq!(c.prefix, 16);

        let c = Ipv4Cidr::parse("192.168.0.0/16").unwrap();
        assert_eq!(c.first(), 0xc0a80000);
        assert_eq!(c.last(), 0xc0a8ffff);

        assert!(Ipv4Cidr::parse("0.0.0.0/0").is_some());
        assert!(Ipv4Cidr::parse("255.255.255.255/32").is_some());
        assert!(Ipv4Cidr::parse("10.0.0.1/24").is_some());
    }

    #[test]
    fn rejects_invalid_cidrs() {
        assert!(Ipv4Cidr::parse("10.244.0.0").is_none());
        assert!(Ipv4Cidr::parse("10.244.0.0/33").is_none());
        assert!(Ipv4Cidr::parse("10.244.0.0/-1").is_none());
        assert!(Ipv4Cidr::parse("10.244.0.0/").is_none());
        assert!(Ipv4Cidr::parse("10.244.0.256/16").is_none());
        assert!(Ipv4Cidr::parse("10.244.0/16").is_none());
        assert!(Ipv4Cidr::parse("").is_none());
        assert!(Ipv4Cidr::parse("a.b.c.d/16").is_none());
        assert!(Ipv4Cidr::parse("10.244.0.0.1/16").is_none());
        assert!(Ipv4Cidr::parse("10.244..0/16").is_none());
    }

    #[test]
    fn overlap_detection() {
        let a = Ipv4Cidr::parse("10.0.0.0/8").unwrap();
        let b = Ipv4Cidr::parse("10.96.0.0/12").unwrap();
        let c = Ipv4Cidr::parse("192.168.0.0/16").unwrap();
        assert!(a.overlaps(&b));
        assert!(b.overlaps(&a));
        assert!(!a.overlaps(&c));
        assert!(!c.overlaps(&b));

        let d = Ipv4Cidr::parse("10.0.0.0/32").unwrap();
        assert!(a.overlaps(&d));
    }

    #[test]
    fn display_roundtrip() {
        for s in ["10.244.0.0/16", "0.0.0.0/0", "192.168.1.0/24"] {
            assert_eq!(Ipv4Cidr::parse(s).unwrap().to_string(), s);
        }
    }
}
