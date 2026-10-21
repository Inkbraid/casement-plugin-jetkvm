//! The picture: the device's H.264 stream decoded into the display, and a
//! status picture while the device has no video to show.
//!
//! Decoding runs on its own thread so the connection never waits for it.
//! Each picture is compared with the last in 64-pixel tiles before it is
//! converted: only tiles that changed are converted, copied and marked as
//! damage, which keeps both the work and the `frame` messages small for a
//! mostly still desktop.

use super::font;
use super::h264::{self, Decode};
use super::yuv::{Frame, Planes, Rect};
use crate::host::Host;
use crate::protocol::State;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::Arc;

const TILE: usize = 64;
/// The status picture's size before there has been a picture.
const STATUS_SIZE: (u32, u32) = (1280, 720);
pub const STATUS_BACKGROUND: [u8; 4] = [0x30, 0x2A, 0x26, 0xFF];
pub const STATUS_TEXT: [u8; 4] = [0xF0, 0xF0, 0xF0, 0xFF];
const STATUS_DETAIL: [u8; 4] = [0xB0, 0xB0, 0xB0, 0xFF];

pub enum Video {
    /// An access unit (Annex B). `contiguous` is false after lost packets.
    Frame {
        data: Arc<[u8]>,
        contiguous: bool,
    },
    /// What to show instead of the picture: a title and a sentence.
    Status(&'static str, &'static str),
    Stop,
}

/// Pictures waiting to be decoded. When decoding falls this far behind,
/// the session skips to the next key frame rather than fall further behind.
pub const QUEUE: usize = 10;

/// Starts the decoding thread.
pub fn start(host: Arc<Host>) -> SyncSender<Video> {
    let (tx, rx) = mpsc::sync_channel(QUEUE);
    if let Err(e) = std::thread::Builder::new()
        .name("video".into())
        .spawn(move || run(host, rx))
    {
        log::error!("can't start the video thread: {e}");
    }
    tx
}

fn run(host: Arc<Host>, rx: Receiver<Video>) {
    // Created on this thread (Media Foundation is per thread), at the first
    // key frame.
    let mut decoder: Option<Box<dyn Decode>> = None;
    let mut waiting_for_key = true;
    let mut frames = 0u64;
    // The picture on show, to compare the next one with.
    let mut shown: Option<Planes> = None;
    while let Ok(msg) = rx.recv() {
        let (data, contiguous) = match msg {
            Video::Stop => return,
            Video::Status(title, detail) => {
                draw_status(&host, title, detail);
                shown = None;
                // The device restarts its stream with a key frame.
                waiting_for_key = true;
                continue;
            }
            Video::Frame { data, contiguous } => (data, contiguous),
        };
        if !contiguous && !waiting_for_key {
            // JetKVM sends a key frame every half second, and ignores
            // requests for one: wait for it rather than show a damaged picture.
            log::debug!("video packets lost; waiting for a key frame");
            waiting_for_key = true;
        }
        if waiting_for_key {
            if !h264::is_key(&data) {
                continue;
            }
            waiting_for_key = false;
        }
        let dec = match &mut decoder {
            Some(d) => d,
            None => match h264::decoder() {
                Ok(d) => decoder.insert(d),
                Err(e) => host.finish(State::Failed, &e),
            },
        };
        match dec.decode(&data) {
            Ok(Some(f)) => {
                if frames == 0 {
                    log::info!("first picture: {}x{}", f.width, f.height);
                }
                frames += 1;
                show(&host, &f, &mut shown);
            }
            Ok(None) => {}
            Err(e) => {
                log::warn!("{e}");
                waiting_for_key = true;
            }
        }
    }
}

/// Puts a decoded picture in the display: all of it after a change of size
/// (or a status picture), otherwise the tiles that differ from `shown`.
fn show(host: &Host, f: &Frame, shown: &mut Option<Planes>) {
    let mut d = host.display();
    let size = (f.width as u32, f.height as u32);
    let comparable = d.size() == size && shown.as_ref().is_some_and(|p| p.matches(f));
    let dirty = {
        if !comparable {
            d.set_size(size.0, size.1);
        }
        let (buf, stride) = d.buffer_mut();
        match shown {
            Some(p) if comparable => changed_tiles(f, p, |r| f.convert(r, buf, stride)),
            _ => {
                f.convert([0, 0, f.width, f.height], buf, stride);
                *shown = Some(Planes::new(f));
                vec![[0, 0, size.0, size.1]]
            }
        }
    };
    for [x, y, w, h] in dirty {
        d.damage(x, y, w, h);
    }
    d.flush();
}

/// Finds the tiles of `f` that differ from `shown`, which takes them in,
/// calls `changed` with each, and returns them as rectangles, adjacent
/// tiles in a row joined.
fn changed_tiles(f: &Frame, shown: &mut Planes, mut changed: impl FnMut(Rect)) -> Vec<[u32; 4]> {
    let (w, h) = (f.width, f.height);
    let mut dirty = Vec::new();
    let mut marks = vec![false; w.div_ceil(TILE)];
    for ty in (0..h).step_by(TILE) {
        let th = TILE.min(h - ty);
        marks.fill(false);
        shown.compare_band(f, ty, th, TILE, &mut marks);
        let mut run: Option<(usize, usize)> = None;
        for (i, &mark) in marks.iter().enumerate() {
            let (tx, tw) = (i * TILE, TILE.min(w - i * TILE));
            if mark {
                changed([tx, ty, tw, th]);
                run = Some((run.map_or(tx, |r| r.0), tx + tw));
            } else if let Some((s, e)) = run.take() {
                dirty.push([s as u32, ty as u32, (e - s) as u32, th as u32]);
            }
        }
        if let Some((s, e)) = run {
            dirty.push([s as u32, ty as u32, (e - s) as u32, th as u32]);
        }
    }
    dirty
}

/// Draws a status picture: a title and a sentence, centred, at the current
/// size (or a default one before the first picture).
fn draw_status(host: &Host, title: &str, detail: &str) {
    log::info!("status: {title}: {detail}");
    let mut d = host.display();
    let (mut w, mut h) = d.size();
    if w == 0 || h == 0 {
        (w, h) = STATUS_SIZE;
        d.set_size(w, h);
    }
    let (buf, stride) = d.buffer_mut();
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&STATUS_BACKGROUND);
    }
    let (w, h) = (w as usize, h as usize);
    // As large as fits in three quarters of the width, up to a limit.
    let fit = |text: &str, max: usize| (w * 3 / 4 / (text.len().max(1) * 8)).clamp(1, max);
    let big = fit(title, 6);
    let small = fit(detail, 3);
    let total = 8 * big + 8 * small * 2;
    let top = h.saturating_sub(total) / 2;
    draw_text(buf, stride, (w, h), title, big, top, STATUS_TEXT);
    draw_text(
        buf,
        stride,
        (w, h),
        detail,
        small,
        top + 8 * big + 8 * small,
        STATUS_DETAIL,
    );
    d.damage_all();
    d.flush();
}

/// Draws one line of text, centred horizontally, `scale` pixels per font pixel.
fn draw_text(
    buf: &mut [u8],
    stride: usize,
    (w, h): (usize, usize),
    text: &str,
    scale: usize,
    top: usize,
    colour: [u8; 4],
) {
    let width = text.chars().count() * 8 * scale;
    let left = w.saturating_sub(width) / 2;
    for (i, c) in text.chars().enumerate() {
        let g = font::glyph(c);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..8 {
                if bits >> col & 1 == 0 {
                    continue;
                }
                let x0 = left + (i * 8 + col) * scale;
                let y0 = top + row * scale;
                for y in y0..(y0 + scale).min(h) {
                    for x in x0..(x0 + scale).min(w) {
                        let o = y * stride + x * 4;
                        buf[o..o + 4].copy_from_slice(&colour);
                    }
                }
            }
        }
    }
}

/// What to show for the device's `videoInputState`, or `None` when there is
/// a picture.
pub fn status_for(state: &serde_json::Value) -> Option<(&'static str, &'static str)> {
    if state.get("ready").and_then(|r| r.as_bool()) == Some(true) {
        return None;
    }
    Some(
        match state.get("error").and_then(|e| e.as_str()).unwrap_or("") {
            "no_signal" => (
                "No signal",
                "The computer is off, asleep, or not connected to the JetKVM.",
            ),
            "no_lock" => (
                "No stable signal",
                "The JetKVM can't lock on to the computer's video signal.",
            ),
            "out_of_range" => (
                "Signal out of range",
                "The computer's resolution or refresh rate is beyond the JetKVM.",
            ),
            _ => ("Waiting for video", "The JetKVM has no picture yet."),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_changed_tiles() {
        use crate::jetkvm::yuv::Chroma;
        let (w, h) = (200, 100);
        let mut y = vec![16u8; w * h];
        let c = vec![128u8; w.div_ceil(2) * h.div_ceil(2)];
        let frame = |y| Frame {
            width: w,
            height: h,
            y,
            y_stride: w,
            chroma: Chroma::Planar {
                u: &c,
                v: &c,
                u_stride: w / 2,
                v_stride: w / 2,
            },
        };
        let before = y.clone();
        let mut shown = Planes::new(&frame(&before));
        // Two changed pixels in adjacent tiles of the first tile row, one in
        // the second.
        for (x, row) in [(10, 5), (70, 60), (150, 90)] {
            y[row * w + x] = 200;
        }
        let mut converted = Vec::new();
        let dirty = changed_tiles(&frame(&y), &mut shown, |r| converted.push(r));
        assert_eq!(dirty, vec![[0, 0, 128, 64], [128, 64, 64, 36]]);
        assert_eq!(
            converted,
            vec![[0, 0, 64, 64], [64, 0, 64, 64], [128, 64, 64, 36]]
        );
        assert!(changed_tiles(&frame(&y), &mut shown, |_| panic!("no change")).is_empty());
    }

    #[test]
    fn statuses() {
        assert_eq!(status_for(&json!({"ready": true, "width": 1920})), None);
        assert_eq!(
            status_for(&json!({"ready": false, "error": "no_signal"})).map(|s| s.0),
            Some("No signal")
        );
        assert_eq!(
            status_for(&json!({"ready": false})).map(|s| s.0),
            Some("Waiting for video")
        );
    }

    #[test]
    fn text_stays_inside() {
        let (w, h) = (100, 20);
        let mut buf = vec![0u8; w * h * 4];
        draw_text(
            &mut buf,
            w * 4,
            (w, h),
            "WIDE TEXT THAT OVERFLOWS",
            2,
            10,
            [1, 2, 3, 4],
        );
        assert!(buf.chunks_exact(4).any(|p| p == [1, 2, 3, 4]));
    }
}
