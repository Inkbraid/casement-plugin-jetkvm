//! OpenH264, for the test builds on platforms other than Windows.

use super::{Decode, Picture};
use crate::jetkvm::yuv;
use openh264::formats::YUVSource;

pub struct OpenH264 {
    dec: openh264::decoder::Decoder,
    buf: Vec<u8>,
}

impl OpenH264 {
    pub fn new() -> Result<Self, String> {
        Ok(OpenH264 {
            dec: openh264::decoder::Decoder::new().map_err(|e| e.to_string())?,
            buf: Vec::new(),
        })
    }
}

impl Decode for OpenH264 {
    fn decode(&mut self, annexb: &[u8]) -> Result<Option<Picture<'_>>, String> {
        let Some(yuv) = self.dec.decode(annexb).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        self.buf.resize(w * h * 4, 0);
        yuv::i420_to_bgrx(
            yuv.y(),
            yuv.u(),
            yuv.v(),
            yuv.strides(),
            w,
            h,
            &mut self.buf,
            w * 4,
        );
        Ok(Some(Picture {
            width: w as u32,
            height: h as u32,
            bgrx: &self.buf,
        }))
    }
}
