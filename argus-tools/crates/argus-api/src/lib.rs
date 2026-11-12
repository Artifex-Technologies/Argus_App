//! The Sarina control API in Rust: the port of the C++ app's `backend/chymaera` server side
//! (`SarinaServer`, `Bridge::handleApiRequest`, `ChymaeraEventLog`). The contract is
//! `docs/SARINA_API.md`; the wire format is the C++ server's, byte for byte.
//!
//! - [`qjson`]: JSON as Qt reads and writes it.
//! - [`eventlog`]: the live event log (`log.tail`), persisted to the datastore.
//! - [`bridge`]: the commands.
//! - [`server`]: loopback TCP, the token, one request per line.
//! - [`pcap`]: pcap and pcapng files.

pub mod bridge;
pub mod eventlog;
pub mod pcap;
pub mod qjson;
pub mod server;
