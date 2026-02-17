//! MicHub — Substrate plugin for audio capture, recording,
//! metering, and live audio streaming via Plexus RPC.
//!
//! Stateless hub: each streaming method creates and owns its audio resources.
//! Resources are dropped automatically when the stream closes — no leaks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_stream::stream;
use base64::Engine;
use futures::Stream;
use ringbuf::traits::{Consumer, Observer};

use crate::capture;
use crate::device;
use crate::meter;
use crate::recorder::WavRecorder;
use crate::types::{Channel, MicEvent, resolve_channel_labels};

/// Audio capture Substrate plugin hub — stateless, Clone is trivial
#[derive(Clone)]
pub struct MicHub;

impl MicHub {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MicHub {
    fn default() -> Self {
        Self::new()
    }
}

#[plexus_macros::hub_methods(
    namespace = "mic",
    version = "0.1.0",
    description = "Audio capture, recording, metering, and live streaming",
    crate_path = "plexus_core"
)]
impl MicHub {
    /// Show USB device info for the connected DJI MIC MINI
    #[plexus_macros::hub_method(
        description = "Get USB device info (vendor/product ID, name) for a connected DJI MIC"
    )]
    pub async fn info(&self) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            match device::find_dji_usb_device() {
                Some(usb) => {
                    let (channels, sample_rate) = device::find_dji_audio_device()
                        .and_then(|d| {
                            device::preferred_stream_config(&d)
                                .map(|c| (c.channels, c.sample_rate.0))
                        })
                        .unwrap_or((2, 48000));

                    yield MicEvent::DeviceInfo {
                        vendor_id: usb.vendor_id,
                        product_id: usb.product_id,
                        name: usb.name,
                        channels,
                        sample_rate,
                    };
                }
                None => {
                    yield MicEvent::Error {
                        message: "DJI MIC MINI USB device not found".to_string(),
                    };
                }
            }
        }
    }

    /// List all audio input devices, highlighting DJI devices
    #[plexus_macros::hub_method(
        description = "List all audio input devices — DJI devices are flagged with is_dji: true"
    )]
    pub async fn list_devices(&self) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            let devices = device::list_audio_input_devices();
            yield MicEvent::DeviceList { devices };
        }
    }

    /// Record audio to a WAV file, streaming progress and levels until timeout or client disconnect
    #[plexus_macros::hub_method(
        streaming,
        description = "Record audio to a WAV file. Streams progress events. Stops after timeout (if set) or when client disconnects.",
        params(
            path = "Output WAV file path (e.g., /tmp/recording.wav)",
            device = "Audio device name substring (default: auto-detect)",
            timeout = "Recording duration limit in milliseconds (default: unlimited — records until client disconnects)"
        )
    )]
    pub async fn record(
        &self,
        path: String,
        device: Option<String>,
        timeout: Option<u64>,
    ) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            // 1. Open audio source (hardware or app)
            let source = match capture::open_audio_source(device.as_deref()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            let sample_rate = source.sample_rate();
            let channels = source.channels();

            // 2. Create ring buffer
            let (producer, mut consumer) = capture::create_ring_buffer(channels);

            // 3. Build + start capture stream
            let error_flag = Arc::new(AtomicBool::new(false));
            let _capture = match capture::build_capture_stream(&source, producer, error_flag.clone()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            // 4. Create WAV writer
            let mut recorder = match WavRecorder::new(&path, sample_rate, channels) {
                Ok(r) => r,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            yield MicEvent::RecordingStarted { path: path.clone() };

            // 5. Stream recording progress
            let mut buf = Vec::with_capacity(48000);
            let mut last_progress = tokio::time::Instant::now();
            let record_start = tokio::time::Instant::now();
            let timeout_duration = timeout.map(tokio::time::Duration::from_millis);

            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

                // Check for device disconnect
                if error_flag.load(Ordering::Relaxed) {
                    yield MicEvent::Error { message: "Audio device disconnected".into() };
                    break;
                }

                // Check timeout
                if let Some(limit) = timeout_duration {
                    if record_start.elapsed() >= limit {
                        break;
                    }
                }

                let available = consumer.occupied_len();
                if available == 0 { continue; }
                buf.resize(available, 0.0);
                let n = consumer.pop_slice(&mut buf[..available]);
                if n == 0 { continue; }

                if let Err(e) = recorder.write_samples(&buf[..n]) {
                    yield MicEvent::Error { message: e };
                    break;
                }

                let levels = meter::compute_levels(&buf[..n], channels);
                yield MicEvent::Level {
                    rms_left: levels.rms_left,
                    rms_right: levels.rms_right,
                    peak_left: levels.peak_left,
                    peak_right: levels.peak_right,
                    timestamp_ms: (recorder.duration_secs() * 1000.0) as u64,
                };

                if last_progress.elapsed() >= tokio::time::Duration::from_secs(1) {
                    last_progress = tokio::time::Instant::now();
                    yield MicEvent::RecordingProgress {
                        duration_secs: recorder.duration_secs(),
                        file_size_bytes: recorder.file_size_bytes(),
                    };
                }
            }

            // 6. Finalize — resources (_input_stream, consumer, ring buffer) dropped after this
            match recorder.finalize() {
                Ok((final_path, duration)) => {
                    yield MicEvent::RecordingStopped {
                        path: final_path,
                        duration_secs: duration,
                    };
                }
                Err(e) => {
                    yield MicEvent::Error { message: e };
                }
            }
        }
    }

    /// Stream real-time audio levels without recording
    #[plexus_macros::hub_method(
        streaming,
        description = "Stream real-time RMS + peak audio levels. Stops when client disconnects.",
        params(
            device = "Audio device name substring (default: auto-detect)"
        )
    )]
    pub async fn levels(
        &self,
        device: Option<String>,
    ) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            let source = match capture::open_audio_source(device.as_deref()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            let channels = source.channels();
            let (producer, mut consumer) = capture::create_ring_buffer(channels);

            let error_flag = Arc::new(AtomicBool::new(false));
            let _capture = match capture::build_capture_stream(&source, producer, error_flag.clone()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            let mut buf = Vec::with_capacity(48000);
            let mut timestamp_ms: u64 = 0;

            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                timestamp_ms += 50;

                if error_flag.load(Ordering::Relaxed) {
                    yield MicEvent::Error { message: "Audio device disconnected".into() };
                    break;
                }

                let available = consumer.occupied_len();
                if available == 0 { continue; }
                buf.resize(available, 0.0);
                let n = consumer.pop_slice(&mut buf[..available]);
                if n == 0 { continue; }

                let levels = meter::compute_levels(&buf[..n], channels);
                yield MicEvent::Level {
                    rms_left: levels.rms_left,
                    rms_right: levels.rms_right,
                    peak_left: levels.peak_left,
                    peak_right: levels.peak_right,
                    timestamp_ms,
                };
            }
        }
    }

    /// Stream live audio data as base64-encoded PCM chunks
    #[plexus_macros::hub_method(
        streaming,
        description = "Stream live audio data as base64-encoded f32le PCM chunks. Stops when client disconnects.",
        params(
            device = "Audio device name substring (default: auto-detect)",
            chunk_ms = "Chunk duration in milliseconds (default: 50, range: 10-500)"
        )
    )]
    pub async fn stream_audio(
        &self,
        device: Option<String>,
        chunk_ms: Option<u32>,
    ) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            let source = match capture::open_audio_source(device.as_deref()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            let sample_rate = source.sample_rate();
            let channels = source.channels();
            let chunk_duration = chunk_ms.unwrap_or(50).clamp(10, 500);

            let (producer, mut consumer) = capture::create_ring_buffer(channels);

            let error_flag = Arc::new(AtomicBool::new(false));
            let _capture = match capture::build_capture_stream(&source, producer, error_flag.clone()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            yield MicEvent::CaptureStarted { sample_rate, channels };

            let mut buf = Vec::with_capacity(48000);
            let mut sequence: u64 = 0;

            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(chunk_duration as u64)).await;

                if error_flag.load(Ordering::Relaxed) {
                    yield MicEvent::Error { message: "Audio device disconnected".into() };
                    break;
                }

                let available = consumer.occupied_len();
                if available == 0 { continue; }
                buf.resize(available, 0.0);
                let n = consumer.pop_slice(&mut buf[..available]);
                if n == 0 { continue; }

                let byte_buf: Vec<u8> = buf[..n]
                    .iter()
                    .flat_map(|s| s.to_le_bytes())
                    .collect();

                let encoded = base64::engine::general_purpose::STANDARD.encode(&byte_buf);
                let frames = n as u32 / channels as u32;

                yield MicEvent::AudioData {
                    data: encoded,
                    frames,
                    channels,
                    sample_rate,
                    sequence,
                };

                sequence += 1;
            }
        }
    }

    /// Probe individual channels with detailed analysis
    #[plexus_macros::hub_method(
        description = "Capture a short audio window and analyze each channel independently. \
                       Reports dBFS levels, noise floor, dynamic range, DC offset, and whether \
                       channels carry independent signals or are duplicated mono.",
        params(
            device = "Audio device name substring (default: auto-detect)",
            duration_ms = "Analysis window duration in milliseconds (default: 1000, range: 200-5000)"
        )
    )]
    pub async fn probe(
        &self,
        device: Option<String>,
        duration_ms: Option<u32>,
    ) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            let source = match capture::open_audio_source(device.as_deref()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            let sample_rate = source.sample_rate();
            let channels = source.channels();

            let (producer, mut consumer) = capture::create_ring_buffer(channels);

            let error_flag = Arc::new(AtomicBool::new(false));
            let _capture = match capture::build_capture_stream(&source, producer, error_flag.clone()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            let duration = duration_ms.unwrap_or(1000).clamp(200, 5000);
            let total_samples = (sample_rate as usize * channels as usize * duration as usize) / 1000;

            // Collect samples over the analysis window
            let mut all_samples = Vec::with_capacity(total_samples);
            let mut buf = Vec::with_capacity(48000);
            let collect_start = tokio::time::Instant::now();
            let target_duration = tokio::time::Duration::from_millis(duration as u64);

            while collect_start.elapsed() < target_duration {
                tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;

                if error_flag.load(Ordering::Relaxed) {
                    yield MicEvent::Error { message: "Audio device disconnected".into() };
                    return;
                }

                let available = consumer.occupied_len();
                if available == 0 { continue; }
                buf.resize(available, 0.0);
                let n = consumer.pop_slice(&mut buf[..available]);
                if n > 0 {
                    all_samples.extend_from_slice(&buf[..n]);
                }
            }

            if all_samples.is_empty() {
                yield MicEvent::Error { message: "No audio samples captured during probe window".to_string() };
                return;
            }

            let actual_duration_secs = all_samples.len() as f32 / (sample_rate as f32 * channels as f32);

            // Resolve channel labels
            let dev_name = source.name();
            let is_dji = source.is_dji();
            let channel_labels = resolve_channel_labels(&dev_name, is_dji, channels, None)
                .unwrap_or_else(|_| (0..channels).map(|i| format!("channel {}", i)).collect());

            let mut channel_data: Vec<Vec<f32>> = Vec::new();
            for ch in 0..channels as usize {
                let ch_samples = meter::deinterleave_channel(&all_samples, ch, channels as usize);
                let analysis = meter::analyze_channel(&ch_samples);

                let label = channel_labels[ch].clone();

                yield MicEvent::ChannelProbe {
                    channel: ch as u16,
                    label,
                    rms: analysis.rms,
                    peak: analysis.peak,
                    rms_dbfs: analysis.rms_dbfs,
                    peak_dbfs: analysis.peak_dbfs,
                    noise_floor_dbfs: analysis.noise_floor_dbfs,
                    dynamic_range_db: analysis.dynamic_range_db,
                    has_signal: analysis.has_signal,
                    dc_offset: analysis.dc_offset,
                };

                channel_data.push(ch_samples);
            }

            let (correlation, independent, assessment) = if channels >= 2 && channel_data.len() >= 2 {
                let corr = meter::channel_correlation(&channel_data[0], &channel_data[1]);
                let indep = corr.abs() < 0.95;
                let msg = if corr.abs() > 0.99 {
                    "Channels are identical — mono signal duplicated across both channels. Only one transmitter active or mic in mono mode.".to_string()
                } else if corr.abs() > 0.95 {
                    format!("Channels are highly correlated ({:.3}) — likely the same source with minor differences.", corr)
                } else if corr.abs() > 0.5 {
                    format!("Channels are moderately correlated ({:.3}) — partially independent signals, some crosstalk.", corr)
                } else {
                    format!("Channels are independent ({:.3}) — carrying distinct audio.", corr)
                };
                (corr, indep, msg)
            } else {
                (1.0, false, "Single channel device — no independence analysis.".to_string())
            };

            yield MicEvent::ProbeSummary {
                channels,
                sample_rate,
                analysis_duration_secs: actual_duration_secs,
                channel_correlation: correlation,
                channels_independent: independent,
                assessment,
            };

            // _input_stream and ring buffer dropped here
        }
    }

    /// Live audio monitoring — plays captured audio through the default output device
    #[plexus_macros::hub_method(
        streaming,
        description = "Play captured audio live through the system output. \
                       Streams level events while monitoring. Stops when client disconnects.",
        params(
            device = "Audio device name substring (default: auto-detect)"
        )
    )]
    pub async fn monitor(
        &self,
        device: Option<String>,
    ) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            // 1. Open audio source (hardware or app)
            let source = match capture::open_audio_source(device.as_deref()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            let sample_rate = source.sample_rate();
            let channels = source.channels();

            // 2. Create ring buffer for metering (separate from monitor)
            let (meter_producer, mut meter_consumer) = capture::create_ring_buffer(channels);

            // 3. Build monitor output pipeline (resampler + output stream)
            let (_monitor_guard, monitor_producer) = match capture::build_monitor_output(sample_rate, channels) {
                Ok(mp) => mp,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            // 4. Build dual capture stream — pushes to BOTH meter and monitor ring buffers
            let error_flag = Arc::new(AtomicBool::new(false));
            let _capture = match capture::build_dual_capture_stream(
                &source, meter_producer, monitor_producer, error_flag.clone()
            ) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            yield MicEvent::MonitorStarted { sample_rate, channels };

            // 5. Stream level events while monitoring
            let mut buf = Vec::with_capacity(48000);
            let mut timestamp_ms: u64 = 0;

            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                timestamp_ms += 50;

                if error_flag.load(Ordering::Relaxed) {
                    yield MicEvent::Error { message: "Audio device disconnected".into() };
                    break;
                }

                let available = meter_consumer.occupied_len();
                if available == 0 { continue; }
                buf.resize(available, 0.0);
                let n = meter_consumer.pop_slice(&mut buf[..available]);
                if n == 0 { continue; }

                let levels = meter::compute_levels(&buf[..n], channels);
                yield MicEvent::Level {
                    rms_left: levels.rms_left,
                    rms_right: levels.rms_right,
                    peak_left: levels.peak_left,
                    peak_right: levels.peak_right,
                    timestamp_ms,
                };
            }

            // _input_stream, _monitor_guard (stops resampler thread + output stream) all dropped here
        }
    }

    /// Enumerate available audio channels with labels
    #[plexus_macros::hub_method(
        description = "List available audio channels with descriptive labels. DJI channels are labeled TX1/TX2 by default.",
        params(
            device = "Audio device name substring (default: auto-detect)",
            labels = "Comma-separated custom labels (e.g. 'host,guest') — must match channel count"
        )
    )]
    pub async fn list_channels(
        &self,
        device: Option<String>,
        labels: Option<String>,
    ) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            let devices = if let Some(ref query) = device {
                // Filter to matching device
                device::list_audio_input_devices()
                    .into_iter()
                    .filter(|d| d.name.to_lowercase().contains(&query.to_lowercase()))
                    .collect::<Vec<_>>()
            } else {
                // Auto-detect: prefer DJI, fall back to all
                let all = device::list_audio_input_devices();
                let dji: Vec<_> = all.iter().filter(|d| d.is_dji).cloned().collect();
                if dji.is_empty() { all } else { dji }
            };

            let mut channels = Vec::new();
            for dev in &devices {
                let resolved = match resolve_channel_labels(
                    &dev.name,
                    dev.is_dji,
                    dev.channels,
                    labels.as_deref(),
                ) {
                    Ok(l) => l,
                    Err(e) => { yield MicEvent::Error { message: e }; return; }
                };

                for (i, label) in resolved.into_iter().enumerate() {
                    channels.push(Channel {
                        index: i as u16,
                        device: dev.name.clone(),
                        label,
                    });
                }
            }

            yield MicEvent::ChannelList { channels };
        }
    }

    /// Stream per-channel deinterleaved audio with level metering
    #[plexus_macros::hub_method(
        streaming,
        description = "Stream per-channel mono audio data with level metering. Each tick emits ChannelAudioData + ChannelLevel per selected channel. Stops when client disconnects.",
        params(
            device = "Audio device name substring (default: auto-detect)",
            channels = "Comma-separated channel indices to stream (e.g. '0' or '0,1') — default: all",
            labels = "Comma-separated custom labels (e.g. 'host,guest') — must match device channel count",
            chunk_ms = "Chunk duration in milliseconds (default: 50, range: 10-500)"
        )
    )]
    pub async fn stream_channels(
        &self,
        device: Option<String>,
        channels: Option<String>,
        labels: Option<String>,
        chunk_ms: Option<u32>,
    ) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            let source = match capture::open_audio_source(device.as_deref()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            let sample_rate = source.sample_rate();
            let num_channels = source.channels();
            let chunk_duration = chunk_ms.unwrap_or(50).clamp(10, 500);

            // Resolve labels
            let dev_name = source.name();
            let is_dji = source.is_dji();
            let channel_labels = match resolve_channel_labels(&dev_name, is_dji, num_channels, labels.as_deref()) {
                Ok(l) => l,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            // Parse selected channel indices
            let selected: Vec<u16> = if let Some(ref ch_str) = channels {
                let mut indices = Vec::new();
                for part in ch_str.split(',') {
                    match part.trim().parse::<u16>() {
                        Ok(idx) if idx < num_channels => indices.push(idx),
                        Ok(idx) => {
                            yield MicEvent::Error {
                                message: format!("Channel index {} out of range (device has {} channels)", idx, num_channels),
                            };
                            return;
                        }
                        Err(_) => {
                            yield MicEvent::Error {
                                message: format!("Invalid channel index: '{}'", part.trim()),
                            };
                            return;
                        }
                    }
                }
                indices
            } else {
                (0..num_channels).collect()
            };

            // Open audio
            let (producer, mut consumer) = capture::create_ring_buffer(num_channels);

            let error_flag = Arc::new(AtomicBool::new(false));
            let _capture = match capture::build_capture_stream(&source, producer, error_flag.clone()) {
                Ok(s) => s,
                Err(e) => { yield MicEvent::Error { message: e }; return; }
            };

            yield MicEvent::CaptureStarted { sample_rate, channels: num_channels };

            let mut buf = Vec::with_capacity(48000);
            let mut sequence: u64 = 0;
            let mut timestamp_ms: u64 = 0;

            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(chunk_duration as u64)).await;
                timestamp_ms += chunk_duration as u64;

                if error_flag.load(Ordering::Relaxed) {
                    yield MicEvent::Error { message: "Audio device disconnected".into() };
                    break;
                }

                let available = consumer.occupied_len();
                if available == 0 { continue; }
                buf.resize(available, 0.0);
                let n = consumer.pop_slice(&mut buf[..available]);
                if n == 0 { continue; }

                // For each selected channel: deinterleave, encode, meter
                for &ch_idx in &selected {
                    let ch_samples = meter::deinterleave_channel(&buf[..n], ch_idx as usize, num_channels as usize);
                    let label = channel_labels[ch_idx as usize].clone();

                    // Encode mono PCM as base64
                    let byte_buf: Vec<u8> = ch_samples.iter()
                        .flat_map(|s| s.to_le_bytes())
                        .collect();
                    let encoded = base64::engine::general_purpose::STANDARD.encode(&byte_buf);
                    let frames = ch_samples.len() as u32;

                    yield MicEvent::ChannelAudioData {
                        channel: ch_idx,
                        label: label.clone(),
                        data: encoded,
                        frames,
                        sample_rate,
                        sequence,
                    };

                    // Per-channel level
                    let level = meter::compute_channel_level(&ch_samples);
                    yield MicEvent::ChannelLevel {
                        channel: ch_idx,
                        label,
                        rms: level.rms,
                        peak: level.peak,
                        timestamp_ms,
                    };
                }

                sequence += 1;
            }
        }
    }

    /// Get current plugin status
    #[plexus_macros::hub_method(
        description = "Check if an audio input device is connected",
        params(
            device = "Audio device name substring (default: auto-detect)"
        )
    )]
    pub async fn status(
        &self,
        device: Option<String>,
    ) -> impl Stream<Item = MicEvent> + Send + 'static {
        stream! {
            yield MicEvent::Status {
                device_connected: capture::is_device_connected(device.as_deref()),
            };
        }
    }
}
