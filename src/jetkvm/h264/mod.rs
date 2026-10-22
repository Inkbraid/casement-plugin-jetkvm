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

/// The picture size, after cropping, of the first sequence parameter set
/// in an Annex B access unit (ITU-T H.264 7.3.2.1.1): what a decoder needs
/// to know before the first picture.
#[cfg(any(windows, test))]
pub fn sps_size(annexb: &[u8]) -> Option<(u32, u32)> {
    let sps = nal_units(annexb).find(|n| n.first().is_some_and(|b| b & 0x1F == 7))?;
    let mut r = Bits::new(sps.get(1..)?);
    let profile = r.bits(8)?;
    r.bits(16)?; // constraint flags, level
    r.ue()?; // seq_parameter_set_id
    let mut chroma_format = 1;
    let mut separate_planes = false;
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format = r.ue()?;
        if chroma_format == 3 {
            separate_planes = r.bits(1)? == 1;
        }
        r.ue()?; // bit depths
        r.ue()?;
        r.bits(1)?; // qpprime_y_zero_transform_bypass_flag
        if r.bits(1)? == 1 {
            let lists = if chroma_format == 3 { 12 } else { 8 };
            for i in 0..lists {
                if r.bits(1)? == 1 {
                    r.scaling_list(if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    r.ue()?; // log2_max_frame_num_minus4
    match r.ue()? {
        0 => {
            r.ue()?;
        }
        1 => {
            r.bits(1)?;
            r.se()?;
            r.se()?;
            for _ in 0..r.ue()? {
                r.se()?;
            }
        }
        _ => {}
    }
    r.ue()?; // max_num_ref_frames
    r.bits(1)?; // gaps_in_frame_num_value_allowed_flag
    let width_mbs = r.ue()? + 1;
    let height_units = r.ue()? + 1;
    let frame_mbs_only = r.bits(1)?;
    if frame_mbs_only == 0 {
        r.bits(1)?; // mb_adaptive_frame_field_flag
    }
    r.bits(1)?; // direct_8x8_inference_flag
    let (mut width, mut height) = (width_mbs * 16, (2 - frame_mbs_only) * height_units * 16);
    if r.bits(1)? == 1 {
        let (left, right, top, bottom) = (r.ue()?, r.ue()?, r.ue()?, r.ue()?);
        // Cropping is counted in chroma samples (Table 6-1).
        let (unit_x, unit_y) = match chroma_format {
            _ if separate_planes => (1, 2 - frame_mbs_only),
            0 => (1, 2 - frame_mbs_only),
            1 => (2, 2 * (2 - frame_mbs_only)),
            2 => (2, 2 - frame_mbs_only),
            _ => (1, 2 - frame_mbs_only),
        };
        width = width.checked_sub(unit_x * (left + right))?;
        height = height.checked_sub(unit_y * (top + bottom))?;
    }
    (width > 0 && height > 0 && width <= 16384 && height <= 16384).then_some((width, height))
}

/// The NAL units of an Annex B access unit, without their start codes.
#[cfg(any(windows, test))]
fn nal_units(annexb: &[u8]) -> impl Iterator<Item = &[u8]> + '_ {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= annexb.len() {
        if annexb[i..].starts_with(&[0, 0, 1]) {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    (0..starts.len()).map(move |n| {
        let end = starts.get(n + 1).map_or(annexb.len(), |next| next - 3);
        let nal = &annexb[starts[n]..end];
        // The zeros before a four-byte start code belong to no unit.
        let trailing = nal.iter().rev().take_while(|b| **b == 0).count();
        &nal[..nal.len() - trailing]
    })
}

/// A bit reader over a NAL unit's payload, without its emulation
/// prevention bytes (`00 00 03`).
#[cfg(any(windows, test))]
struct Bits {
    data: Vec<u8>,
    pos: usize,
}

#[cfg(any(windows, test))]
impl Bits {
    fn new(payload: &[u8]) -> Self {
        let mut data = Vec::with_capacity(payload.len());
        let mut zeros = 0;
        for &b in payload {
            if zeros >= 2 && b == 3 {
                zeros = 0;
                continue;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            data.push(b);
        }
        Bits { data, pos: 0 }
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            let byte = self.data.get(self.pos / 8)?;
            v = (v << 1) | u32::from(byte >> (7 - self.pos % 8) & 1);
            self.pos += 1;
        }
        Some(v)
    }

    /// An unsigned Exp-Golomb number, `ue(v)`.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bits(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }

    /// A signed Exp-Golomb number, `se(v)`.
    fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        let magnitude = k.div_ceil(2) as i32;
        Some(if k % 2 == 1 { magnitude } else { -magnitude })
    }

    /// Skips a scaling list of `size` entries (7.3.2.1.1.1).
    fn scaling_list(&mut self, size: usize) -> Option<()> {
        let (mut last, mut next) = (8i32, 8i32);
        for _ in 0..size {
            if next != 0 {
                next = (last + self.se()? + 256) % 256;
            }
            if next != 0 {
                last = next;
            }
        }
        Some(())
    }
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

    /// Sizes from parameter sets: the test clip (cropped from 160x128),
    /// and x264's High 1080p, Main 1366x768 and High 4:4:4 720p.
    #[test]
    fn reads_the_picture_size_from_the_sps() {
        assert_eq!(sps_size(&access_units(CLIP)[0]), Some((160, 120)));
        assert_eq!(sps_size(&access_units(CLIP)[1]), None);
        let sps = |hex: &str| {
            let mut unit = vec![0, 0, 0, 1];
            unit.extend(
                (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()),
            );
            unit
        };
        let high = sps("67640028acd940780227e5c044000003000400000300f03c60c658");
        let main = sps("674d4020eca02b030f378088000003000800000301e078c18cb0");
        let high444 = sps("67f4001f919b280a00b76022000003000200000300781e30632c");
        assert_eq!(sps_size(&high), Some((1920, 1080)));
        assert_eq!(sps_size(&main), Some((1366, 768)));
        assert_eq!(sps_size(&high444), Some((1280, 720)));
        // Cut short, it says nothing.
        assert_eq!(sps_size(&high[..10]), None);
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
