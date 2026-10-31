//! The filesystem protocol between programs and BadgerHandler
//! (docs/s4-design.md §2). A folder handle is a channel; each request is one
//! message and gets one reply message.
//!
//! Request: `op u8 | arg u64 | count u16 | path_len u8 | path | data`.
//! Reply: `status i64` (bytes or a value when >= 0, `-error` otherwise)
//! followed by the result bytes. `Open` is followed, on success, by the new
//! folder channel as a handle message (tag `TAG_DIR`).

use crate::app::send_wait;
use crate::{Error, Handle, recv, recv_handle, send};
use alloc::vec::Vec;
use zoo_abi::{DirEntry, MAX_DIR_NAME, MAX_MESSAGE};

/// Handle-message tags.
pub const TAG_ROOT: u64 = 3;
pub const TAG_DIR: u64 = 4;

const HEADER: usize = 1 + 8 + 2 + 1;
/// Most file bytes in one reply.
pub const MAX_DATA: usize = MAX_MESSAGE - 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Op {
    Open = 1,
    List = 2,
    Stat = 3,
    Read = 4,
    CreateDir = 5,
    RemoveDir = 6,
    CreateFile = 7,
    Write = 8,
    Truncate = 9,
    RemoveFile = 10,
    Rename = 11,
}

impl Op {
    fn from_u8(b: u8) -> Option<Op> {
        Some(match b {
            1 => Op::Open,
            2 => Op::List,
            3 => Op::Stat,
            4 => Op::Read,
            5 => Op::CreateDir,
            6 => Op::RemoveDir,
            7 => Op::CreateFile,
            8 => Op::Write,
            9 => Op::Truncate,
            10 => Op::RemoveFile,
            11 => Op::Rename,
            _ => return None,
        })
    }
}

pub struct Request<'a> {
    pub op: Op,
    pub arg: u64,
    pub count: u16,
    pub path: &'a str,
    pub data: &'a [u8],
}

impl<'a> Request<'a> {
    pub fn decode(m: &'a [u8]) -> Option<Request<'a>> {
        if m.len() < HEADER {
            return None;
        }
        let op = Op::from_u8(m[0])?;
        let arg = u64::from_le_bytes(m[1..9].try_into().ok()?);
        let count = u16::from_le_bytes(m[9..11].try_into().ok()?);
        let plen = m[11] as usize;
        let path = core::str::from_utf8(m.get(HEADER..HEADER + plen)?).ok()?;
        Some(Request {
            op,
            arg,
            count,
            path,
            data: &m[HEADER + plen..],
        })
    }
}

/// Most data bytes a request with this path can carry.
pub fn room(path: &str) -> usize {
    MAX_MESSAGE.saturating_sub(HEADER + path.len())
}

fn encode(
    op: Op,
    arg: u64,
    count: u16,
    path: &str,
    data: &[u8],
    out: &mut [u8; MAX_MESSAGE],
) -> Result<usize, Error> {
    if path.len() > 255 || HEADER + path.len() + data.len() > MAX_MESSAGE {
        return Err(Error::Inval);
    }
    out[0] = op as u8;
    out[1..9].copy_from_slice(&arg.to_le_bytes());
    out[9..11].copy_from_slice(&count.to_le_bytes());
    out[11] = path.len() as u8;
    out[HEADER..HEADER + path.len()].copy_from_slice(path.as_bytes());
    let end = HEADER + path.len() + data.len();
    out[HEADER + path.len()..end].copy_from_slice(data);
    Ok(end)
}

/// Sends a reply: status, then `payload`.
pub fn reply(chan: Handle, status: Result<u64, Error>, payload: &[u8]) -> Result<(), Error> {
    let mut m = [0u8; MAX_MESSAGE];
    let s: i64 = match status {
        Ok(v) => v as i64,
        Err(e) => e.as_return(),
    };
    m[..8].copy_from_slice(&s.to_le_bytes());
    let n = payload.len().min(MAX_MESSAGE - 8);
    m[8..8 + n].copy_from_slice(&payload[..n]);
    send_wait(chan, &m[..8 + n])
}

/// One request and its reply: (value, reply bytes after the status, copied
/// into `out`, count).
pub fn call(
    dir: Handle,
    op: Op,
    arg: u64,
    count: u16,
    path: &str,
    data: &[u8],
    out: &mut [u8],
) -> Result<(u64, usize), Error> {
    let mut m = [0u8; MAX_MESSAGE];
    let n = encode(op, arg, count, path, data, &mut m)?;
    send(dir, &m[..n])?;
    let mut r = [0u8; MAX_MESSAGE];
    let n = recv(dir, &mut r)?;
    if n < 8 {
        return Err(Error::Inval);
    }
    let status = i64::from_le_bytes(r[..8].try_into().map_err(|_| Error::Inval)?);
    if status < 0 {
        return Err(Error::from_return(status).unwrap_or(Error::Inval));
    }
    let k = (n - 8).min(out.len());
    out[..k].copy_from_slice(&r[8..8 + k]);
    Ok((status as u64, k))
}

pub fn encode_entry(e: &DirEntry, out: &mut Vec<u8>) {
    out.push(e.kind as u8);
    out.extend_from_slice(&e.size.to_le_bytes());
    let n = (e.name_len as usize).min(MAX_DIR_NAME);
    out.extend_from_slice(&e.name[..n]);
}

pub fn decode_entry(b: &[u8]) -> Option<DirEntry> {
    if b.len() < 9 {
        return None;
    }
    let mut e = DirEntry {
        kind: b[0] as u64,
        size: u64::from_le_bytes(b[1..9].try_into().ok()?),
        ..DirEntry::default()
    };
    let name = &b[9..];
    let n = name.len().min(MAX_DIR_NAME);
    e.name[..n].copy_from_slice(&name[..n]);
    e.name_len = n as u64;
    Some(e)
}

/// Waits for the folder channel that follows a successful `Open`.
pub fn take_dir(dir: Handle) -> Result<Handle, Error> {
    let (h, _tag) = recv_handle(dir)?;
    Ok(h)
}
