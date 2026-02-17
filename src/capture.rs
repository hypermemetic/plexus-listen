//! Functional audio capture helpers — no shared state, each call owns its resources

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Stream;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::HeapRb;
use rubato::Resampler;
use audioadapter_buffers::direct::SequentialSliceOfVecs;

use crate::device;
#[cfg(target_os = "macos")]
use crate::sck;

/// Wrapper to make cpal::Stream safely Send.
/// We only keep the stream alive (drop stops it) — the audio callback
/// runs on its own OS thread managed by cpal.
pub struct SendStream(pub Stream);
unsafe impl Send for SendStream {}

impl SendStream {
    pub fn play(&self) -> Result<(), cpal::PlayStreamError> {
        self.0.play()
    }
}

/// Size of the ring buffer in frames (at 48kHz stereo, ~10 seconds)
const RING_BUFFER_FRAMES: usize = 48000 * 10;

/// Size of the monitor ring buffer in frames (~2 seconds at output rate)
const MONITOR_BUFFER_FRAMES: usize = 48000 * 2;

/// Open a cpal input device by name (or auto-detect DJI).
/// Returns the device and a preferred stream config.
pub fn open_input_device(
    device_name: Option<&str>,
) -> Result<(cpal::Device, cpal::StreamConfig), String> {
    let dev = device::find_audio_device(device_name).ok_or_else(|| match device_name {
        Some(name) => format!("Audio input device matching '{}' not found", name),
        None => "No audio input device found".to_string(),
    })?;

    let config = device::preferred_stream_config(&dev)
        .ok_or_else(|| "Could not determine stream config for device".to_string())?;

    Ok((dev, config))
}

/// Create a ring buffer sized for capture and return the split halves.
pub fn create_ring_buffer(channels: u16) -> (ringbuf::HeapProd<f32>, ringbuf::HeapCons<f32>) {
    let rb = HeapRb::<f32>::new(RING_BUFFER_FRAMES * channels as usize);
    rb.split()
}

/// Build a cpal input stream that pushes samples into the given ring buffer producer.
/// The returned SendStream must be kept alive — dropping it stops capture.
/// The error_flag is set to true if the audio device reports an error (e.g. disconnect).
pub fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut producer: ringbuf::HeapProd<f32>,
    error_flag: Arc<AtomicBool>,
) -> Result<SendStream, String> {
    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = producer.push_slice(data);
            },
            move |err| {
                tracing::error!("Audio input stream error: {}", err);
                error_flag.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| format!("Failed to build input stream: {}", e))?;

    stream
        .play()
        .map_err(|e| format!("Failed to start audio stream: {}", e))?;

    Ok(SendStream(stream))
}

/// Build a cpal input stream that pushes to TWO ring buffer producers (metering + monitoring).
/// Used by the monitor method which needs separate data paths.
/// The error_flag is set to true if the audio device reports an error (e.g. disconnect).
pub fn build_dual_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut producer_a: ringbuf::HeapProd<f32>,
    mut producer_b: ringbuf::HeapProd<f32>,
    error_flag: Arc<AtomicBool>,
) -> Result<SendStream, String> {
    let stream = device
        .build_input_stream(
            config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let _ = producer_a.push_slice(data);
                let _ = producer_b.push_slice(data);
            },
            move |err| {
                tracing::error!("Audio input stream error: {}", err);
                error_flag.store(true, Ordering::Relaxed);
            },
            None,
        )
        .map_err(|e| format!("Failed to build input stream: {}", e))?;

    stream
        .play()
        .map_err(|e| format!("Failed to start audio stream: {}", e))?;

    Ok(SendStream(stream))
}

/// Unified audio source — either a hardware device or a macOS application.
pub enum AudioSource {
    Hardware(cpal::Device, cpal::StreamConfig),
    #[cfg(target_os = "macos")]
    App(sck::AppSource),
}

impl AudioSource {
    pub fn sample_rate(&self) -> u32 {
        match self {
            AudioSource::Hardware(_, config) => config.sample_rate.0,
            #[cfg(target_os = "macos")]
            AudioSource::App(_) => 48000,
        }
    }

    pub fn channels(&self) -> u16 {
        match self {
            AudioSource::Hardware(_, config) => config.channels,
            #[cfg(target_os = "macos")]
            AudioSource::App(_) => 2,
        }
    }

    pub fn name(&self) -> String {
        match self {
            AudioSource::Hardware(dev, _) => {
                cpal::traits::DeviceTrait::name(dev).unwrap_or_else(|_| "Unknown".to_string())
            }
            #[cfg(target_os = "macos")]
            AudioSource::App(app) => app.name.clone(),
        }
    }

    pub fn is_dji(&self) -> bool {
        match self {
            AudioSource::Hardware(dev, _) => {
                cpal::traits::DeviceTrait::name(dev)
                    .map(|n| n.contains("DJI") || n.contains("dji"))
                    .unwrap_or(false)
            }
            #[cfg(target_os = "macos")]
            AudioSource::App(_) => false,
        }
    }
}

/// Unified capture stream — either cpal hardware or SCK app capture.
pub enum CaptureStream {
    Cpal(SendStream),
    #[cfg(target_os = "macos")]
    Sck(sck::AppCaptureGuard),
}

/// Open an audio source by device name.
/// Tries hardware first, then macOS app capture. Default (None) = hardware auto-detect only.
pub fn open_audio_source(
    device_name: Option<&str>,
) -> Result<AudioSource, String> {
    match device_name {
        None => {
            // Default: hardware auto-detect (existing DJI behavior)
            let (dev, config) = open_input_device(None)?;
            Ok(AudioSource::Hardware(dev, config))
        }
        Some(name) => {
            // Try hardware first
            if let Some(dev) = device::find_audio_device(Some(name)) {
                let config = device::preferred_stream_config(&dev)
                    .ok_or_else(|| "Could not determine stream config for device".to_string())?;
                return Ok(AudioSource::Hardware(dev, config));
            }
            // Try app capture on macOS
            #[cfg(target_os = "macos")]
            {
                if let Some(app) = device::find_app_source(name) {
                    return Ok(AudioSource::App(app));
                }
            }
            Err(format!("No audio device or application matching '{}' found", name))
        }
    }
}

/// Build a capture stream from a unified audio source.
pub fn build_capture_stream(
    source: &AudioSource,
    producer: ringbuf::HeapProd<f32>,
    error_flag: Arc<AtomicBool>,
) -> Result<CaptureStream, String> {
    match source {
        AudioSource::Hardware(dev, config) => {
            let stream = build_input_stream(dev, config, producer, error_flag)?;
            Ok(CaptureStream::Cpal(stream))
        }
        #[cfg(target_os = "macos")]
        AudioSource::App(app) => {
            let guard = sck::start_app_capture(app, producer, error_flag)?;
            Ok(CaptureStream::Sck(guard))
        }
    }
}

/// Build a dual capture stream (metering + monitoring) from a unified audio source.
pub fn build_dual_capture_stream(
    source: &AudioSource,
    producer_a: ringbuf::HeapProd<f32>,
    producer_b: ringbuf::HeapProd<f32>,
    error_flag: Arc<AtomicBool>,
) -> Result<CaptureStream, String> {
    match source {
        AudioSource::Hardware(dev, config) => {
            let stream = build_dual_input_stream(dev, config, producer_a, producer_b, error_flag)?;
            Ok(CaptureStream::Cpal(stream))
        }
        #[cfg(target_os = "macos")]
        AudioSource::App(app) => {
            let guard = sck::start_app_capture_dual(app, producer_a, producer_b, error_flag)?;
            Ok(CaptureStream::Sck(guard))
        }
    }
}

/// Guard that stops the resampler thread when dropped.
/// Also holds the output cpal::Stream alive.
pub struct MonitorGuard {
    running: Arc<AtomicBool>,
    _output_stream: Stream,
}

// cpal::Stream is Send but not Sync; we protect it inside the guard
unsafe impl Send for MonitorGuard {}
unsafe impl Sync for MonitorGuard {}

impl Drop for MonitorGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

/// Build a monitoring output pipeline.
///
/// Returns a `MonitorGuard` (keep alive — dropping stops everything) and a
/// `HeapProd<f32>` that the caller feeds input-rate interleaved samples into.
///
/// Internally spawns a resampler thread (if rates differ) that reads from the
/// monitor input ring buffer, resamples via `rubato::Fft`, and writes to an
/// output ring buffer consumed by the cpal output callback.
pub fn build_monitor_output(
    input_rate: u32,
    input_channels: u16,
) -> Result<(MonitorGuard, ringbuf::HeapProd<f32>), String> {
    let host = cpal::default_host();
    let output_device = host
        .default_output_device()
        .ok_or_else(|| "No audio output device found".to_string())?;

    // Determine output config
    let output_config = {
        let supported = output_device
            .supported_output_configs()
            .map_err(|e| format!("Failed to query output configs: {}", e))?;

        let mut matched = false;
        for cfg in supported {
            if cfg.channels() >= input_channels
                && cfg.min_sample_rate().0 <= input_rate
                && cfg.max_sample_rate().0 >= input_rate
            {
                matched = true;
                break;
            }
        }

        if matched {
            cpal::StreamConfig {
                channels: input_channels,
                sample_rate: cpal::SampleRate(input_rate),
                buffer_size: cpal::BufferSize::Default,
            }
        } else {
            let default_cfg = output_device
                .default_output_config()
                .map_err(|e| format!("Failed to get default output config: {}", e))?;
            tracing::warn!(
                "Output device doesn't support {}Hz/{}ch, using default: {}Hz/{}ch",
                input_rate,
                input_channels,
                default_cfg.sample_rate().0,
                default_cfg.channels()
            );
            default_cfg.config()
        }
    };

    let output_rate = output_config.sample_rate.0;
    let output_channels = output_config.channels;
    let needs_resample = output_rate != input_rate;

    tracing::info!(
        "Monitor output: {}Hz, {}ch{}",
        output_rate,
        output_channels,
        if needs_resample {
            format!(" (resampling from {}Hz)", input_rate)
        } else {
            String::new()
        }
    );

    let running = Arc::new(AtomicBool::new(true));

    if needs_resample {
        // === Resampled path ===
        // Input callback → monitor_in_rb → resampler thread → monitor_out_rb → output callback

        // Ring buffer: caller pushes input-rate samples here
        let in_rb = HeapRb::<f32>::new(MONITOR_BUFFER_FRAMES * input_channels as usize);
        let (monitor_producer, staging_consumer) = in_rb.split();

        // Ring buffer: resampler thread pushes output-rate samples here
        let out_rb = HeapRb::<f32>::new(MONITOR_BUFFER_FRAMES * output_channels as usize);
        let (out_producer, mut out_consumer) = out_rb.split();

        // Spawn resampler thread
        let ch_count = input_channels as usize;
        let running_thread = running.clone();

        std::thread::Builder::new()
            .name("audio-resample".to_string())
            .spawn(move || {
                resample_loop(
                    staging_consumer,
                    out_producer,
                    input_rate,
                    output_rate,
                    ch_count,
                    running_thread,
                );
            })
            .map_err(|e| format!("Failed to spawn resampler thread: {}", e))?;

        // Output stream reads from out_rb
        let out_ch = output_channels as usize;
        let in_ch = input_channels as usize;

        let output_stream = output_device
            .build_output_stream(
                &output_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if out_ch == in_ch {
                        let n = out_consumer.pop_slice(data);
                        for s in &mut data[n..] {
                            *s = 0.0;
                        }
                    } else {
                        // Channel count mismatch — read frame by frame
                        let frames = data.len() / out_ch;
                        let mut frame_buf = vec![0.0f32; in_ch];
                        for f in 0..frames {
                            let n = out_consumer.pop_slice(&mut frame_buf);
                            for ch in 0..out_ch {
                                data[f * out_ch + ch] = if n > 0 && ch < in_ch {
                                    frame_buf[ch]
                                } else {
                                    0.0
                                };
                            }
                        }
                    }
                },
                move |err| {
                    tracing::error!("Audio output stream error: {}", err);
                },
                None,
            )
            .map_err(|e| format!("Failed to build output stream: {}", e))?;

        output_stream
            .play()
            .map_err(|e| format!("Failed to start output stream: {}", e))?;

        let guard = MonitorGuard {
            running,
            _output_stream: output_stream,
        };

        Ok((guard, monitor_producer))
    } else {
        // === Passthrough path (no resampling) ===
        let rb = HeapRb::<f32>::new(MONITOR_BUFFER_FRAMES * input_channels as usize);
        let (monitor_producer, mut consumer) = rb.split();

        let out_ch = output_channels as usize;
        let in_ch = input_channels as usize;

        let output_stream = output_device
            .build_output_stream(
                &output_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    if out_ch == in_ch {
                        let n = consumer.pop_slice(data);
                        for s in &mut data[n..] {
                            *s = 0.0;
                        }
                    } else {
                        let frames = data.len() / out_ch;
                        let mut frame_buf = vec![0.0f32; in_ch];
                        for f in 0..frames {
                            let n = consumer.pop_slice(&mut frame_buf);
                            for ch in 0..out_ch {
                                data[f * out_ch + ch] = if n > 0 && ch < in_ch {
                                    frame_buf[ch]
                                } else {
                                    0.0
                                };
                            }
                        }
                    }
                },
                move |err| {
                    tracing::error!("Audio output stream error: {}", err);
                },
                None,
            )
            .map_err(|e| format!("Failed to build output stream: {}", e))?;

        output_stream
            .play()
            .map_err(|e| format!("Failed to start output stream: {}", e))?;

        let guard = MonitorGuard {
            running,
            _output_stream: output_stream,
        };

        Ok((guard, monitor_producer))
    }
}

/// Resampler loop running in a dedicated thread.
/// Reads interleaved input-rate samples, deinterleaves, resamples via rubato::Fft,
/// re-interleaves, and pushes to the output ring buffer.
fn resample_loop(
    mut consumer: ringbuf::HeapCons<f32>,
    mut producer: ringbuf::HeapProd<f32>,
    input_rate: u32,
    output_rate: u32,
    channels: usize,
    running: Arc<AtomicBool>,
) {
    let chunk_size = 1024usize;

    let mut resampler = match rubato::Fft::<f32>::new(
        input_rate as usize,
        output_rate as usize,
        chunk_size,
        1, // sub_chunks
        channels,
        rubato::FixedSync::Input,
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Failed to create resampler: {}", e);
            return;
        }
    };

    let frames_needed = resampler.input_frames_next();
    let samples_needed = frames_needed * channels;
    let mut read_buf = vec![0.0f32; samples_needed];

    // Deinterleaved buffers for rubato
    let mut in_bufs: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0; frames_needed]).collect();
    let out_frames_max = resampler.output_frames_max();
    let mut out_bufs: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0; out_frames_max]).collect();

    // Interleaved output staging
    let mut interleaved_out = Vec::with_capacity(out_frames_max * channels);

    while running.load(Ordering::Relaxed) {
        let available = consumer.occupied_len();
        if available < samples_needed {
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }

        let n = consumer.pop_slice(&mut read_buf[..samples_needed]);
        if n == 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }

        let in_frames = n / channels;

        // Deinterleave
        for ch in 0..channels {
            for f in 0..frames_needed {
                in_bufs[ch][f] = if f < in_frames {
                    read_buf[f * channels + ch]
                } else {
                    0.0
                };
            }
        }

        // Resample via audioadapter wrappers
        let in_adapter = match SequentialSliceOfVecs::new(&in_bufs[..], channels, frames_needed) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("Adapter error: {}", e);
                continue;
            }
        };
        let mut out_adapter =
            match SequentialSliceOfVecs::new_mut(&mut out_bufs[..], channels, out_frames_max) {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("Adapter error: {}", e);
                    continue;
                }
            };

        let (_consumed, produced) =
            match resampler.process_into_buffer(&in_adapter, &mut out_adapter, None) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Resample error: {}", e);
                    continue;
                }
            };

        // Re-interleave
        interleaved_out.clear();
        for f in 0..produced {
            for ch in 0..channels {
                interleaved_out.push(out_bufs[ch][f]);
            }
        }

        let _ = producer.push_slice(&interleaved_out);
    }
}

/// Check if the DJI device is connected (audio subsystem)
pub fn is_dji_device_connected() -> bool {
    device::find_dji_audio_device().is_some()
}

/// Check if any matching audio input device or app is available.
/// If device_name is None, checks for DJI device (backwards compat).
pub fn is_device_connected(device_name: Option<&str>) -> bool {
    if device::find_audio_device(device_name).is_some() {
        return true;
    }
    #[cfg(target_os = "macos")]
    if let Some(name) = device_name {
        if device::find_app_source(name).is_some() {
            return true;
        }
    }
    false
}
