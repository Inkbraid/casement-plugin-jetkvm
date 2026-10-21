//! YUV 4:2:0 to BGRX, BT.601 limited range (what H.264 encoders such as the
//! JetKVM's produce unless they signal otherwise).

#[inline]
fn pixel(y: u8, u: u8, v: u8) -> [u8; 4] {
    let c = (y as i32 - 16).max(0) * 298;
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let clip = |x: i32| (x >> 8).clamp(0, 255) as u8;
    let r = clip(c + 409 * e + 128);
    let g = clip(c - 100 * d - 208 * e + 128);
    let b = clip(c + 516 * d + 128);
    [b, g, r, 0xFF]
}

/// Planar I420 (Y, U, V planes) to BGRX, `out_stride` bytes per output row.
#[allow(clippy::too_many_arguments)]
pub fn i420_to_bgrx(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    (y_stride, u_stride, v_stride): (usize, usize, usize),
    width: usize,
    height: usize,
    out: &mut [u8],
    out_stride: usize,
) {
    for row in 0..height {
        let yr = &y[row * y_stride..row * y_stride + width];
        let ur = &u[(row / 2) * u_stride..];
        let vr = &v[(row / 2) * v_stride..];
        let o = &mut out[row * out_stride..row * out_stride + width * 4];
        for (col, px) in o.chunks_exact_mut(4).enumerate() {
            px.copy_from_slice(&pixel(yr[col], ur[col / 2], vr[col / 2]));
        }
    }
}

/// Semi-planar NV12 (Y plane, interleaved UV plane) to BGRX.
#[allow(clippy::too_many_arguments)]
pub fn nv12_to_bgrx(
    y: &[u8],
    uv: &[u8],
    y_stride: usize,
    uv_stride: usize,
    width: usize,
    height: usize,
    out: &mut [u8],
    out_stride: usize,
) {
    for row in 0..height {
        let yr = &y[row * y_stride..row * y_stride + width];
        let uvr = &uv[(row / 2) * uv_stride..];
        let o = &mut out[row * out_stride..row * out_stride + width * 4];
        for (col, px) in o.chunks_exact_mut(4).enumerate() {
            let c = (col / 2) * 2;
            px.copy_from_slice(&pixel(yr[col], uvr[c], uvr[c + 1]));
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

    #[test]
    fn layouts_agree() {
        // A 4x2 picture with distinct chroma per 2x2 block.
        let y = [100u8, 110, 120, 130, 140, 150, 160, 170];
        let (u, v) = ([90u8, 160], [200u8, 60]);
        let uv = [90u8, 200, 160, 60];
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        i420_to_bgrx(&y, &u, &v, (4, 2, 2), 4, 2, &mut a, 16);
        nv12_to_bgrx(&y, &uv, 4, 4, 4, 2, &mut b, 16);
        assert_eq!(a, b);
        assert_eq!(&a[0..4], &pixel(100, 90, 200));
        assert_eq!(&a[12..16], &pixel(130, 160, 60));
    }
}
