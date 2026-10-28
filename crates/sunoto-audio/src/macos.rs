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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::{AudioError, AudioEvent, CaptureConfig, CaptureHandle};

const DEVICE_RECHECK: Duration = Duration::from_secs(1);

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
const SEL_TRANSPORT_TYPE: u32 = fourcc(b't', b'r', b'a', b'n'); // 'tran'
const SEL_NOMINAL_SAMPLE_RATE: u32 = fourcc(b'n', b's', b'r', b't'); // 'nsrt'
const SEL_PROCESS_OBJECTS: u32 = fourcc(b'p', b'r', b's', b'#'); // 'prs#' (macOS 14.2+)
const SEL_PROCESS_PID: u32 = fourcc(b'p', b'p', b'i', b'd'); // 'ppid'
const SEL_PROCESS_RUNNING_INPUT: u32 = fourcc(b'p', b'i', b'r', b'i'); // 'piri'

const TRANSPORT_BUILT_IN: u32 = fourcc(b'b', b'l', b't', b'n');
const TRANSPORT_USB: u32 = fourcc(b'u', b's', b'b', b' ');
const TRANSPORT_BLUETOOTH: u32 = fourcc(b'b', b'l', b'u', b'e');
const TRANSPORT_BLUETOOTH_LE: u32 = fourcc(b'b', b'l', b'e', b'a');

const TIMESTAMP_SAMPLE_TIME_VALID: u32 = 1;

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
    /// Bytes per frame of the device's first buffer, before the down-mix.
    in_bytes_per_frame: u32,
    /// Device sample time the next callback should start at, once known.
    next_sample_time: Option<f64>,
}

/// Input lost between callbacks, over the whole process: CoreAudio skipped
/// ahead because a callback came late (a busy Mac), counted from the input
/// timestamps. `crate::input_gaps` reads them.
pub(crate) static GAP_COUNT: AtomicU64 = AtomicU64::new(0);
pub(crate) static GAP_LOST_US: AtomicU64 = AtomicU64::new(0);

unsafe impl Send for CaptureState {}

struct CoreAudioCapture {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    state: Arc<Mutex<CaptureState>>,
}

impl Drop for CoreAudioCapture {
    fn drop(&mut self) {
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

fn configured_input_device(config: &CaptureConfig) -> Result<AudioObjectID, AudioError> {
    if config.device == "auto" {
        return default_input_device();
    }
    let raw = config
        .device
        .strip_prefix("coreaudio:")
        .unwrap_or(&config.device);
    let device = raw
        .parse::<AudioObjectID>()
        .map_err(|_| AudioError::Resolve(format!("invalid CoreAudio device id: {raw}")))?;
    if device != 0 && device_has_input_streams(device) {
        Ok(device)
    } else {
        Err(AudioError::NoMicrophone)
    }
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

fn start_device_capture(
    device: AudioObjectID,
    frame_ms: u32,
    tx: Sender<AudioEvent>,
) -> Result<CoreAudioCapture, AudioError> {
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

    let frame_samples = (16000 * frame_ms as usize) / 1000;
    let state = Arc::new(Mutex::new(CaptureState {
        tx,
        accum: Vec::new(),
        frame_samples,
        src_format,
        in_bytes_per_frame: in_format.mBytesPerFrame,
        next_sample_time: None,
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

    Ok(CoreAudioCapture {
        device,
        proc_id,
        state,
    })
}

fn send_started(tx: &Sender<AudioEvent>, device: AudioObjectID) -> bool {
    tx.send(AudioEvent::Started {
        device: format!("coreaudio:{device}"),
        description: Some("CoreAudio default input".to_string()),
    })
    .is_ok()
}

fn sleep_unless_stopped(stop: &AtomicBool, delay: Duration) {
    let deadline = Instant::now() + delay;
    while !stop.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(50)));
    }
}

unsafe extern "C" fn io_proc(
    _device: AudioObjectID,
    _now: *const AudioTimeStamp,
    input_data: *const AudioBufferList,
    in_time: *const AudioTimeStamp,
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
    if !in_time.is_null() && list.mNumberBuffers > 0 {
        let frames = list.mBuffers[0].mDataByteSize / guard.in_bytes_per_frame.max(1);
        note_gap(&mut guard, unsafe { &*in_time }, frames);
    }
    let bytes = downmix_to_mono_bytes(&guard, list);
    let samples = convert_chunk(&mut guard, &bytes);
    accumulate_and_emit(&mut guard, samples);
    0
}

/// Count a jump in the input's sample time: frames the device produced that
/// never reached this callback.
fn note_gap(state: &mut CaptureState, time: &AudioTimeStamp, frames: u32) {
    if time.mFlags & TIMESTAMP_SAMPLE_TIME_VALID == 0 {
        state.next_sample_time = None;
        return;
    }
    if let Some(expected) = state.next_sample_time {
        let skipped = time.mSampleTime - expected;
        // Sample times are whole frames; anything under one is jitter.
        if skipped >= 1.0 {
            GAP_COUNT.fetch_add(1, Ordering::Relaxed);
            let rate = state.src_format.mSampleRate.max(1.0);
            GAP_LOST_US.fetch_add((skipped / rate * 1e6) as u64, Ordering::Relaxed);
        }
    }
    state.next_sample_time = Some(time.mSampleTime + f64::from(frames));
}

/// Mic type, rate and other users of the input for the accuracy record.
pub(crate) fn input_info(device: &str) -> Option<crate::InputInfo> {
    let device: AudioObjectID = device.strip_prefix("coreaudio:")?.parse().ok()?;
    let transport: u32 = get_property(device, SEL_TRANSPORT_TYPE)?;
    let rate: f64 = get_property(device, SEL_NOMINAL_SAMPLE_RATE)?;
    let kind = match transport {
        TRANSPORT_BUILT_IN => crate::MicKind::BuiltIn,
        TRANSPORT_USB => crate::MicKind::Usb,
        TRANSPORT_BLUETOOTH | TRANSPORT_BLUETOOTH_LE => crate::MicKind::Bluetooth,
        _ => crate::MicKind::Other,
    };
    Some(crate::InputInfo {
        kind,
        sample_rate: rate.round() as u32,
        other_apps: other_apps_recording(),
    })
}

/// How many other processes are recording from any input now. `None` before
/// macOS 14.2, which has no process objects.
fn other_apps_recording() -> Option<u32> {
    let a = addr(SEL_PROCESS_OBJECTS, SCOPE_GLOBAL);
    let mut size: u32 = 0;
    let status = unsafe { AudioObjectGetPropertyDataSize(1, &a, 0, std::ptr::null(), &mut size) };
    if status != 0 {
        return None;
    }
    let mut processes: Vec<AudioObjectID> = vec![0; size as usize / std::mem::size_of::<AudioObjectID>()];
    let status = unsafe {
        AudioObjectGetPropertyData(
            1,
            &a,
            0,
            std::ptr::null(),
            &mut size,
            processes.as_mut_ptr() as *mut c_void,
        )
    };
    if status != 0 {
        return None;
    }
    let me = std::process::id() as i32;
    let others = processes
        .into_iter()
        .filter(|&process| {
            get_property::<i32>(process, SEL_PROCESS_PID).is_some_and(|pid| pid != me)
                && get_property::<u32>(process, SEL_PROCESS_RUNNING_INPUT).is_some_and(|on| on != 0)
        })
        .count();
    Some(others as u32)
}

/// A fixed-size global property of `object`.
fn get_property<T: Copy + Default>(object: AudioObjectID, selector: u32) -> Option<T> {
    let a = addr(selector, SCOPE_GLOBAL);
    let mut value = T::default();
    let mut size = std::mem::size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &a,
            0,
            std::ptr::null(),
            &mut size,
            &mut value as *mut T as *mut c_void,
        )
    };
    (status == 0 && size as usize == std::mem::size_of::<T>()).then_some(value)
}

/// Entry point used by `start_capture` on macOS.
pub fn start_capture_macos(config: CaptureConfig) -> Result<CaptureHandle, AudioError> {
    if config.frame_ms == 0 {
        return Err(AudioError::Resolve(
            "frame_ms must be at least 1".to_string(),
        ));
    }
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let mut device = configured_input_device(&config)?;
    let capture = start_device_capture(device, config.frame_ms, tx.clone())?;
    let _ = send_started(&tx, device);

    // The capture thread owns the CoreAudioCapture; when CaptureHandle's stop
    // is set, the thread drops it (tearing down CoreAudio) and exits. For
    // "auto", it also follows the system default input if the user connects or
    // selects another mic after startup.
    let stop_for_thread = Arc::clone(&stop);
    let thread = thread::spawn(move || {
        let mut capture = Some(capture);
        let mut next_recheck = Instant::now() + DEVICE_RECHECK;
        let mut backoff_index = 0usize;
        while !stop_for_thread.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(50));
            if config.device != "auto" || Instant::now() < next_recheck {
                continue;
            }
            next_recheck = Instant::now() + DEVICE_RECHECK;
            let Ok(next_device) = default_input_device() else {
                continue;
            };
            if next_device == device {
                continue;
            }

            drop(capture.take());
            if tx
                .send(AudioEvent::Stopped {
                    reason: format!(
                        "CoreAudio input device changed from coreaudio:{device} to coreaudio:{next_device}"
                    ),
                })
                .is_err()
            {
                break;
            }

            loop {
                if stop_for_thread.load(Ordering::SeqCst) {
                    break;
                }
                match start_device_capture(next_device, config.frame_ms, tx.clone()) {
                    Ok(new_capture) => {
                        device = next_device;
                        capture = Some(new_capture);
                        backoff_index = 0;
                        let _ = send_started(&tx, device);
                        break;
                    }
                    Err(error) => {
                        let _ = tx.send(AudioEvent::Stopped {
                            reason: format!(
                                "CoreAudio capture unavailable after input switch: {error}"
                            ),
                        });
                        let delay = config
                            .restart_backoff_ms
                            .get(backoff_index)
                            .or_else(|| config.restart_backoff_ms.last())
                            .copied()
                            .unwrap_or(1000);
                        backoff_index = (backoff_index + 1)
                            .min(config.restart_backoff_ms.len().saturating_sub(1));
                        sleep_unless_stopped(&stop_for_thread, Duration::from_millis(delay));
                    }
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(sample_time: f64) -> AudioTimeStamp {
        AudioTimeStamp {
            mFlags: TIMESTAMP_SAMPLE_TIME_VALID,
            mHostTime: 0,
            mRateScalar: 1.0,
            mWordClockTime: 0,
            mSampleTime: sample_time,
            mReserved: std::ptr::null_mut(),
        }
    }

    #[test]
    fn a_jump_in_sample_time_counts_as_a_gap() {
        let (tx, _rx) = mpsc::channel();
        let mut format = target_format();
        format.mSampleRate = 48_000.0;
        let mut state = CaptureState {
            tx,
            accum: Vec::new(),
            frame_samples: 320,
            src_format: format,
            in_bytes_per_frame: 4,
            next_sample_time: None,
        };
        let before = crate::input_gaps();
        note_gap(&mut state, &stamp(0.0), 512);
        note_gap(&mut state, &stamp(512.0), 512);
        // 4,800 frames at 48 kHz never arrived: 100 ms.
        note_gap(&mut state, &stamp(1_024.0 + 4_800.0), 512);
        let gaps = crate::input_gaps().since(before);
        // Other tests may run captures in parallel; this one adds exactly one.
        assert!(gaps.count >= 1);
        assert!(gaps.lost_us >= 100_000);
        assert_eq!(state.next_sample_time, Some(1_024.0 + 4_800.0 + 512.0));
    }

    #[test]
    fn the_default_input_has_a_kind_and_rate() {
        let Ok(device) = default_input_device() else {
            return; // no microphone on this machine
        };
        let info = input_info(&format!("coreaudio:{device}")).expect("device answers");
        assert!(info.sample_rate >= 8_000, "{info:?}");
    }
}
