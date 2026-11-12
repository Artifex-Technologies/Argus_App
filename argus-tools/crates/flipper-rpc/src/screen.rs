//! The device's screen, as the C++ app's `ScreenStreamer` (backend/flipperzero/screenstreamer.cpp)
//! receives it: `StartScreenStreamRequest`, then frames as broadcasts (command id 0), then
//! `StopScreenStreamRequest`. Watching the screen changes nothing on the device.
//!
//! A frame is the 128x64 monochrome screen, 1024 bytes: byte `y / 8 * 128 + x`, bit `y % 8` is the
//! pixel at (x, y), as the app's canvas reads it. A frame of any other length is dropped, as the
//! C++ streamer drops it since 2026-11-12 (its canvas read past the end of a short one).
//!
//! The app keeps the stream open ([`Session::screen_start`], then [`Session::next_screen_frame`]
//! between its other requests, [`Session::screen_stop`]) and sends the keypad's input events
//! ([`Session::gui_input`]) as the C++ `ScreenStreamer::sendInputEvent` does. An input event is
//! the one request here that changes what the device does: it is a button press.

use std::time::{Duration, Instant};

use crate::proto::pb::main::Content;
use crate::proto::pb_gui::{self, InputKey, InputType};
use crate::session::{RpcError, Session};
use crate::transport::Transport;

pub const WIDTH: usize = 128;
pub const HEIGHT: usize = 64;
pub const FRAME_LEN: usize = WIDTH * HEIGHT / 8;
/// At most this many frames per call.
pub const MAX_FRAMES: usize = 10_000;
/// Broadcasts that are not frames, or frames of the wrong length, accepted while waiting.
const MAX_OTHER: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub data: Vec<u8>,
    /// `PB_Gui.ScreenOrientation`: 0 horizontal, 1 horizontal flipped, 2 vertical, 3 vertical
    /// flipped.
    pub orientation: i32,
}

impl Frame {
    /// The pixel at (x, y), true when lit.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        self.data[y / 8 * WIDTH + x] & (1 << (y % 8)) != 0
    }

    /// A binary PBM image (P4): lit pixels black, as the screen shows them, in the frame's own
    /// (unrotated) layout.
    pub fn to_pbm(&self) -> Vec<u8> {
        let mut out = format!("P4\n{WIDTH} {HEIGHT}\n").into_bytes();
        for y in 0..HEIGHT {
            for xb in 0..WIDTH / 8 {
                let mut b = 0u8;
                for bit in 0..8 {
                    if self.pixel(xb * 8 + bit, y) {
                        b |= 0x80 >> bit;
                    }
                }
                out.push(b);
            }
        }
        out
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Frames {
    pub frames: Vec<Frame>,
    /// Frames dropped for their length.
    pub dropped: usize,
}

impl<T: Transport> Session<T> {
    /// Starts the screen stream, collects `count` frames (each read waits at most the session's
    /// timeout), and stops the stream again.
    pub fn screen_frames(&mut self, count: usize) -> Result<Frames, RpcError> {
        if count == 0 || count > MAX_FRAMES {
            return Err(RpcError::Invalid(format!("between 1 and {MAX_FRAMES} frames")));
        }
        self.request(Content::GuiStartScreenStreamRequest(pb_gui::StartScreenStreamRequest {}), |_| Ok(()))?;
        let mut out = Frames::default();
        let mut other = 0usize;
        let collected = self.receive(0, |m| {
            match m.content {
                Some(Content::GuiScreenFrame(f)) if f.data.len() == FRAME_LEN => {
                    out.frames.push(Frame { data: f.data, orientation: f.orientation });
                }
                Some(Content::GuiScreenFrame(_)) => {
                    out.dropped += 1;
                    other += 1;
                }
                _ => other += 1,
            }
            if other > MAX_OTHER {
                return Err(RpcError::Limit(format!("more than {MAX_OTHER} broadcasts that were not usable frames")));
            }
            Ok(out.frames.len() >= count)
        });
        // Stop the stream even when collecting failed, if the session is still usable.
        let stopped = self.request(Content::GuiStopScreenStreamRequest(pb_gui::StopScreenStreamRequest {}), |_| Ok(()));
        collected?;
        stopped?;
        Ok(out)
    }
}

impl<T: Transport> Session<T> {
    /// `GuiStartScreenStreamRequest`: frames follow as broadcasts until [`Session::screen_stop`].
    pub fn screen_start(&mut self) -> Result<(), RpcError> {
        self.request(Content::GuiStartScreenStreamRequest(pb_gui::StartScreenStreamRequest {}), |_| Ok(()))
    }

    /// `GuiStopScreenStreamRequest`. Frames already on their way are skipped by the requests
    /// that follow, as any broadcast is.
    pub fn screen_stop(&mut self) -> Result<(), RpcError> {
        self.request(Content::GuiStopScreenStreamRequest(pb_gui::StopScreenStreamRequest {}), |_| Ok(()))
    }

    /// The next screen frame of an open stream, waiting at most `wait`; `None` when none came.
    /// Frames of the wrong length and other broadcasts are skipped (at most [`MAX_OTHER`] per
    /// call); a quiet device leaves the session usable.
    pub fn next_screen_frame(&mut self, wait: Duration) -> Result<Option<Frame>, RpcError> {
        let deadline = Instant::now() + wait;
        let mut other = 0usize;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(m) = self.poll_message(left)? else { return Ok(None) };
            match m.content {
                Some(Content::GuiScreenFrame(f)) if m.command_id == 0 && f.data.len() == FRAME_LEN => {
                    return Ok(Some(Frame { data: f.data, orientation: f.orientation }));
                }
                _ => other += 1,
            }
            if other > MAX_OTHER {
                return Err(RpcError::Limit(format!("more than {MAX_OTHER} messages that were not usable frames")));
            }
            if left.is_zero() {
                return Ok(None);
            }
        }
    }

    /// `GuiSendInputEventRequest`: one input event (a press, release, short, long or repeat of
    /// one button), as the keypad sends them.
    pub fn gui_input(&mut self, key: InputKey, kind: InputType) -> Result<(), RpcError> {
        let req = pb_gui::SendInputEventRequest { key: key as i32, r#type: kind as i32 };
        self.request(Content::GuiSendInputEventRequest(req), |_| Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_and_pbm() {
        let mut data = vec![0u8; FRAME_LEN];
        data[0] = 0b0000_0001; // (0, 0)
        data[WIDTH + 5] = 0b1000_0000; // (5, 15)
        let f = Frame { data, orientation: 0 };
        assert!(f.pixel(0, 0) && f.pixel(5, 15) && !f.pixel(1, 0));
        let pbm = f.to_pbm();
        let header = b"P4\n128 64\n".len();
        assert_eq!(pbm.len(), header + FRAME_LEN);
        assert_eq!(pbm[header], 0x80);
        assert_eq!(pbm[header + 15 * 16], 0b0000_0100);
    }
}
