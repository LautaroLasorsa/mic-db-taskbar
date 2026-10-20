//! Per-device WASAPI shared-mode capture computing RMS level in dBFS.
//!
//! Each [`Meter`] owns one capture thread. The thread publishes the RMS level of the last
//! ~100 ms window through an atomic, so the UI thread never blocks on audio.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::thread::JoinHandle;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    IAudioCaptureClient, IAudioClient, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{Error, HSTRING, Result};

use crate::devices;

/// Lowest level reported; digital silence maps here.
pub const FLOOR_DB: f32 = -90.0;
/// Length of one RMS window.
const WINDOW_MS: u32 = 100;
/// Requested shared-mode buffer, in 100 ns units (200 ms).
const BUFFER_DURATION_HNS: i64 = 2_000_000;
/// How long the capture thread waits for data before re-checking the stop flag.
const WAIT_TIMEOUT_MS: u32 = 200;

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

const STATE_STARTING: u8 = 0;
const STATE_RUNNING: u8 = 1;
const STATE_FAILED: u8 = 2;

struct Shared {
    level_bits: AtomicU32,
    state: AtomicU8,
}

/// Live level meter for one capture endpoint. Dropping it stops the capture thread.
pub struct Meter {
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Meter {
    pub fn start(device_id: &str) -> Self {
        let shared = Arc::new(Shared {
            level_bits: AtomicU32::new(FLOOR_DB.to_bits()),
            state: AtomicU8::new(STATE_STARTING),
        });
        let stop = Arc::new(AtomicBool::new(false));
        let (thread_shared, thread_stop) = (Arc::clone(&shared), Arc::clone(&stop));
        let id = device_id.to_owned();
        let thread = std::thread::Builder::new()
            .name("capture".to_owned())
            .spawn(move || {
                let outcome = capture_loop(&id, &thread_shared, &thread_stop);
                let final_state = if outcome.is_ok() {
                    STATE_STARTING
                } else {
                    STATE_FAILED
                };
                thread_shared.state.store(final_state, Ordering::Release);
            })
            .ok();
        if thread.is_none() {
            shared.state.store(STATE_FAILED, Ordering::Release);
        }
        Self {
            shared,
            stop,
            thread,
        }
    }

    /// Latest window level in dBFS, or `None` while not capturing.
    pub fn level_db(&self) -> Option<f32> {
        (self.shared.state.load(Ordering::Acquire) == STATE_RUNNING)
            .then(|| f32::from_bits(self.shared.level_bits.load(Ordering::Relaxed)))
    }

    pub fn has_failed(&self) -> bool {
        self.shared.state.load(Ordering::Acquire) == STATE_FAILED
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// RAII guard pairing `CoInitializeEx` with `CoUninitialize`.
struct ComApartment;

impl ComApartment {
    fn multithreaded() -> Result<Self> {
        // SAFETY: balanced by CoUninitialize in Drop on the same thread.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: matches the successful CoInitializeEx in `multithreaded`.
        unsafe { CoUninitialize() };
    }
}

struct OwnedEvent(HANDLE);

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        // SAFETY: handle created by CreateEventW and owned exclusively here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn capture_loop(device_id: &str, shared: &Shared, stop: &AtomicBool) -> Result<()> {
    let _com = ComApartment::multithreaded()?;
    // SAFETY: COM calls on interfaces obtained in this apartment; the buffer pointer
    // returned by GetBuffer is only read before the matching ReleaseBuffer.
    unsafe {
        let device = devices::enumerator()?.GetDevice(&HSTRING::from(device_id))?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let format = initialize_client(&client)?;
        let event = OwnedEvent(CreateEventW(None, false, false, None)?);
        client.SetEventHandle(event.0)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        shared.state.store(STATE_RUNNING, Ordering::Release);

        let mut window = RmsWindow::new(format.sample_rate * WINDOW_MS / 1000);
        let result = (|| {
            while !stop.load(Ordering::Acquire) {
                if WaitForSingleObject(event.0, WAIT_TIMEOUT_MS) == WAIT_FAILED {
                    return Err(Error::from_thread());
                }
                drain_packets(&capture, &format, &mut window, shared)?;
            }
            Ok(())
        })();
        let _ = client.Stop();
        result
    }
}

/// Initializes the client in shared mode with the engine mix format and returns that format.
unsafe fn initialize_client(client: &IAudioClient) -> Result<SampleFormat> {
    // SAFETY: GetMixFormat returns a CoTaskMem block we free after Initialize copied it.
    unsafe {
        let mix = client.GetMixFormat()?;
        let format = SampleFormat::from_wave_format(mix);
        let init = client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            BUFFER_DURATION_HNS,
            0,
            mix,
            None,
        );
        CoTaskMemFree(Some(mix as *const _));
        init?;
        format
            .ok_or_else(|| Error::new(windows::Win32::Foundation::E_NOTIMPL, "unsupported format"))
    }
}

unsafe fn drain_packets(
    capture: &IAudioCaptureClient,
    format: &SampleFormat,
    window: &mut RmsWindow,
    shared: &Shared,
) -> Result<()> {
    // SAFETY: see `capture_loop`.
    unsafe {
        while capture.GetNextPacketSize()? > 0 {
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
            if silent || data.is_null() {
                window.push_silence(frames, format.channels);
            } else {
                let len = frames as usize * format.block_align;
                let bytes = std::slice::from_raw_parts(data, len);
                format.for_each_sample(bytes, |sample| window.push(sample));
                window.end_frames(frames);
            }
            capture.ReleaseBuffer(frames)?;
            if let Some(db) = window.take_completed() {
                shared.level_bits.store(db.to_bits(), Ordering::Relaxed);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Float32,
    Int16,
    Int24,
    Int32,
}

#[derive(Debug, Clone, Copy)]
struct SampleFormat {
    encoding: Encoding,
    channels: u32,
    sample_rate: u32,
    block_align: usize,
}

impl SampleFormat {
    /// Decodes a (possibly extensible) wave format; `None` if the encoding is unsupported.
    unsafe fn from_wave_format(ptr: *const WAVEFORMATEX) -> Option<Self> {
        // SAFETY: caller passes a valid WAVEFORMATEX; extensible formats are large enough.
        let base = unsafe { std::ptr::read_unaligned(ptr) };
        let tag = if base.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
            let ext = unsafe { std::ptr::read_unaligned(ptr.cast::<WAVEFORMATEXTENSIBLE>()) };
            // KSDATAFORMAT_SUBTYPE_{PCM,IEEE_FLOAT} carry the legacy tag in their first field.
            ext.SubFormat.data1 as u16
        } else {
            base.wFormatTag
        };
        let channels = u32::from(base.nChannels);
        let block_align = usize::from(base.nBlockAlign);
        if channels == 0 {
            return None;
        }
        let container_bytes = block_align / channels as usize;
        let encoding = match (tag, container_bytes) {
            (WAVE_FORMAT_IEEE_FLOAT, 4) => Encoding::Float32,
            (WAVE_FORMAT_PCM, 2) => Encoding::Int16,
            (WAVE_FORMAT_PCM, 3) => Encoding::Int24,
            (WAVE_FORMAT_PCM, 4) => Encoding::Int32,
            _ => return None,
        };
        Some(Self {
            encoding,
            channels,
            sample_rate: base.nSamplesPerSec,
            block_align,
        })
    }

    fn for_each_sample(&self, bytes: &[u8], mut f: impl FnMut(f32)) {
        match self.encoding {
            Encoding::Float32 => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .for_each(|b| f(f32::from_le_bytes(*b))),
            Encoding::Int16 => bytes
                .as_chunks::<2>()
                .0
                .iter()
                .for_each(|b| f(f32::from(i16::from_le_bytes(*b)) / 32_768.0)),
            Encoding::Int24 => bytes.as_chunks::<3>().0.iter().for_each(|&[lo, mid, hi]| {
                let value = i32::from_le_bytes([0, lo, mid, hi]) >> 8;
                f(value as f32 / 8_388_608.0)
            }),
            Encoding::Int32 => bytes
                .as_chunks::<4>()
                .0
                .iter()
                .for_each(|b| f(i32::from_le_bytes(*b) as f32 / 2_147_483_648.0)),
        }
    }
}

/// Accumulates squared samples and emits one dBFS value per completed window.
struct RmsWindow {
    frames_per_window: u32,
    frames: u32,
    samples: u64,
    sum_squares: f64,
    completed: Option<f32>,
}

impl RmsWindow {
    fn new(frames_per_window: u32) -> Self {
        Self {
            frames_per_window: frames_per_window.max(1),
            frames: 0,
            samples: 0,
            sum_squares: 0.0,
            completed: None,
        }
    }

    fn push(&mut self, sample: f32) {
        self.sum_squares += f64::from(sample) * f64::from(sample);
        self.samples += 1;
    }

    fn push_silence(&mut self, frames: u32, channels: u32) {
        self.samples += u64::from(frames) * u64::from(channels);
        self.end_frames(frames);
    }

    fn end_frames(&mut self, frames: u32) {
        self.frames += frames;
        if self.frames >= self.frames_per_window {
            self.completed = Some(rms_to_db(self.sum_squares, self.samples));
            self.frames = 0;
            self.samples = 0;
            self.sum_squares = 0.0;
        }
    }

    fn take_completed(&mut self) -> Option<f32> {
        self.completed.take()
    }
}

fn rms_to_db(sum_squares: f64, samples: u64) -> f32 {
    if samples == 0 {
        return FLOOR_DB;
    }
    let rms = (sum_squares / samples as f64).sqrt();
    if rms <= 0.0 {
        return FLOOR_DB;
    }
    ((20.0 * rms.log10()) as f32).max(FLOOR_DB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_square_is_zero_db_and_silence_is_floor() {
        let mut window = RmsWindow::new(4);
        (0..8).for_each(|i| window.push(if i % 2 == 0 { 1.0 } else { -1.0 }));
        window.end_frames(4);
        assert!(window.take_completed().expect("window complete").abs() < 1e-4);
        window.push_silence(4, 2);
        assert_eq!(window.take_completed(), Some(FLOOR_DB));
    }

    #[test]
    fn decodes_int24_little_endian() {
        let format = SampleFormat {
            encoding: Encoding::Int24,
            channels: 1,
            sample_rate: 48_000,
            block_align: 3,
        };
        let mut seen = Vec::new();
        format.for_each_sample(&[0x00, 0x00, 0xC0], |s| seen.push(s));
        assert_eq!(seen, vec![-0.5]);
    }
}
