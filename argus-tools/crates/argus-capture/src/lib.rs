//! Argus capture pull and datastore: the Rust port of the C++ app's `backend/chymaera`
//! capture side (CaptureSync, the datastore, the Flipper-format parsers).
//!
//! - [`sync`]: pull the capture folders (and only those) into the datastore.
//! - [`store`]: the SQLite datastore shared with the C++ GUI/CLI and read by the Sarina API.
//! - [`meta`]: what a capture says about itself, stored as its `meta` JSON.

pub mod meta;
pub mod store;
pub mod sync;
