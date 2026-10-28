//! espion UDP wire format, shared by the firmware (`cam-p4`) and the host apps.
//!
//! This crate is the single source of truth for the wire format. Every packet is a fixed
//! raw header (magic, protocol version, message kind) followed by a postcard-encoded body;
//! see `docs/protocol.md`. Any change to the format bumps the protocol version and updates
//! that document.
//!
//! Not implemented yet.

#![no_std]
