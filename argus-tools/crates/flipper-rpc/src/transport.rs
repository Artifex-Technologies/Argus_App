//! Byte transports a [`crate::Session`] runs over: the Flipper's USB serial port, or a Unix
//! socket carrying the same byte stream (the fake Flipper in tests, or a serial-to-socket bridge).
//!
//! Nothing here looks for devices. A serial port is opened only when a caller names its path.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// A bidirectional byte stream with a read timeout. A read that times out returns an error
/// whose kind is `TimedOut` or `WouldBlock` (sockets report the latter).
pub trait Transport: Read + Write + Send {
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()>;
}

impl Transport for UnixStream {
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        UnixStream::set_read_timeout(self, Some(timeout))
    }
}

impl Transport for Box<dyn Transport> {
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        (**self).set_read_timeout(timeout)
    }
}

/// The Flipper's CDC-ACM serial port.
pub struct SerialTransport {
    port: Box<dyn serialport::SerialPort>,
}

impl SerialTransport {
    /// Opens `path` (exclusively, as `serialport` does on Unix) and pulses DTR the way the C++
    /// app does before reading the CLI banner: low, 50 ms, high. The Flipper prints its banner
    /// when DTR goes high. The baud rate is irrelevant for USB CDC; 230400 is qFlipper's.
    pub fn open(path: &Path, timeout: Duration) -> io::Result<SerialTransport> {
        let name = path.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "serial port path is not UTF-8"))?;
        let mut port = serialport::new(name, 230_400).timeout(timeout).open().map_err(io::Error::from)?;
        port.write_data_terminal_ready(false).map_err(io::Error::from)?;
        std::thread::sleep(Duration::from_millis(50));
        port.write_data_terminal_ready(true).map_err(io::Error::from)?;
        Ok(SerialTransport { port })
    }
}

impl Read for SerialTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.port.read(buf)
    }
}

impl Write for SerialTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.port.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.port.flush()
    }
}

impl Transport for SerialTransport {
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.port.set_timeout(timeout).map_err(io::Error::from)
    }
}

/// True for the error kinds a read timeout produces on the transports above.
pub fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock)
}
