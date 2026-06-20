//! CoreAudio microphone capture for macOS (Phase 2).
//!
//! Produces the same 16 kHz mono s16le [`AudioEvent::Frame`] stream as the
//! Linux `parec` path, feeding `sunoto-core` preroll unchanged. Captures from
//! the default input device (or the configured CoreAudio device id), converts
//! the device's native format to 16 kHz mono s16le with an `AudioConverter`,
//! and emits frame-sized chunks over the channel.
//!
//! Requires the Microphone TCC permission (prompted on first capture). On
//! denial or no input device, reports `AudioError::NoMicrophone`.
//!
//! Uses the current IOProcID-based CoreAudio API
//! (`AudioDeviceCreateIOProcID` / `AudioDeviceStart` / `AudioDeviceStop` /
//! `AudioDeviceDestroyIOProcID`).

#![cfg(target_os = "macos")]
#![allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code
)]
#![allow(clippy::duplicated_attributes)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{AudioError, AudioEvent, CaptureConfig, CaptureHandle};

// ----- FourCC helpers --------------------------------------------------------

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    // C multi-char constants: the first character is the most-significant
    // byte, e.g. 'dIn ' == 0x64496E20.
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | (d as u32)
}

const SCOPE_GLOBAL: u32 = fourcc(b'g', b'l', b'o', b'b'); // 'glob'
const SCOPE_INPUT: u32 = fourcc(b'i', b'n', b'p', b't'); // 'inpt'
const SEL_DEFAULT_INPUT_DEVICE: u32 = fourcc(b'd', b'I', b'n', b' '); // 'dIn '
const SEL_DEVICES: u32 = fourcc(b'd', b'e', b'v', b'#'); // 'dev#'
const SEL_DEVICE_NAME: u32 = fourcc(b'n', b'a', b'm', b'e'); // 'name'
const SEL_STREAMS: u32 = fourcc(b's', b't', b'm', b'#'); // 'stm#'
const SEL_VIRTUAL_FORMAT: u32 = fourcc(b's', b'f', b'm', b't'); // 'sfmt'

const FMT_LINEAR_PCM: u32 = fourcc(b'l', b'p', b'c', b'm'); // 'lpcm'

const FLAG_IS_FLOAT: u32 = 1 << 0;
const FLAG_IS_SIGNED_INTEGER: u32 = 1 << 2;
const FLAG_IS_PACKED: u32 = 1 << 3;
const FLAG_IS_NON_INTERLEAVED: u32 = 1 << 4;

const SEL_SRC_QUALITY: u32 = fourcc(b's', b'r', b'c', b'q'); // 'srcq'
const QUALITY_MAX: u32 = 0x7F;

// ----- CoreAudio / AudioToolbox FFI ------------------------------------------

#[link(name = "CoreAudio", kind = "framework")]
#[link(name = "AudioToolbox", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {}

pub type OSStatus = i32;
pub type AudioObjectID = u32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioObjectPropertyAddress {
    pub mSelector: u32,
    pub mScope: u32,
    pub mElement: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AudioBuffer {
    pub mNumberChannels: u32,
    pub mDataByteSize: u32,
    pub mData: *mut c_void,
}

#[repr(C)]
pub struct AudioBufferList {
    pub mNumberBuffers: u32,
    pub mBuffers: [AudioBuffer; 1],
}

#[repr(C)]
pub struct AudioTimeStamp {
    pub mFlags: u32,
    pub mHostTime: u64,
    pub mRateScalar: f64,
    pub mWordClockTime: u64,
    pub mSampleTime: f64,
    pub mReserved: *mut u8,
}

pub type AudioDeviceIOProc = unsafe extern "C" fn(
    inDevice: AudioObjectID,
    inNow: *const AudioTimeStamp,
    inInputData: *const AudioBufferList,
    inInputTime: *const AudioTimeStamp,
    outOutputData: *mut AudioBufferList,
    outOutputTime: *const AudioTimeStamp,
    inClientData: *mut c_void,
) -> OSStatus;
pub type AudioDeviceIOProcID = AudioDeviceIOProc;

unsafe extern "C" {
    pub fn AudioObjectGetPropertyData(
        inObjectID: AudioObjectID,
        inAddress: *const AudioObjectPropertyAddress,
        inQualifierDataSize: u32,
        inQualifierData: *const c_void,
        ioDataSize: *mut u32,
        outData: *mut c_void,
    ) -> OSStatus;
    pub fn AudioObjectGetPropertyDataSize(
        inObjectID: AudioObjectID,
        inAddress: *const AudioObjectPropertyAddress,
        inQualifierDataSize: u32,
        inQualifierData: *const c_void,
        outDataSize: *mut u32,
    ) -> OSStatus;
    pub fn AudioDeviceCreateIOProcID(
        inDevice: AudioObjectID,
        inProc: AudioDeviceIOProc,
        inClientData: *mut c_void,
        outIOProcID: *mut AudioDeviceIOProcID,
    ) -> OSStatus;
    pub fn AudioDeviceStart(inDevice: AudioObjectID, inProcID: AudioDeviceIOProcID) -> OSStatus;
    pub fn AudioDeviceStop(inDevice: AudioObjectID, inProcID: AudioDeviceIOProcID) -> OSStatus;
    pub fn AudioDeviceDestroyIOProcID(
        inDevice: AudioObjectID,
        inProcID: AudioDeviceIOProcID,
    ) -> OSStatus;
}

// ----- capture state ---------------------------------------------------------

struct CaptureState {
    tx: Sender<AudioEvent>,
    accum: Vec<i16>,
    frame_samples: usize,
    /// Source ASBD (mono, after down-mix) describing the IO proc's buffers.
    src_format: AudioStreamBasicDescription,
}

unsafe impl Send for CaptureState {}

struct CoreAudioCapture {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    state: Arc<Mutex<CaptureState>>,
    stop: Arc<AtomicBool>,
}

impl Drop for CoreAudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        unsafe {
            AudioDeviceStop(self.device, self.proc_id);
            AudioDeviceDestroyIOProcID(self.device, self.proc_id);
        }
    }
}

unsafe impl Send for CoreAudioCapture {}

fn addr(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: 0,
    }
}

fn default_input_device() -> Result<AudioObjectID, AudioError> {
    // Prefer the system's default input device. On macOS 14+, without
    // Microphone TCC permission this property can return 'who?' (unknown)
    // because input devices are hidden from the process. Fall back to
    // enumerating all devices and picking the first that exposes input
    // streams. The TCC prompt itself is triggered later by AudioDeviceStart.
    let a = addr(SEL_DEFAULT_INPUT_DEVICE, SCOPE_GLOBAL);
    let mut device: AudioObjectID = 0;
    let mut size: u32 = std::mem::size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            1, // kAudioObjectSystemObject
            &a,
            0,
            std::ptr::null(),
            &mut size,
            &mut device as *mut AudioObjectID as *mut c_void,
        )
    };
    if status == 0 && device != 0 && device_has_input_streams(device) {
        return Ok(device);
    }
    // Fallback: enumerate all devices, pick the first with input streams.
    let dev = first_device_with_input()?;
    Ok(dev)
}

fn device_has_input_streams(device: AudioObjectID) -> bool {
    let saddr = addr(SEL_STREAMS, SCOPE_INPUT);
    let mut size: u32 = 0;
    let status =
        unsafe { AudioObjectGetPropertyDataSize(device, &saddr, 0, std::ptr::null(), &mut size) };
    status == 0 && size > 0
}

fn first_device_with_input() -> Result<AudioObjectID, AudioError> {
    let a = addr(SEL_DEVICES, SCOPE_GLOBAL);
    let mut size: u32 = 0;
    let status = unsafe { AudioObjectGetPropertyDataSize(1, &a, 0, std::ptr::null(), &mut size) };
    if status != 0 || size == 0 {
        return Err(AudioError::Resolve(format!("no audio devices ({status})")));
    }
    let count = size as usize / std::mem::size_of::<AudioObjectID>();
    let mut devices: Vec<AudioObjectID> = vec![0; count];
    let status = unsafe {
        AudioObjectGetPropertyData(
            1,
            &a,
            0,
            std::ptr::null(),
            &mut size,
            devices.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 {
        return Err(AudioError::Resolve(format!(
            "device enumeration failed ({status})"
        )));
    }
    for &d in &devices {
        if d != 0 && device_has_input_streams(d) {
            return Ok(d);
        }
    }
    Err(AudioError::NoMicrophone)
}

fn device_input_format(device: AudioObjectID) -> Result<AudioStreamBasicDescription, AudioError> {
    let saddr = addr(SEL_STREAMS, SCOPE_INPUT);
    let mut size: u32 = 0;
    let status =
        unsafe { AudioObjectGetPropertyDataSize(device, &saddr, 0, std::ptr::null(), &mut size) };
    if status != 0 || size == 0 {
        return Err(AudioError::Resolve(format!("no input streams ({status})")));
    }
    let count = size as usize / std::mem::size_of::<AudioObjectID>();
    let mut streams: Vec<AudioObjectID> = vec![0; count];
    let status = unsafe {
        AudioObjectGetPropertyData(
            device,
            &saddr,
            0,
            std::ptr::null(),
            &mut size,
            streams.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 || streams.is_empty() {
        return Err(AudioError::Resolve(format!("no input stream ({status})")));
    }
    let stream = streams[0];
    let faddr = addr(SEL_VIRTUAL_FORMAT, SCOPE_INPUT);
    let mut fmt = AudioStreamBasicDescription::default();
    let mut fsize: u32 = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            stream,
            &faddr,
            0,
            std::ptr::null(),
            &mut fsize,
            &mut fmt as *mut AudioStreamBasicDescription as *mut c_void,
        )
    };
    if status != 0 {
        return Err(AudioError::Resolve(format!("no stream format ({status})")));
    }
    Ok(fmt)
}

/// Target ASBD: 16 kHz, mono, signed-integer 16-bit little-endian PCM.
fn target_format() -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: 16000.0,
        mFormatID: FMT_LINEAR_PCM,
        mFormatFlags: FLAG_IS_SIGNED_INTEGER | FLAG_IS_PACKED,
        mBytesPerPacket: 2,
        mFramesPerPacket: 1,
        mBytesPerFrame: 2,
        mChannelsPerFrame: 1,
        mBitsPerChannel: 16,
        mReserved: 0,
    }
}

/// Down-mix a multi-channel non-interleaved buffer list to a mono interleaved
/// byte stream in the source sample format, then return those bytes.
fn downmix_to_mono_bytes(state: &CaptureState, list: &AudioBufferList) -> Vec<u8> {
    let nbufs = list.mNumberBuffers as usize;
    if nbufs == 0 {
        return Vec::new();
    }
    let fmt = &state.src_format;
    let chans = fmt.mChannelsPerFrame.max(1) as usize;
    let is_float = (fmt.mFormatFlags & FLAG_IS_FLOAT) != 0;
    let is_nonint =
        (fmt.mFormatFlags & FLAG_IS_NON_INTERLEAVED) != 0 && nbufs == chans && chans > 1;
    let bps = (fmt.mBitsPerChannel / 8).max(1) as usize;

    if !is_nonint {
        // Interleaved (or already mono): copy the single buffer's bytes.
        let buf = &list.mBuffers[0];
        let len = buf.mDataByteSize as usize;
        return unsafe { std::slice::from_raw_parts(buf.mData as *const u8, len).to_vec() };
    }

    let frames = if fmt.mBytesPerFrame != 0 {
        (list.mBuffers[0].mDataByteSize as usize) / fmt.mBytesPerFrame as usize
    } else {
        return Vec::new();
    };

    if is_float {
        let mut mono: Vec<f32> = Vec::with_capacity(frames);
        for f in 0..frames {
            let mut sum = 0.0f32;
            for c in 0..chans {
                let base = list.mBuffers[c].mData as *const f32;
                sum += unsafe { *base.add(f) };
            }
            mono.push(sum / chans as f32);
        }
        unsafe { std::slice::from_raw_parts(mono.as_ptr() as *const u8, mono.len() * 4).to_vec() }
    } else {
        // Signed integer non-interleaved: average to mono.
        let mut mono: Vec<u8> = Vec::with_capacity(frames * bps);
        for f in 0..frames {
            let mut sum = 0i64;
            for c in 0..chans {
                let base = list.mBuffers[c].mData as *const u8;
                let v = read_sint(unsafe { base.add(f * bps) }, bps);
                sum += v;
            }
            let avg = (sum / chans as i64) as i32;
            write_sint(&mut mono, avg, bps);
        }
        mono
    }
}

fn read_sint(p: *const u8, bps: usize) -> i64 {
    match bps {
        2 => {
            let (lo, hi) = unsafe { (*p, *p.add(1)) };
            (lo as i16 | ((hi as i16) << 8)) as i64
        }
        3 => {
            let (lo, mid, hi) = unsafe { (*p, *p.add(1), *p.add(2)) };
            let v = (lo as i64) | ((mid as i64) << 8) | ((hi as i64) << 16);
            if hi & 0x80 != 0 { v | !0x00FF_FFFF } else { v }
        }
        4 => {
            let (a, b, c, d) = unsafe { (*p, *p.add(1), *p.add(2), *p.add(3)) };
            (a as i64) | ((b as i64) << 8) | ((c as i64) << 16) | ((d as i64) << 24)
        }
        _ => 0,
    }
}

fn write_sint(out: &mut Vec<u8>, v: i32, bps: usize) {
    match bps {
        2 => out.extend_from_slice(&(v as i16).to_le_bytes()),
        3 => {
            out.push(v as u8);
            out.push((v >> 8) as u8);
            out.push((v >> 16) as u8);
        }
        4 => out.extend_from_slice(&v.to_le_bytes()),
        _ => {}
    }
}

/// Convert a chunk of source-format mono bytes to 16k mono s16le samples.
/// Convert a chunk of source-format mono bytes to 16 kHz mono s16le samples.
///
/// Done in Rust rather than via `AudioConverterConvertBuffer` (which returns
/// `paramErr` for some PCM combinations and is finicky across devices).
/// Supports the common cases: float32 and signed-integer PCM, mono, at the
/// device's native rate, resampled to 16 kHz by linear interpolation.
fn convert_chunk(state: &mut CaptureState, in_bytes: &[u8]) -> Vec<i16> {
    if in_bytes.is_empty() {
        return Vec::new();
    }
    let fmt = &state.src_format;
    let bps = (fmt.mBitsPerChannel / 8).max(1) as usize;
    let bpf = if fmt.mBytesPerFrame != 0 {
        fmt.mBytesPerFrame as usize
    } else {
        bps
    };
    if bpf == 0 {
        return Vec::new();
    }
    let in_frames = in_bytes.len() / bpf;
    if in_frames == 0 {
        return Vec::new();
    }
    let is_float = (fmt.mFormatFlags & FLAG_IS_FLOAT) != 0;
    let is_signed_int = (fmt.mFormatFlags & FLAG_IS_SIGNED_INTEGER) != 0;

    // Decode the mono input bytes into f32 samples in [-1, 1].
    let mut input: Vec<f32> = Vec::with_capacity(in_frames);
    if is_float && bps == 4 {
        for f in 0..in_frames {
            let off = f * bpf;
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&in_bytes[off..off + 4]);
            input.push(f32::from_le_bytes(buf));
        }
    } else if is_signed_int {
        for f in 0..in_frames {
            let off = f * bpf;
            let v = read_sint(in_bytes.as_ptr().wrapping_add(off), bps);
            let scale = match bps {
                2 => 1.0 / 32768.0,
                3 => 1.0 / 8388608.0,
                4 => 1.0 / 2147483648.0,
                _ => 1.0,
            };
            input.push((v as f32) * scale);
        }
    } else {
        return Vec::new();
    }

    // Resample to 16 kHz by linear interpolation.
    let src_rate = fmt.mSampleRate;
    if src_rate <= 0.0 {
        return Vec::new();
    }
    let ratio = src_rate / 16000.0;
    if ratio <= 0.0 {
        return Vec::new();
    }
    let last = in_frames as f64 - 1.0;
    let mut out: Vec<i16> = Vec::with_capacity(((in_frames as f64) / ratio).ceil() as usize);
    let mut i = 0u64;
    loop {
        let pos = i as f64 * ratio;
        if pos >= last {
            break;
        }
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let a = input[idx];
        let b = if idx + 1 < in_frames {
            input[idx + 1]
        } else {
            a
        };
        let v = a + (b - a) * frac;
        out.push((v.clamp(-1.0, 1.0) * 32767.0) as i16);
        i += 1;
    }
    out
}

fn accumulate_and_emit(state: &mut CaptureState, samples: Vec<i16>) {
    state.accum.extend(samples);
    let frame = state.frame_samples;
    while state.accum.len() >= frame {
        let chunk: Vec<i16> = state.accum.drain(..frame).collect();
        if state.tx.send(AudioEvent::Frame(chunk)).is_err() {
            break;
        }
    }
}

unsafe extern "C" fn io_proc(
    _device: AudioObjectID,
    _now: *const AudioTimeStamp,
    input_data: *const AudioBufferList,
    _in_time: *const AudioTimeStamp,
    _out_data: *mut AudioBufferList,
    _out_time: *const AudioTimeStamp,
    client_data: *mut c_void,
) -> OSStatus {
    if input_data.is_null() || client_data.is_null() {
        return 0;
    }
    // SAFETY: client_data points at the Mutex<CaptureState> that is the
    // inner value of the Arc owned by CoreAudioCapture (Arc::as_ptr yields
    // *const T, T = Mutex<CaptureState>). The Arc is kept alive until after
    // AudioDeviceStop/DestroyIOProcID in CoreAudioCapture::Drop, so the
    // pointer is valid for every IO proc callback.
    let state_mutex = unsafe { &*(client_data as *const Mutex<CaptureState>) };
    let mut guard = match state_mutex.lock() {
        Ok(g) => g,
        Err(_) => return 0,
    };
    let list = unsafe { &*input_data };
    let bytes = downmix_to_mono_bytes(&guard, list);
    let samples = convert_chunk(&mut guard, &bytes);
    accumulate_and_emit(&mut guard, samples);
    0
}

/// Entry point used by `start_capture` on macOS.
pub fn start_capture_macos(config: CaptureConfig) -> Result<CaptureHandle, AudioError> {
    if config.frame_ms == 0 {
        return Err(AudioError::Resolve(
            "frame_ms must be at least 1".to_string(),
        ));
    }
    let device = default_input_device()?;
    let in_format = device_input_format(device)?;

    // The IO proc down-mixes to mono before conversion; the converter runs in
    // Rust (float/int decode + linear-interpolation resample to 16 kHz).
    let mut src_format = in_format;
    if src_format.mChannelsPerFrame > 1 {
        src_format.mChannelsPerFrame = 1;
        let bpp = src_format.mBytesPerPacket / src_format.mFramesPerPacket.max(1);
        src_format.mBytesPerFrame = bpp;
        src_format.mBytesPerPacket = bpp * src_format.mFramesPerPacket.max(1);
    }
    let _ = target_format(); // target shape is handled in Rust convert_chunk

    let frame_samples = (16000 * config.frame_ms as usize) / 1000;
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let state = Arc::new(Mutex::new(CaptureState {
        tx,
        accum: Vec::new(),
        frame_samples,
        src_format,
    }));
    let state_ptr = Arc::as_ptr(&state) as *mut c_void;

    let mut proc_id_slot: std::mem::MaybeUninit<AudioDeviceIOProcID> =
        std::mem::MaybeUninit::uninit();
    let status =
        unsafe { AudioDeviceCreateIOProcID(device, io_proc, state_ptr, proc_id_slot.as_mut_ptr()) };
    if status != 0 {
        return Err(AudioError::Resolve(format!(
            "AudioDeviceCreateIOProcID failed ({status})"
        )));
    }
    let proc_id = unsafe { proc_id_slot.assume_init() };
    let status = unsafe { AudioDeviceStart(device, proc_id) };
    if status != 0 {
        unsafe {
            AudioDeviceDestroyIOProcID(device, proc_id);
        }
        return Err(AudioError::Resolve(format!(
            "AudioDeviceStart failed ({status})"
        )));
    }

    let _ = state.lock().ok().and_then(|g| {
        g.tx.send(AudioEvent::Started {
            device: format!("coreaudio:{device}"),
            description: Some("CoreAudio default input".to_string()),
        })
        .ok()
    });

    let capture = CoreAudioCapture {
        device,
        proc_id,
        state,
        stop: Arc::clone(&stop),
    };

    // The capture thread owns the CoreAudioCapture; when CaptureHandle's stop
    // is set, the thread drops it (tearing down CoreAudio) and exits.
    let stop_for_thread = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        let _capture = capture;
        while !stop_for_thread.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(50));
        }
        // CoreAudioCapture drops here -> AudioDeviceStop + DestroyIOProcID.
    });

    Ok(CaptureHandle {
        events: rx,
        stop,
        child: Arc::new(Mutex::new(None)),
        thread: Some(thread),
    })
}

// Keep the JoinHandle import used.
#[allow(dead_code)]
fn _use_join(_h: JoinHandle<()>) {}
