//! H.264 decoding.
//!
//! The shipped Windows build uses Windows' own decoder (Media Foundation),
//! so no H.264 implementation is compiled into the plugin. Other platforms
//! (the Linux test builds) use OpenH264.

#[cfg(windows)]
mod mf;
#[cfg(not(windows))]
mod openh264;

use super::yuv::Frame;

pub trait Decode {
    /// Decodes one access unit (Annex B). `Ok(None)` while the decoder
    /// needs more input.
    fn decode(&mut self, annexb: &[u8]) -> Result<Option<Frame<'_>>, String>;
}

/// A decoder for this platform. Must be created and used on one thread.
pub fn decoder() -> Result<Box<dyn Decode>, String> {
    #[cfg(windows)]
    return mf::MediaFoundation::new().map(|d| Box::new(d) as Box<dyn Decode>);
    #[cfg(not(windows))]
    return openh264::OpenH264::new().map(|d| Box::new(d) as Box<dyn Decode>);
}

/// The NAL unit types in an Annex B access unit.
pub fn nal_types(annexb: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut i = 0;
    std::iter::from_fn(move || {
        while i + 3 < annexb.len() {
            let start = if annexb[i..].starts_with(&[0, 0, 1]) {
                Some(i + 3)
            } else if annexb[i..].starts_with(&[0, 0, 0, 1]) {
                Some(i + 4)
            } else {
                None
            };
            i += 1;
            if let Some(s) = start {
                i = s;
                if let Some(b) = annexb.get(s) {
                    return Some(b & 0x1F);
                }
            }
        }
        None
    })
}

/// Whether an access unit starts a new picture sequence (IDR, or the
/// SPS that precedes one), so decoding can begin there.
pub fn is_key(annexb: &[u8]) -> bool {
    nal_types(annexb).any(|t| t == 5 || t == 7)
}

/// Splits an Annex B stream into access units (pictures): a new one starts
/// at a parameter set, SEI or delimiter after a slice, or at a picture's
/// first slice (`first_mb_in_slice` 0, so the slice header's first bit set).
pub fn access_units(stream: &[u8]) -> Vec<Vec<u8>> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i..].starts_with(&[0, 0, 1]) {
            let s = if i > 0 && stream[i - 1] == 0 {
                i - 1
            } else {
                i
            };
            starts.push((s, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::new();
    let mut cur = Vec::new();
    let mut has_slice = false;
    for (n, &(s, body)) in starts.iter().enumerate() {
        let end = starts.get(n + 1).map_or(stream.len(), |next| next.0);
        let t = stream.get(body).map_or(0, |b| b & 0x1F);
        let slice = matches!(t, 1 | 5);
        let first_slice = slice && stream.get(body + 1).is_some_and(|b| b & 0x80 != 0);
        if has_slice && (first_slice || matches!(t, 6..=9)) {
            units.push(std::mem::take(&mut cur));
            has_slice = false;
        }
        cur.extend_from_slice(&stream[s..end]);
        has_slice |= slice;
    }
    if has_slice {
        units.push(cur);
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIP: &[u8] = include_bytes!("../../../tests/data/bars-160x120.h264");

    #[test]
    fn splits_and_classifies() {
        let units = access_units(CLIP);
        assert_eq!(units.len(), 5);
        assert!(is_key(&units[0]));
        // Two slices a picture.
        assert_eq!(nal_types(&units[0]).filter(|t| *t == 5).count(), 2);
        assert_eq!(nal_types(&units[1]).collect::<Vec<_>>(), [1, 1]);
        assert!(
            !units[1..].iter().all(|u| is_key(u)),
            "not every frame is a key frame"
        );
    }

    /// Decodes the SMPTE bars clip and checks the colours of the top bars:
    /// white, yellow, cyan, green, magenta, red, blue at 75 %.
    #[test]
    fn decodes_colour_bars() {
        let mut dec = decoder().expect("a decoder");
        let mut last = None;
        for unit in access_units(CLIP) {
            if let Some(f) = dec.decode(&unit).expect("decodes") {
                let mut px = vec![0; f.width * f.height * 4];
                f.convert([0, 0, f.width, f.height], &mut px, f.width * 4);
                last = Some((f.width, f.height, px));
            }
        }
        let (w, h, px) = last.expect("a picture");
        assert_eq!((w, h), (160, 120));
        let expected: [[u8; 3]; 7] = [
            [191, 191, 191],
            [0, 191, 191],
            [191, 191, 0],
            [0, 191, 0],
            [191, 0, 191],
            [0, 0, 191],
            [191, 0, 0],
        ];
        for (i, want) in expected.iter().enumerate() {
            let x = (i * 160 / 7) + 160 / 14;
            let o = (20 * w + x) * 4;
            let got = &px[o..o + 3];
            for c in 0..3 {
                assert!(
                    (got[c] as i32 - want[c] as i32).abs() <= 24,
                    "bar {i} at x={x}: got {got:?}, want {want:?} (BGR)"
                );
            }
            assert_eq!(px[o + 3], 255);
        }
    }
}
