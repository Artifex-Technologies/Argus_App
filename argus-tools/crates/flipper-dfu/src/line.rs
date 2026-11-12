//! A [`Transport`] over a text stream, one request per line: how `argus-cli --dfu-socket` reaches
//! the fake bootloader in tests (and the line protocol the C++ oracle speaks). It opens nothing
//! itself and never touches USB. Requests: `ALT n`, `OUT request value hex`, `IN request value
//! length`, `DESC`, `STRING alt`, each answered by one line (`OK`, `OK n`, `DATA hex`, `ERR`; `-` is
//! no bytes); `SLEEP ms` reports a wait and gets no answer (nothing waits: the other end is a fake).

use std::io::{BufRead, Read, Write};

use crate::device::{Transport, TransportError};

/// The longest answer line read (a control transfer is at most 64 KiB, as hex).
const MAX_LINE: u64 = 256 << 10;

pub struct LineTransport<R, W> {
    reader: R,
    writer: W,
}

fn hex(b: &[u8]) -> String {
    if b.is_empty() {
        return "-".into();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s == "-" {
        return Some(Vec::new());
    }
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2).map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()).collect()
}

fn err(m: impl Into<String>) -> TransportError {
    TransportError(m.into())
}

impl<R: BufRead, W: Write> LineTransport<R, W> {
    pub fn new(reader: R, writer: W) -> LineTransport<R, W> {
        LineTransport { reader, writer }
    }

    fn send(&mut self, line: &str) -> Result<(), TransportError> {
        writeln!(self.writer, "{line}").and_then(|()| self.writer.flush()).map_err(|e| err(e.to_string()))
    }

    fn ask(&mut self, line: &str) -> Result<String, TransportError> {
        self.send(line)?;
        let mut answer = String::new();
        let n = Read::take(&mut self.reader, MAX_LINE).read_line(&mut answer).map_err(|e| err(e.to_string()))?;
        if n == 0 || !answer.ends_with('\n') {
            return Err(err("the device has gone"));
        }
        answer.pop();
        Ok(answer)
    }

    fn data(&mut self, line: &str) -> Result<Vec<u8>, TransportError> {
        let a = self.ask(line)?;
        match a.strip_prefix("DATA ") {
            Some(h) => unhex(h).ok_or_else(|| err("bad answer")),
            None => Err(err("request failed")),
        }
    }
}

impl<R: BufRead, W: Write> Transport for LineTransport<R, W> {
    fn set_alt(&mut self, alt: u8) -> Result<(), TransportError> {
        match self.ask(&format!("ALT {alt}"))?.as_str() {
            "OK" => Ok(()),
            _ => Err(err("set alternate setting failed")),
        }
    }

    fn control_out(&mut self, request: u8, value: u16, data: &[u8]) -> Result<usize, TransportError> {
        let a = self.ask(&format!("OUT {request} {value} {}", hex(data)))?;
        a.strip_prefix("OK ").and_then(|n| n.parse().ok()).ok_or_else(|| err("request failed"))
    }

    fn control_in(&mut self, request: u8, value: u16, length: u16) -> Result<Vec<u8>, TransportError> {
        let d = self.data(&format!("IN {request} {value} {length}"))?;
        if d.len() > usize::from(length) {
            return Err(err("more bytes than asked for"));
        }
        Ok(d)
    }

    fn functional_descriptor(&mut self) -> Result<Vec<u8>, TransportError> {
        self.data("DESC")
    }

    fn interface_string(&mut self, alt: u8) -> Result<Vec<u8>, TransportError> {
        self.data(&format!("STRING {alt}"))
    }

    fn sleep_ms(&mut self, ms: u32) {
        let _ = self.send(&format!("SLEEP {ms}"));
    }
}
