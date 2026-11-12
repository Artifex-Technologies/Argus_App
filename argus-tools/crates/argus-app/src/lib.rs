//! Argus's desktop app in Rust (egui/eframe): the port of the Qt app's `application/` folder on the Rust crates of the
//! port (flipper-rpc, argus-capture, argus-api, flipper-updates, flipper-dfu). See docs/RUST_PORT.md, phase 8.
//!
//! The app is a library so the tests can run it headless: `ArgusApp::ui` draws one frame into any `egui::Ui`.

pub mod app;
pub mod device;
pub mod discovery;
pub mod firmware;
pub mod net;
pub mod recovery;
pub mod settings;
pub mod theme;

pub use app::{ArgusApp, Config};
