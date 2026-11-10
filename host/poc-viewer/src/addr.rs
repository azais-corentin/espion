//! Camera addresses with an interface scope (`[fe80::4553:9281%eth0]:5009`), and the local
//! address to bind so nothing leaves through another interface.
//!
//! Interface names and link-local addresses come from `/proc/net/if_inet6` (Linux), which any
//! user can read.

use std::{
    fs,
    io::{self, ErrorKind},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6},
};

/// One line of `/proc/net/if_inet6`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Inet6 {
    addr: Ipv6Addr,
    index: u32,
    scope: u8,
    name: String,
}

/// IPv6 scope of link-local addresses in `/proc/net/if_inet6`.
const SCOPE_LINK: u8 = 0x20;

fn if_inet6() -> io::Result<Vec<Inet6>> {
    let text = fs::read_to_string("/proc/net/if_inet6")?;
    Ok(text.lines().filter_map(parse_if_inet6).collect())
}

fn parse_if_inet6(line: &str) -> Option<Inet6> {
    let mut f = line.split_whitespace();
    let hex = f.next()?;
    let addr = u128::from_str_radix(hex, 16).ok()?;
    let index = u32::from_str_radix(f.next()?, 16).ok()?;
    let _prefix = f.next()?;
    let scope = u8::from_str_radix(f.next()?, 16).ok()?;
    let _flags = f.next()?;
    let name = f.next()?.to_owned();
    Some(Inet6 {
        addr: Ipv6Addr::from(addr),
        index,
        scope,
        name,
    })
}

/// Parses `[addr%scope]:port`, `[addr]:port` or `ipv4:port`. The scope is an interface name or
/// index.
pub fn parse(s: &str) -> io::Result<SocketAddr> {
    let bad = |what: &str| io::Error::new(ErrorKind::InvalidInput, format!("{what}: {s}"));
    let Some((host, port)) = s.strip_prefix('[').and_then(|rest| rest.split_once("]:")) else {
        return s.parse().map_err(|_| bad("not an address with a port"));
    };
    let port: u16 = port.parse().map_err(|_| bad("bad port"))?;
    let (ip, scope) = match host.split_once('%') {
        Some((ip, scope)) => (ip, Some(scope)),
        None => (host, None),
    };
    let ip: Ipv6Addr = ip.parse().map_err(|_| bad("bad IPv6 address"))?;
    let scope_id = match scope {
        None => 0,
        Some(sc) => match sc.parse::<u32>() {
            Ok(i) => i,
            Err(_) => interface_index(sc)?,
        },
    };
    Ok(SocketAddr::V6(SocketAddrV6::new(ip, port, 0, scope_id)))
}

/// The index of interface `name`.
pub fn interface_index(name: &str) -> io::Result<u32> {
    if_inet6()?
        .into_iter()
        .find(|i| i.name == name)
        .map(|i| i.index)
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::NotFound,
                format!("no IPv6 interface named {name}"),
            )
        })
}

/// The local address to bind for talking to `camera`, port 0: for a link-local camera this
/// host's link-local address on the camera's interface (with its scope), for loopback the
/// loopback address, otherwise the unspecified address.
pub fn local_for(camera: SocketAddr) -> io::Result<SocketAddr> {
    match camera {
        SocketAddr::V4(a) if a.ip().is_loopback() => Ok((Ipv4Addr::LOCALHOST, 0).into()),
        SocketAddr::V4(_) => Ok((Ipv4Addr::UNSPECIFIED, 0).into()),
        SocketAddr::V6(a) if a.ip().is_loopback() => Ok((Ipv6Addr::LOCALHOST, 0).into()),
        SocketAddr::V6(a) if a.ip().is_unicast_link_local() => {
            if a.scope_id() == 0 {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("link-local {camera} needs an interface scope (%eth0)"),
                ));
            }
            let own = if_inet6()?
                .into_iter()
                .find(|i| i.index == a.scope_id() && i.scope == SCOPE_LINK)
                .ok_or_else(|| {
                    io::Error::new(
                        ErrorKind::NotFound,
                        format!("no link-local address on interface {}", a.scope_id()),
                    )
                })?;
            Ok(SocketAddr::V6(SocketAddrV6::new(own.addr, 0, 0, own.index)))
        }
        SocketAddr::V6(_) => Ok((Ipv6Addr::UNSPECIFIED, 0).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_lines() {
        let i = parse_if_inet6("fe80000000000000fae43bfffe922f05 06 40 20 80     eth0").unwrap();
        assert_eq!(
            i.addr,
            "fe80::fae4:3bff:fe92:2f05".parse::<Ipv6Addr>().unwrap()
        );
        assert_eq!((i.index, i.scope, i.name.as_str()), (6, 0x20, "eth0"));
    }

    #[test]
    fn parses_scoped_and_plain_addresses() {
        let a = parse("[fe80::4553:9281%7]:5009").unwrap();
        let SocketAddr::V6(v6) = a else { panic!() };
        assert_eq!(v6.scope_id(), 7);
        assert_eq!(v6.port(), 5009);
        assert_eq!(parse("[::1]:5009").unwrap(), "[::1]:5009".parse().unwrap());
        assert_eq!(
            parse("127.0.0.1:9").unwrap(),
            "127.0.0.1:9".parse().unwrap()
        );
        assert!(parse("[fe80::1%no-such-interface]:5009").is_err());
        assert!(parse("fe80::1").is_err());
    }

    #[test]
    fn loopback_binds_loopback() {
        assert_eq!(
            local_for("[::1]:5009".parse().unwrap()).unwrap(),
            "[::1]:0".parse().unwrap()
        );
        assert!(local_for("[fe80::1]:5009".parse().unwrap()).is_err());
    }
}
