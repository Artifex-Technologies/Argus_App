//! Flipper Zero recovery for Argus, the Rust port of the C++ app's `dfu/` (the STM32WB55's ROM
//! bootloader speaks ST's DfuSe over USB, VID 0483 PID DF11).
//!
//! - [`dfuse`]: DfuSe files (`.dfu`), parsed with every length checked first.
//! - [`layout`]: the memory layout a DfuSe device describes in its interface strings.
//!
//! Everything here is pure code over bytes; nothing opens a device.

pub mod dfuse;
pub mod layout;
