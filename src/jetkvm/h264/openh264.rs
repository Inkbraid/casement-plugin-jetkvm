//! OpenH264, for the test builds on platforms other than Windows.

use super::Decode;
use crate::jetkvm::yuv::{Chroma, Frame};
use openh264::formats::YUVSource;

pub struct OpenH264 {
    dec: openh264::decoder::Decoder,
    /// The last picture's planes, as the decoder laid them out.
    planes: [Vec<u8>; 3],
    strides: (usize, usize, usize),
    size: (usize, usize),
}

impl OpenH264 {
    pub fn new() -> Result<Self, String> {
        Ok(OpenH264 {
            dec: openh264::decoder::Decoder::new().map_err(|e| e.to_string())?,
            planes: Default::default(),
            strides: (0, 0, 0),
            size: (0, 0),
        })
    }
}

impl Decode for OpenH264 {
    fn decode(&mut self, annexb: &[u8]) -> Result<Option<Frame<'_>>, String> {
        let Some(yuv) = self.dec.decode(annexb).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        // The wrapper only lends the planes for as long as it lives.
        for (plane, data) in self.planes.iter_mut().zip([yuv.y(), yuv.u(), yuv.v()]) {
            plane.clear();
            plane.extend_from_slice(data);
        }
        self.strides = yuv.strides();
        self.size = yuv.dimensions();
        let [y, u, v] = &self.planes;
        Ok(Some(Frame {
            width: self.size.0,
            height: self.size.1,
            y,
            y_stride: self.strides.0,
            chroma: Chroma::Planar {
                u,
                v,
                u_stride: self.strides.1,
                v_stride: self.strides.2,
            },
        }))
    }
}
