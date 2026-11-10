//! espion UDP wire format, shared by the firmware and the host apps.
//!
//! This crate is the single source of truth for the wire format. Every datagram is a fixed raw
//! header (magic, protocol version, message kind) followed by one postcard-encoded body; see
//! `docs/protocol.md`. Any change to the format bumps [`VERSION`] and updates that document.
//!
//! - [`cam`]: the camera stream (kinds `0x20`..=`0x22`), used by the `poc-p4-stream` firmware
//!   and the `poc-viewer` host app.
//! - The tracking messages (kinds `0x01`, `0x02`, `0x10`, `0x11`) are a draft and not
//!   implemented yet: [`read_header`] reports them as [`Error::Kind`].
//!
//! `no_std` without `alloc`: every message encodes into and decodes from a caller's buffer.

#![no_std]

use core::fmt;

pub mod cam;

/// First two bytes of every datagram: `"ES"`.
pub const MAGIC: [u8; 2] = *b"ES";

/// Protocol version. Firmware and host must match exactly.
pub const VERSION: u8 = 3;

/// Length of the raw header: magic (2), version (1), kind (1).
pub const HEADER_LEN: usize = 4;

/// Message kinds (the header's fourth byte). A value is never reused.
pub mod kind {
    /// Draft, not implemented: centroids of one frame, board to host.
    pub const CENTROIDS: u8 = 0x01;
    /// Draft, not implemented: board status, board to host.
    pub const STATUS: u8 = 0x02;
    /// Reserved: was Announce in an earlier draft.
    pub const RESERVED_ANNOUNCE: u8 = 0x03;
    /// Draft, not implemented: detector configuration, host to board.
    pub const CONFIG: u8 = 0x10;
    /// Draft, not implemented: frame epoch, host to board.
    pub const FRAME_EPOCH: u8 = 0x11;
    /// Camera stream request, host to board ([`crate::cam::Request`]).
    pub const CAM_REQUEST: u8 = 0x20;
    /// Camera stream response, board to host ([`crate::cam::Response`]).
    pub const CAM_RESPONSE: u8 = 0x21;
    /// Camera frame chunk, board to host ([`crate::cam::FrameChunk`]).
    pub const FRAME_CHUNK: u8 = 0x22;
}

/// Why a datagram could not be encoded or decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// Shorter than the header.
    Short,
    /// The first two bytes are not [`MAGIC`]: not an espion datagram.
    Magic,
    /// A protocol version other than [`VERSION`] (the version found).
    Version(u8),
    /// A kind this version does not implement (the kind found).
    Kind(u8),
    /// The body does not decode as the kind's message.
    Body,
    /// Bytes left after the body: a datagram carries exactly one message.
    Trailing,
    /// The caller's buffer is too small for the message.
    BufferFull,
    /// A frame chunk off the chunk grid, outside its frame, or whose frame length disagrees
    /// with its encoding and size ([`cam::FrameChunk`]).
    ChunkLayout,
    /// A frame chunk with more than [`cam::CHUNK_MAX`] bytes of data.
    ChunkTooLong,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Short => f.write_str("datagram shorter than the header"),
            Self::Magic => f.write_str("not an espion datagram (magic)"),
            Self::Version(v) => write!(f, "protocol version {v}, expected {VERSION}"),
            Self::Kind(k) => write!(f, "message kind {k:#04x} not implemented"),
            Self::Body => f.write_str("body does not decode"),
            Self::Trailing => f.write_str("bytes after the message body"),
            Self::BufferFull => f.write_str("buffer too small for the message"),
            Self::ChunkLayout => f.write_str("frame chunk off the chunk grid of its frame"),
            Self::ChunkTooLong => f.write_str("frame chunk longer than the maximum"),
        }
    }
}

impl core::error::Error for Error {}

/// Writes the raw header for `kind` into the first [`HEADER_LEN`] bytes of `buf`.
pub fn write_header(kind: u8, buf: &mut [u8]) -> Result<(), Error> {
    let header = buf.get_mut(..HEADER_LEN).ok_or(Error::BufferFull)?;
    header.copy_from_slice(&[MAGIC[0], MAGIC[1], VERSION, kind]);
    Ok(())
}

/// Checks the header of `datagram` (length, magic, version) and returns its kind and body.
/// The kind is not checked here.
pub fn read_header(datagram: &[u8]) -> Result<(u8, &[u8]), Error> {
    let (header, body) = datagram.split_at_checked(HEADER_LEN).ok_or(Error::Short)?;
    if header[..2] != MAGIC {
        return Err(Error::Magic);
    }
    if header[2] != VERSION {
        return Err(Error::Version(header[2]));
    }
    Ok((header[3], body))
}

/// Encodes `kind` and `body` into `buf`; returns the datagram (a prefix of `buf`).
pub(crate) fn encode<'b, T: serde::Serialize>(
    kind: u8,
    body: &T,
    buf: &'b mut [u8],
) -> Result<&'b [u8], Error> {
    write_header(kind, buf)?;
    let n = postcard::to_slice(body, &mut buf[HEADER_LEN..])
        .map_err(|_| Error::BufferFull)?
        .len();
    Ok(&buf[..HEADER_LEN + n])
}

/// Decodes a whole body: exactly one message, no trailing bytes.
pub(crate) fn decode_body<'a, T: serde::Deserialize<'a>>(body: &'a [u8]) -> Result<T, Error> {
    match postcard::take_from_bytes(body) {
        Ok((v, [])) => Ok(v),
        Ok(_) => Err(Error::Trailing),
        Err(_) => Err(Error::Body),
    }
}
