//! Decoded pictures (YUV 4:2:0) and their conversion to BGRX, BT.601
//! limited range (what H.264 encoders such as the JetKVM's produce unless
//! they signal otherwise).
//!
//! Pictures are compared before they are converted, so for a still screen
//! only the tiles that changed are converted at all. Each chroma sample
//! covers 2×2 pixels: its terms are worked out once and applied to both
//! rows, in plain integer arithmetic.

/// A decoded 4:2:0 picture: luma, and chroma at half resolution both ways.
#[derive(Clone, Copy)]
pub struct Frame<'a> {
    pub width: usize,
    pub height: usize,
    pub y: &'a [u8],
    pub y_stride: usize,
    pub chroma: Chroma<'a>,
}

#[derive(Clone, Copy)]
pub enum Chroma<'a> {
    /// I420: separate U and V planes.
    Planar {
        u: &'a [u8],
        v: &'a [u8],
        u_stride: usize,
        v_stride: usize,
    },
    /// NV12: one plane of U, V pairs.
    Interleaved { uv: &'a [u8], stride: usize },
}

/// A rectangle, `[x, y, width, height]`, with `x` and `y` even.
pub type Rect = [usize; 4];

/// The chroma terms of one sample: what it adds to red, green and blue.
#[inline(always)]
fn chroma(u: u8, v: u8) -> (i32, i32, i32) {
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    (409 * e + 128, -100 * d - 208 * e + 128, 516 * d + 128)
}

/// A BGRX pixel, as bytes.
#[inline(always)]
fn pack(y: u8, (r, g, b): (i32, i32, i32)) -> [u8; 4] {
    let c = (y as i32 - 16).max(0) * 298;
    let r = ((c + r) >> 8).clamp(0, 255) as u32;
    let g = ((c + g) >> 8).clamp(0, 255) as u32;
    let b = ((c + b) >> 8).clamp(0, 255) as u32;
    (0xFF00_0000 | r << 16 | g << 8 | b).to_le_bytes()
}

#[cfg(test)]
fn pixel(y: u8, u: u8, v: u8) -> [u8; 4] {
    pack(y, chroma(u, v))
}

/// Converts a row, or two rows that share chroma, into `width * 4` bytes.
#[inline(always)]
fn rows(
    y0: &[u8],
    y1: Option<&[u8]>,
    mut uv: impl Iterator<Item = (u8, u8)>,
    o0: &mut [u8],
    o1: Option<&mut [u8]>,
) {
    let odd = y0.len() % 2 == 1;
    match (y1, o1) {
        (Some(y1), Some(o1)) => {
            let pairs = y0
                .chunks_exact(2)
                .zip(y1.chunks_exact(2))
                .zip(o0.chunks_exact_mut(8).zip(o1.chunks_exact_mut(8)));
            for (((a, b), (oa, ob)), (u, v)) in pairs.zip(&mut uv) {
                let c = chroma(u, v);
                oa[..4].copy_from_slice(&pack(a[0], c));
                oa[4..].copy_from_slice(&pack(a[1], c));
                ob[..4].copy_from_slice(&pack(b[0], c));
                ob[4..].copy_from_slice(&pack(b[1], c));
            }
            if let (true, Some((u, v))) = (odd, uv.next()) {
                let (c, x) = (chroma(u, v), y0.len() - 1);
                o0[x * 4..].copy_from_slice(&pack(y0[x], c));
                o1[x * 4..].copy_from_slice(&pack(y1[x], c));
            }
        }
        _ => {
            for ((a, oa), (u, v)) in y0.chunks_exact(2).zip(o0.chunks_exact_mut(8)).zip(&mut uv) {
                let c = chroma(u, v);
                oa[..4].copy_from_slice(&pack(a[0], c));
                oa[4..].copy_from_slice(&pack(a[1], c));
            }
            if let (true, Some((u, v))) = (odd, uv.next()) {
                let x = y0.len() - 1;
                o0[x * 4..].copy_from_slice(&pack(y0[x], chroma(u, v)));
            }
        }
    }
}

/// The byte range of `len` items from `x` in row `row` of a plane.
fn span(row: usize, stride: usize, x: usize, len: usize) -> std::ops::Range<usize> {
    row * stride + x..row * stride + x + len
}

impl Frame<'_> {
    /// Converts `rect` into BGRX at the same place in `out`, `out_stride`
    /// bytes per row.
    pub fn convert(&self, [x, y, w, h]: Rect, out: &mut [u8], out_stride: usize) {
        let cx = x / 2;
        let cw = w.div_ceil(2);
        for row in (y..y + h).step_by(2) {
            let pair = row + 1 < y + h;
            let y0 = &self.y[span(row, self.y_stride, x, w)];
            let y1 = pair.then(|| &self.y[span(row + 1, self.y_stride, x, w)]);
            let left = out.len() - row * out_stride;
            let (o0, rest) = out[row * out_stride..].split_at_mut(out_stride.min(left));
            let o0 = &mut o0[x * 4..(x + w) * 4];
            let o1 = pair.then(|| &mut rest[x * 4..(x + w) * 4]);
            let crow = row / 2;
            match self.chroma {
                Chroma::Planar {
                    u,
                    v,
                    u_stride,
                    v_stride,
                } => {
                    let u = &u[span(crow, u_stride, cx, cw)];
                    let v = &v[span(crow, v_stride, cx, cw)];
                    rows(y0, y1, u.iter().copied().zip(v.iter().copied()), o0, o1);
                }
                Chroma::Interleaved { uv, stride } => {
                    let c = &uv[span(crow, stride, cx * 2, cw * 2)];
                    rows(y0, y1, c.chunks_exact(2).map(|p| (p[0], p[1])), o0, o1);
                }
            }
        }
    }
}

/// An owned, compact copy of a picture, to find what changes in the next.
pub struct Planes {
    width: usize,
    height: usize,
    interleaved: bool,
    y: Vec<u8>,
    /// U then V (planar), or the U, V pairs (interleaved), `width / 2`
    /// samples a row, rounded up.
    c: Vec<u8>,
}

impl Planes {
    pub fn new(f: &Frame) -> Planes {
        let mut p = Planes {
            width: f.width,
            height: f.height,
            interleaved: matches!(f.chroma, Chroma::Interleaved { .. }),
            y: vec![0; f.width * f.height],
            c: vec![0; f.width.div_ceil(2) * f.height.div_ceil(2) * 2],
        };
        let mut all = vec![false; 1];
        p.compare_band(f, 0, f.height, f.width.max(1), &mut all);
        p
    }

    /// Whether `f` has this picture's size and layout.
    pub fn matches(&self, f: &Frame) -> bool {
        (self.width, self.height) == (f.width, f.height)
            && self.interleaved == matches!(f.chroma, Chroma::Interleaved { .. })
    }

    /// Compares rows `y..y + h` of `f` (`y` even) with this picture, marks
    /// in `changed` the tiles of that band, `tile` pixels wide (even), that
    /// differ, and takes the rows in.
    pub fn compare_band(
        &mut self,
        f: &Frame,
        y: usize,
        h: usize,
        tile: usize,
        changed: &mut [bool],
    ) {
        let w = self.width;
        // Whole rows first: on a still screen most are the same.
        let mut row = |new: &[u8], mine: &mut [u8], unit: usize| {
            if new == &mine[..] {
                return;
            }
            for (i, c) in changed.iter_mut().enumerate() {
                let span = (i * unit).min(new.len())..((i + 1) * unit).min(new.len());
                if !*c && new[span.clone()] != mine[span] {
                    *c = true;
                }
            }
            mine.copy_from_slice(new);
        };
        for r in y..y + h {
            row(
                &f.y[span(r, f.y_stride, 0, w)],
                &mut self.y[span(r, w, 0, w)],
                tile,
            );
        }
        let ccols = w.div_ceil(2);
        let crows = self.height.div_ceil(2);
        for r in y / 2..(y + h).div_ceil(2) {
            match f.chroma {
                Chroma::Planar {
                    u,
                    v,
                    u_stride,
                    v_stride,
                } => {
                    let (mine_u, mine_v) = self.c.split_at_mut(ccols * crows);
                    row(
                        &u[span(r, u_stride, 0, ccols)],
                        &mut mine_u[span(r, ccols, 0, ccols)],
                        tile / 2,
                    );
                    row(
                        &v[span(r, v_stride, 0, ccols)],
                        &mut mine_v[span(r, ccols, 0, ccols)],
                        tile / 2,
                    );
                }
                Chroma::Interleaved { uv, stride } => {
                    let n = ccols * 2;
                    row(
                        &uv[span(r, stride, 0, n)],
                        &mut self.c[span(r, n, 0, n)],
                        tile,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_colours() {
        // Limited-range BT.601 values for black, white and 75 % red.
        assert_eq!(pixel(16, 128, 128), [0, 0, 0, 255]);
        assert_eq!(pixel(235, 128, 128), [255, 255, 255, 255]);
        let red = pixel(65, 100, 212);
        assert!(red[2] > 180 && red[1] < 20 && red[0] < 20, "{red:?}");
    }

    /// A 5x3 picture with padded strides: Y (stride 6), U and V (stride 4)
    /// and the same chroma interleaved (stride 8).
    fn planes() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let y = (0..6 * 3).map(|i| 30 + i as u8 * 10).collect();
        let u = vec![80u8, 170, 120, 0, 120, 140, 60, 0];
        let v = vec![190u8, 70, 100, 0, 110, 90, 220, 0];
        let uv = u.iter().zip(&v).flat_map(|(a, b)| [*a, *b]).collect();
        (y, u, v, uv)
    }

    fn planar<'a>(y: &'a [u8], u: &'a [u8], v: &'a [u8]) -> Frame<'a> {
        Frame {
            width: 5,
            height: 3,
            y,
            y_stride: 6,
            chroma: Chroma::Planar {
                u,
                v,
                u_stride: 4,
                v_stride: 4,
            },
        }
    }

    #[test]
    fn layouts_agree_with_padding_and_odd_sizes() {
        let (y, u, v, uv) = planes();
        let i420 = planar(&y, &u, &v);
        let nv12 = Frame {
            chroma: Chroma::Interleaved { uv: &uv, stride: 8 },
            ..i420
        };
        let mut a = vec![0u8; 7 * 4 * 3]; // stride 28
        let mut b = a.clone();
        i420.convert([0, 0, 5, 3], &mut a, 28);
        nv12.convert([0, 0, 5, 3], &mut b, 28);
        assert_eq!(a, b);
        for row in 0..3 {
            for col in 0..5 {
                let c = (row / 2) * 4 + col / 2;
                let o = row * 28 + col * 4;
                let want = pixel(y[row * 6 + col], u[c], v[c]);
                assert_eq!(&a[o..o + 4], &want, "({col}, {row})");
            }
            assert!(
                a[row * 28 + 20..row * 28 + 28].iter().all(|b| *b == 0),
                "padding"
            );
        }

        // A part converts the same as the whole, and nothing else.
        let mut part = vec![0u8; a.len()];
        i420.convert([2, 2, 3, 1], &mut part, 28);
        assert_eq!(&part[2 * 28 + 8..2 * 28 + 20], &a[2 * 28 + 8..2 * 28 + 20]);
        assert!(part[..2 * 28 + 8].iter().all(|b| *b == 0));
    }

    #[test]
    fn finds_changes() {
        let (mut y, u, v, uv) = planes();
        let before = y.clone();
        let mut p = Planes::new(&planar(&before, &u, &v));
        let mut changed = [false; 3]; // tiles 2 pixels wide
        p.compare_band(&planar(&before, &u, &v), 0, 3, 2, &mut changed);
        assert_eq!(changed, [false; 3]);

        y[2 * 6 + 4] ^= 1; // the bottom right pixel
        let after = planar(&y, &u, &v);
        assert!(p.matches(&after));
        p.compare_band(&after, 0, 2, 2, &mut changed);
        assert_eq!(changed, [false; 3], "not in rows 0 and 1");
        p.compare_band(&after, 2, 1, 2, &mut changed);
        assert_eq!(changed, [false, false, true]);
        // Taken in, so no longer a change.
        let mut again = [false; 3];
        p.compare_band(&after, 0, 3, 2, &mut again);
        assert_eq!(again, [false; 3]);

        // Chroma counts too: V sample 1 covers columns 2 and 3 of rows 0, 1.
        let mut v2 = v.clone();
        v2[1] ^= 1;
        let mut changed = [false; 3];
        p.compare_band(&planar(&y, &u, &v2), 0, 2, 2, &mut changed);
        assert_eq!(changed, [false, true, false]);

        let nv12 = Frame {
            chroma: Chroma::Interleaved { uv: &uv, stride: 8 },
            ..after
        };
        assert!(!p.matches(&nv12));
        let mut q = Planes::new(&nv12);
        let mut uv2 = uv.clone();
        uv2[8 + 4] ^= 1; // U of sample 2 in chroma row 1: column 4, row 2
        let mut changed = [false; 3];
        q.compare_band(
            &Frame {
                chroma: Chroma::Interleaved {
                    uv: &uv2,
                    stride: 8,
                },
                ..nv12
            },
            2,
            1,
            2,
            &mut changed,
        );
        assert_eq!(changed, [false, false, true]);
    }
}
