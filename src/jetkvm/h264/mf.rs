//! Windows' own H.264 decoder: Media Foundation's H.264 decoder transform
//! (`CLSID_MSH264DecoderMFT`), in software mode with NV12 output.
//!
//! Windows N editions (and Windows Server) leave Media Foundation out until
//! it is installed, so its library is loaded when the first picture comes
//! rather than linked: without it the plugin still starts and says what is
//! missing.

use super::Decode;
use crate::jetkvm::yuv::{Chroma, Frame};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use windows::core::{s, w, Interface, GUID, HRESULT, PCSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};

const MISSING: &str = "Windows' media components (Media Foundation) aren't installed, so the JetKVM's picture can't be shown. On Windows N editions, install the Media Feature Pack.";

/// The functions of `mfplat.dll` the decoder uses.
struct Mfplat {
    startup: unsafe extern "system" fn(u32, u32) -> HRESULT,
    shutdown: unsafe extern "system" fn() -> HRESULT,
    create_media_type: unsafe extern "system" fn(*mut *mut c_void) -> HRESULT,
    create_sample: unsafe extern "system" fn(*mut *mut c_void) -> HRESULT,
    create_aligned_memory_buffer: unsafe extern "system" fn(u32, u32, *mut *mut c_void) -> HRESULT,
}

/// A function of `module` as the pointer type `T`.
unsafe fn function<T>(module: HMODULE, name: PCSTR) -> Option<T> {
    let f = unsafe { GetProcAddress(module, name) }?;
    // Function pointers all have the same size.
    Some(unsafe { std::mem::transmute_copy(&f) })
}

impl Mfplat {
    /// Loads `mfplat.dll` from System32, once.
    fn get() -> Result<&'static Mfplat, String> {
        static MFPLAT: OnceLock<Option<Mfplat>> = OnceLock::new();
        MFPLAT
            .get_or_init(|| unsafe {
                let m =
                    LoadLibraryExW(w!("mfplat.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
                Some(Mfplat {
                    startup: function(m, s!("MFStartup"))?,
                    shutdown: function(m, s!("MFShutdown"))?,
                    create_media_type: function(m, s!("MFCreateMediaType"))?,
                    create_sample: function(m, s!("MFCreateSample"))?,
                    create_aligned_memory_buffer: function(m, s!("MFCreateAlignedMemoryBuffer"))?,
                })
            })
            .as_ref()
            .ok_or_else(|| MISSING.to_string())
    }

    fn media_type(&self) -> windows::core::Result<IMFMediaType> {
        let mut p = std::ptr::null_mut();
        unsafe {
            (self.create_media_type)(&mut p).ok()?;
            Ok(IMFMediaType::from_raw(p))
        }
    }

    fn sample(&self) -> windows::core::Result<IMFSample> {
        let mut p = std::ptr::null_mut();
        unsafe {
            (self.create_sample)(&mut p).ok()?;
            Ok(IMFSample::from_raw(p))
        }
    }

    /// A buffer of `size` bytes whose start is a multiple of `alignment`, a
    /// power of two.
    fn memory_buffer(&self, size: u32, alignment: u32) -> windows::core::Result<IMFMediaBuffer> {
        let mut p = std::ptr::null_mut();
        unsafe {
            // The function takes the alignment minus one (MF_16_BYTE_ALIGNMENT is 15).
            (self.create_aligned_memory_buffer)(size, alignment - 1, &mut p).ok()?;
            Ok(IMFMediaBuffer::from_raw(p))
        }
    }
}

/// 100 ns units per frame for the input samples' times; the decoder only
/// needs them to increase.
const FRAME_TIME: i64 = 333_333;

/// The output format, as the decoder last reported it.
#[derive(Default)]
struct Format {
    /// The coded frame's height: the buffers hold this many rows of luma,
    /// often more than the picture (1088 for 1080).
    frame_height: u32,
    stride: u32,
    /// The visible picture within the frame.
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    provides_samples: bool,
    /// Our output buffer's size and alignment, when we provide the samples.
    buffer_size: u32,
    alignment: u32,
}

/// Media Foundation, started. Shuts it down when dropped, which is after
/// the decoder that uses it (fields drop in order).
struct Started(&'static Mfplat);

impl Drop for Started {
    fn drop(&mut self) {
        let _ = unsafe { (self.0.shutdown)() };
    }
}

pub struct MediaFoundation {
    mf: &'static Mfplat,
    mft: IMFTransform,
    format: Format,
    /// Pictures and format changes so far, for error messages.
    pictures: u32,
    format_changes: u32,
    /// Our output sample and its buffer, when the decoder doesn't allocate
    /// its own.
    out: Option<(IMFSample, IMFMediaBuffer)>,
    time: i64,
    /// The last picture: luma, and the U, V pairs.
    y: Vec<u8>,
    uv: Vec<u8>,
    _started: Started,
}

fn err(what: &str, e: windows::core::Error) -> String {
    format!("{what}: {e}")
}

impl MediaFoundation {
    pub fn new() -> Result<Self, String> {
        let mf = Mfplat::get()?;
        let (mft, started) = unsafe {
            // Any apartment will do; this thread only decodes.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            (mf.startup)(MF_VERSION, MFSTARTUP_NOSOCKET)
                .ok()
                .map_err(|e| err("Windows' media framework didn't start", e))?;
            let started = Started(mf);
            let mft: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER).map_err(
                    |e| {
                        log::error!("H.264 decoder: {e}");
                        MISSING.to_string()
                    },
                )?;
            (mft, started)
        };
        let mut dec = MediaFoundation {
            mf,
            mft,
            format: Format::default(),
            pictures: 0,
            format_changes: 0,
            out: None,
            time: 0,
            y: Vec::new(),
            uv: Vec::new(),
            _started: started,
        };
        dec.configure()?;
        Ok(dec)
    }

    fn configure(&mut self) -> Result<(), String> {
        unsafe {
            // Output each picture as soon as it is decoded.
            if let Ok(a) = self.mft.GetAttributes() {
                let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let input = self
                .mf
                .media_type()
                .map_err(|e| err("MFCreateMediaType", e))?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .and_then(|_| input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264))
                .and_then(|_| {
                    input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                })
                .map_err(|e| err("Setting up the H.264 input", e))?;
            self.mft
                .SetInputType(0, &input, 0)
                .map_err(|e| err("The H.264 decoder refused its input type", e))?;
        }
        self.set_output_type()?;
        unsafe {
            let _ = self
                .mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = self
                .mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }
        Ok(())
    }

    /// Picks NV12 output and reads the format that comes with it.
    fn set_output_type(&mut self) -> Result<(), String> {
        unsafe {
            let mut i = 0;
            loop {
                let t = self
                    .mft
                    .GetOutputAvailableType(0, i)
                    .map_err(|e| err("The H.264 decoder offers no NV12 output", e))?;
                if t.GetGUID(&MF_MT_SUBTYPE).ok() == Some(MFVideoFormat_NV12) {
                    self.mft
                        .SetOutputType(0, &t, 0)
                        .map_err(|e| err("The H.264 decoder refused NV12 output", e))?;
                    break;
                }
                i += 1;
            }
            let t = self
                .mft
                .GetOutputCurrentType(0)
                .map_err(|e| err("GetOutputCurrentType", e))?;
            let size = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            let (frame_width, frame_height) = ((size >> 32) as u32, size as u32);
            let mut f = Format {
                frame_height,
                stride: t
                    .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                    .ok()
                    .map(|s| (s as i32).unsigned_abs())
                    .filter(|s| *s >= frame_width)
                    .unwrap_or(frame_width),
                width: frame_width,
                height: frame_height,
                ..Format::default()
            };
            for key in [&MF_MT_MINIMUM_DISPLAY_APERTURE, &MF_MT_GEOMETRIC_APERTURE] {
                if let Some((x, y, w, h)) = aperture(&t, key) {
                    if x + w <= frame_width && y + h <= frame_height {
                        (f.x, f.y, f.width, f.height) = (x, y, w, h);
                        break;
                    }
                }
            }
            let info = self
                .mft
                .GetOutputStreamInfo(0)
                .map_err(|e| err("GetOutputStreamInfo", e))?;
            let provides = (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
            f.provides_samples = info.dwFlags & provides != 0;
            self.out = None;
            if !f.provides_samples {
                let size = info.cbSize.max(f.stride * frame_height * 3 / 2);
                // The decoder copies its pictures with the alignment it asks
                // for in cbAlignment (in bytes; 0 when it has no need).
                let alignment = info.cbAlignment.max(16).next_power_of_two();
                (f.buffer_size, f.alignment) = (size, alignment);
                let sample = self.mf.sample().map_err(|e| err("MFCreateSample", e))?;
                let buffer = self
                    .mf
                    .memory_buffer(size, alignment)
                    .map_err(|e| err("MFCreateAlignedMemoryBuffer", e))?;
                sample
                    .AddBuffer(&buffer)
                    .map_err(|e| err("IMFSample::AddBuffer", e))?;
                self.out = Some((sample, buffer));
            }
            log::debug!(
                "H.264 decoder output: {frame_width}x{frame_height} frame, picture {}x{} at {},{}, stride {}",
                f.width,
                f.height,
                f.x,
                f.y,
                f.stride
            );
            self.format = f;
        }
        Ok(())
    }

    fn input_sample(&mut self, data: &[u8]) -> Result<IMFSample, String> {
        unsafe {
            let buffer = self
                .mf
                .memory_buffer(data.len() as u32, 1)
                .map_err(|e| err("MFCreateAlignedMemoryBuffer", e))?;
            let mut ptr = std::ptr::null_mut();
            buffer
                .Lock(&mut ptr, None, None)
                .map_err(|e| err("IMFMediaBuffer::Lock", e))?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            let _ = buffer.Unlock();
            buffer
                .SetCurrentLength(data.len() as u32)
                .map_err(|e| err("IMFMediaBuffer::SetCurrentLength", e))?;
            let sample = self.mf.sample().map_err(|e| err("MFCreateSample", e))?;
            sample
                .AddBuffer(&buffer)
                .map_err(|e| err("IMFSample::AddBuffer", e))?;
            let _ = sample.SetSampleTime(self.time);
            let _ = sample.SetSampleDuration(FRAME_TIME);
            self.time += FRAME_TIME;
            Ok(sample)
        }
    }

    /// Takes every picture the decoder has ready, keeping the last.
    /// Returns whether there was one.
    fn drain(&mut self) -> Result<bool, String> {
        let mut got = false;
        // The decoder says when it needs more input; the bound only guards
        // against one that never does.
        for _ in 0..64 {
            let ours = match &self.out {
                Some((sample, buffer)) if !self.format.provides_samples => {
                    // An empty buffer for every picture: with the last
                    // picture's length still set, the decoder found no room
                    // for the next one ("CopyDecodedFrame failed").
                    unsafe { buffer.SetCurrentLength(0) }
                        .map_err(|e| err("IMFMediaBuffer::SetCurrentLength", e))?;
                    Some(sample.clone())
                }
                _ => None,
            };
            let (result, sample) = unsafe {
                let mut out = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(ours),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0;
                let result = self.mft.ProcessOutput(0, &mut out, &mut status);
                drop(ManuallyDrop::take(&mut out[0].pEvents));
                (result, ManuallyDrop::take(&mut out[0].pSample))
            };
            match result {
                Ok(()) => {
                    if let Some(s) = sample {
                        self.take(&s)?;
                        got = true;
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(got),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.format_changes += 1;
                    self.set_output_type()?
                }
                Err(e) => {
                    return Err(format!(
                        "{} ({})",
                        err("The H.264 decoder failed", e),
                        self.describe_output()
                    ))
                }
            }
        }
        Ok(got)
    }

    /// The output format and buffer, and what the decoder asks for, for
    /// error messages.
    fn describe_output(&self) -> String {
        let f = &self.format;
        let mut text = format!(
            "after {} pictures and {} format changes; output: {} rows of {} bytes, picture {}x{} at {},{}",
            self.pictures, self.format_changes, f.frame_height, f.stride, f.width, f.height, f.x, f.y
        );
        if !f.provides_samples {
            text += &format!(
                "; our buffer {} bytes aligned to {}",
                f.buffer_size, f.alignment
            );
        }
        if let Ok(info) = unsafe { self.mft.GetOutputStreamInfo(0) } {
            text += &format!(
                "; the decoder asks for {} bytes aligned to {}, flags {:#x}",
                info.cbSize, info.cbAlignment, info.dwFlags
            );
        }
        text
    }

    /// Copies the visible part of the NV12 picture in `sample` into `self.y`
    /// and `self.uv`, without padding.
    fn take(&mut self, sample: &IMFSample) -> Result<(), String> {
        let f = &self.format;
        let stride = f.stride as usize;
        let frame_h = f.frame_height as usize;
        let need = stride * (frame_h + frame_h.div_ceil(2));
        if f.width == 0 || f.height == 0 {
            return Err("The H.264 decoder gave a picture before its size.".into());
        }
        unsafe {
            let buffer = sample
                .ConvertToContiguousBuffer()
                .map_err(|e| err("ConvertToContiguousBuffer", e))?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer
                .Lock(&mut ptr, None, Some(&mut len))
                .map_err(|e| err("IMFMediaBuffer::Lock", e))?;
            if ptr.is_null() || (len as usize) < need {
                let _ = buffer.Unlock();
                return Err(format!(
                    "The H.264 decoder's picture is {len} bytes, expected {need}."
                ));
            }
            let data = std::slice::from_raw_parts(ptr, need);
            let (w, h) = (f.width as usize, f.height as usize);
            let (x, y) = (f.x as usize, f.y as usize);
            self.y.clear();
            for row in y..y + h {
                self.y
                    .extend_from_slice(&data[row * stride + x..row * stride + x + w]);
            }
            // Chroma: half the rows, and U, V pairs for every two columns.
            let chroma = stride * frame_h;
            let cw = w.div_ceil(2) * 2;
            self.uv.clear();
            for row in y / 2..y / 2 + h.div_ceil(2) {
                let o = chroma + row * stride + x / 2 * 2;
                self.uv.extend_from_slice(&data[o..o + cw]);
            }
            let _ = buffer.Unlock();
        }
        self.pictures += 1;
        Ok(())
    }
}

/// An aperture attribute (an `MFVideoArea`): x, y, width, height.
fn aperture(t: &IMFMediaType, key: &GUID) -> Option<(u32, u32, u32, u32)> {
    let mut blob = [0u8; 16];
    let mut size = 0u32;
    unsafe { t.GetBlob(key, &mut blob, Some(&mut size)) }.ok()?;
    if size != 16 {
        return None;
    }
    let i16_at = |o: usize| i16::from_le_bytes([blob[o], blob[o + 1]]);
    let i32_at = |o: usize| i32::from_le_bytes([blob[o], blob[o + 1], blob[o + 2], blob[o + 3]]);
    // MFOffset is {fract: u16, value: i16}; then SIZE {cx, cy}.
    let (x, y, w, h) = (i16_at(2), i16_at(6), i32_at(8), i32_at(12));
    if x < 0 || y < 0 || w <= 0 || h <= 0 {
        return None;
    }
    Some((x as u32, y as u32, w as u32, h as u32))
}

impl Decode for MediaFoundation {
    fn decode(&mut self, annexb: &[u8]) -> Result<Option<Frame<'_>>, String> {
        let sample = self.input_sample(annexb)?;
        let mut got = false;
        loop {
            match unsafe { self.mft.ProcessInput(0, &sample, 0) } {
                Ok(()) => break,
                // Full: take what is ready, then try again.
                Err(e) if e.code() == MF_E_NOTACCEPTING => got |= self.drain()?,
                Err(e) => return Err(err("The H.264 decoder refused a frame", e)),
            }
        }
        got |= self.drain()?;
        let width = self.format.width as usize;
        Ok(got.then(|| Frame {
            width,
            height: self.format.height as usize,
            y: &self.y,
            y_stride: width,
            chroma: Chroma::Interleaved {
                uv: &self.uv,
                stride: width.div_ceil(2) * 2,
            },
        }))
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        let _ = unsafe { self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0) };
    }
}
