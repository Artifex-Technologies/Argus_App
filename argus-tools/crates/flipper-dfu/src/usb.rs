//! The real USB transport (nusb), a thin adapter from [`Transport`] to one opened STM32 DFU device;
//! built only with the `usb` feature. No test uses it: every test drives [`crate::device::Dfu`]
//! with a fake device. It never searches for a device on its own; the caller hands it the one to
//! open, and it refuses anything that is not ST's DFU bootloader (VID 0483, PID DF11).
//!
//! Like the C++ app's libusb backend (`dfu/libusb/usbdevice.cpp`), but a failed transfer is not
//! retried: retrying a download could run an erase or a write twice.

use std::num::NonZeroU8;
use std::time::Duration;

use nusb::descriptors::language_id::US_ENGLISH;
use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};
use nusb::{DeviceInfo, MaybeFuture};

use crate::device::{Transport, TransportError};

pub const VENDOR_ID: u16 = 0x0483;
pub const PRODUCT_ID: u16 = 0xDF11;
const TIMEOUT: Duration = Duration::from_secs(1);
const DFU_FUNCTIONAL: u8 = 0x21;

fn err(e: impl std::fmt::Display) -> TransportError {
    TransportError(e.to_string())
}

pub struct UsbTransport {
    device: nusb::Device,
    interface: nusb::Interface,
}

impl UsbTransport {
    /// Opens `info` and claims interface 0, if it is ST's DFU bootloader.
    pub fn open(info: &DeviceInfo) -> Result<UsbTransport, TransportError> {
        if (info.vendor_id(), info.product_id()) != (VENDOR_ID, PRODUCT_ID) {
            return Err(TransportError(format!("not an STM32 DFU device ({:04x}:{:04x})", info.vendor_id(), info.product_id())));
        }
        let device = info.open().wait().map_err(err)?;
        let interface = device.claim_interface(0).wait().map_err(err)?;
        Ok(UsbTransport { device, interface })
    }
}

impl Transport for UsbTransport {
    fn set_alt(&mut self, alt: u8) -> Result<(), TransportError> {
        self.interface.set_alt_setting(alt).wait().map_err(err)
    }

    fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<usize, TransportError> {
        let out = ControlOut { control_type: ControlType::Class, recipient: Recipient::Interface, request, value, index: 0, data };
        self.interface.control_out(out, TIMEOUT).wait().map_err(err)?;
        Ok(data.len())
    }

    fn control_in(&mut self, request: u8, value: u16, length: u16) -> Result<Vec<u8>, TransportError> {
        let req = ControlIn { control_type: ControlType::Class, recipient: Recipient::Interface, request, value, index: 0, length };
        self.interface.control_in(req, TIMEOUT).wait().map_err(err)
    }

    fn functional_descriptor(&mut self) -> Result<Vec<u8>, TransportError> {
        Ok(self
            .interface
            .descriptors()
            .flat_map(|alt| alt.descriptors().filter(|d| d.descriptor_type() == DFU_FUNCTIONAL).map(|d| d.to_vec()).collect::<Vec<_>>())
            .next()
            .unwrap_or_default())
    }

    fn interface_string(&mut self, alt: u8) -> Result<Vec<u8>, TransportError> {
        let index: Option<NonZeroU8> = self.interface.descriptors().find(|d| d.alternate_setting() == alt).and_then(|d| d.string_index());
        let index = index.ok_or_else(|| TransportError(format!("alternate setting {alt} has no interface string")))?;
        let s = self.device.get_string_descriptor(index, US_ENGLISH, TIMEOUT).wait().map_err(err)?;
        // ASCII, as libusb hands it to the C++ app
        Ok(s.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect())
    }

    fn sleep_ms(&mut self, ms: u32) {
        std::thread::sleep(Duration::from_millis(u64::from(ms)));
    }
}
