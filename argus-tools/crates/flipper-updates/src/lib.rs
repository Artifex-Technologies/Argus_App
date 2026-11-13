//! Flipper's update server for Argus, the Rust port of the C++ app's `UpdateRegistry`,
//! `Flipper::Updates` (`flipperupdates.cpp`), `RemoteFileFetcher`, `RegionInfo` and the region data
//! step of `RegionProvisioningOperation`.
//!
//! - [`directory`]: `directory.json` (channels, versions, files with their SHA-256), read as Qt reads it.
//! - [`region`]: the region bundle (`/regions/api/v0/bundle`) and the `/int/.region_data` it becomes.
//! - [`fetch`]: HTTPS GET with rustls and the system's roots, https only, every body bounded, a
//!   file's SHA-256 checked before its bytes are handed out.
//! - [`repair`]: the files of a repair (firmware, wireless stack, option bytes, resources), as the
//!   C++ app's `FirmwareHelper` fetches and reads them.
//!
//! Nothing here talks to a device; nothing fetches on its own: callers name the server.

pub mod directory;
pub mod fetch;
pub mod region;
pub mod repair;

/// Flipper's update server (the host of the C++ app's directory and region URLs).
pub const DEFAULT_SERVER: &str = "https://update.flipperzero.one";

/// The firmware directory on a server (the C++ app's firmware directory URL).
pub fn directory_url(server: &str) -> String {
    format!("{}/firmware/directory.json", server.trim_end_matches('/'))
}

/// The region bundle on a server (`RegionProvisioningOperation`'s API URL).
pub fn region_url(server: &str) -> String {
    format!("{}/regions/api/v0/bundle", server.trim_end_matches('/'))
}
