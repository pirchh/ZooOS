//! DNS messages (docs/n3-design.md): a query for a name's IPv4 address,
//! and reading the answer. Pure functions over bytes, so they are tested on
//! the host; the guest sends and receives them over UDP.

/// Writes a query with id `id` for the A record of `name` into `out`;
/// returns its length, or `None` if the name is not valid or `out` is too
/// small.
pub fn query(id: u16, name: &str, out: &mut [u8]) -> Option<usize> {
    let name = name.trim_end_matches('.');
    if name.is_empty() || name.len() > 253 {
        return None;
    }
    let mut at = 12;
    let need = 12 + name.len() + 2 + 4;
    if out.len() < need {
        return None;
    }
    out[..12].fill(0);
    out[0..2].copy_from_slice(&id.to_be_bytes());
    out[2] = 0x01; // recursion desired
    out[5] = 1; // one question
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        out[at] = label.len() as u8;
        out[at + 1..at + 1 + label.len()].copy_from_slice(label.as_bytes());
        at += 1 + label.len();
    }
    out[at] = 0;
    // Type A, class IN.
    out[at + 1..at + 5].copy_from_slice(&[0, 1, 0, 1]);
    Some(at + 5)
}

/// Skips a (possibly compressed) name at `at`; the offset after it.
fn skip_name(msg: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *msg.get(at)?;
        if len == 0 {
            return Some(at + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(at + 2);
        }
        at += 1 + usize::from(len);
    }
}

/// Why an answer gave no address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsError {
    /// Not a reply to our query (id, flags or shape).
    NotOurs,
    /// The server says the name does not exist.
    NoSuchName,
    /// Some other error code from the server.
    Server(u8),
    /// A reply with no IPv4 address in it.
    NoAddress,
}

/// The first IPv4 address in reply `msg` to query `id`.
pub fn answer(id: u16, msg: &[u8]) -> Result<[u8; 4], DnsError> {
    if msg.len() < 12 || msg[0..2] != id.to_be_bytes() || msg[2] & 0x80 == 0 {
        return Err(DnsError::NotOurs);
    }
    match msg[3] & 0x0F {
        0 => {}
        3 => return Err(DnsError::NoSuchName),
        code => return Err(DnsError::Server(code)),
    }
    let questions = u16::from_be_bytes([msg[4], msg[5]]);
    let answers = u16::from_be_bytes([msg[6], msg[7]]);
    let mut at = 12;
    for _ in 0..questions {
        at = skip_name(msg, at).ok_or(DnsError::NotOurs)? + 4;
    }
    for _ in 0..answers {
        at = skip_name(msg, at).ok_or(DnsError::NotOurs)?;
        let rr = msg.get(at..at + 10).ok_or(DnsError::NotOurs)?;
        let (kind, class) = (
            u16::from_be_bytes([rr[0], rr[1]]),
            u16::from_be_bytes([rr[2], rr[3]]),
        );
        let len = usize::from(u16::from_be_bytes([rr[8], rr[9]]));
        let data = msg.get(at + 10..at + 10 + len).ok_or(DnsError::NotOurs)?;
        if kind == 1 && class == 1 && len == 4 {
            return Ok([data[0], data[1], data[2], data[3]]);
        }
        // CNAMEs and others: keep looking.
        at += 10 + len;
    }
    Err(DnsError::NoAddress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_bytes() {
        let mut out = [0u8; 64];
        let n = query(0x1234, "example.com", &mut out).unwrap();
        assert_eq!(
            &out[..n],
            b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03com\x00\x00\x01\x00\x01"
        );
        assert_eq!(query(1, "", &mut out), None);
        assert_eq!(query(1, "a..b", &mut out), None);
        assert_eq!(query(1, "example.com", &mut [0u8; 10]), None);
    }

    /// A reply as a resolver sends it: the question, then a CNAME and an
    /// A record whose names are compressed pointers.
    fn reply(id: u16, rcode: u8) -> Vec<u8> {
        let mut m = Vec::new();
        m.extend_from_slice(&id.to_be_bytes());
        m.extend_from_slice(&[0x81, 0x80 | rcode, 0, 1, 0, 2, 0, 0, 0, 0]);
        m.extend_from_slice(b"\x03www\x07example\x03com\x00\x00\x01\x00\x01");
        // CNAME www.example.com -> example.com (pointer to offset 16).
        m.extend_from_slice(&[0xC0, 12, 0, 5, 0, 1, 0, 0, 0, 60, 0, 2, 0xC0, 16]);
        m.extend_from_slice(&[0xC0, 16, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 93, 184, 215, 14]);
        m
    }

    #[test]
    fn answer_skips_cname() {
        assert_eq!(answer(7, &reply(7, 0)), Ok([93, 184, 215, 14]));
    }

    #[test]
    fn answer_rejects() {
        assert_eq!(answer(8, &reply(7, 0)), Err(DnsError::NotOurs));
        assert_eq!(answer(7, &reply(7, 3)), Err(DnsError::NoSuchName));
        assert_eq!(answer(7, &reply(7, 2)), Err(DnsError::Server(2)));
        let mut cut = reply(7, 0);
        cut.truncate(cut.len() - 3);
        assert_eq!(answer(7, &cut), Err(DnsError::NotOurs));
        let mut none = reply(7, 0);
        none[7] = 1; // the CNAME only
        assert_eq!(answer(7, &none), Err(DnsError::NoAddress));
    }
}
