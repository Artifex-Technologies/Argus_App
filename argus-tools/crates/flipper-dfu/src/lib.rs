//! Flipper Zero recovery for Argus, the Rust port of the C++ app's `dfu/` (the STM32WB55's ROM
//! bootloader speaks ST's DfuSe over USB, VID 0483 PID DF11).
//!
//! - [`dfuse`]: DfuSe files (`.dfu`), parsed with every length checked first.
//! - [`layout`]: the memory layout a DfuSe device describes in its interface strings.
//! - [`device`]: ST's DfuSe protocol over a [`device::Transport`] (status polling, set address,
//!   erase, download, upload, leave), every wait bounded.
//!
//! - `usb` (feature `usb` only): the nusb transport for a real device, which no test uses.
//!
//! Without the `usb` feature everything here is pure code and nothing can open a device. The tests
//! drive [`device::Dfu`] with a fake device.

pub mod device;
pub mod dfuse;
pub mod layout;
#[cfg(feature = "usb")]
pub mod usb;
