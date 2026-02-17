//! ScreenCaptureKit integration — per-app audio capture on macOS 13+
//!
//! Pushes interleaved f32 PCM into the same HeapProd<f32> ring buffer that
//! cpal uses, so hub methods work transparently with either source.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ringbuf::traits::Producer;
use screencapturekit::prelude::*;

/// Lightweight struct representing a capturable application
#[derive(Debug, Clone)]
pub struct AppSource {
    pub name: String,
    pub bundle_id: String,
    pub pid: i32,
}

/// Enumerate running applications that can be captured via ScreenCaptureKit.
/// Requires Screen Recording permission. On denial, returns an error.
pub fn list_capturable_apps() -> Result<Vec<AppSource>, String> {
    let content = SCShareableContent::get()
        .map_err(|e| format!("Screen Recording permission required to list applications. \
            Grant permission in System Settings → Privacy & Security → Screen Recording, \
            then restart this process. ({})", e))?;

    let mut apps: Vec<AppSource> = content
        .applications()
        .into_iter()
        .filter(|app| !app.application_name().is_empty())
        .map(|app| AppSource {
            name: app.application_name(),
            bundle_id: app.bundle_identifier(),
            pid: app.process_id(),
        })
        .collect();

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps.dedup_by(|a, b| a.pid == b.pid);
    Ok(apps)
}

/// Guard that stops SCK capture on drop. Holds the SCStream alive.
pub struct AppCaptureGuard {
    stream: SCStream,
}

unsafe impl Send for AppCaptureGuard {}

impl Drop for AppCaptureGuard {
    fn drop(&mut self) {
        let _ = self.stream.stop_capture();
    }
}

/// Reinterpret a raw byte slice from an AudioBuffer as f32 samples.
fn bytes_as_f32(bytes: &[u8]) -> &[f32] {
    if bytes.is_empty() {
        return &[];
    }
    unsafe {
        std::slice::from_raw_parts(
            bytes.as_ptr() as *const f32,
            bytes.len() / std::mem::size_of::<f32>(),
        )
    }
}

/// Extract interleaved f32 samples from a CMSampleBuffer and push into a ring buffer producer.
///
/// SCK may deliver audio as either:
/// - 1 buffer with interleaved stereo (number_channels == 2)
/// - 2 separate mono buffers (number_channels == 1 each) — planar layout
///
/// We always push interleaved data into the ring buffer.
fn push_audio_samples(sample: &CMSampleBuffer, producer: &Arc<Mutex<ringbuf::HeapProd<f32>>>) {
    let Some(audio_buffers) = sample.audio_buffer_list() else { return };
    let num_buffers = audio_buffers.num_buffers();
    if num_buffers == 0 { return; }

    let Ok(mut p) = producer.lock() else { return };

    // Check if this is planar audio (multiple mono buffers that need interleaving)
    let first = match audio_buffers.get(0) {
        Some(b) => b,
        None => return,
    };

    if num_buffers >= 2 && first.number_channels == 1 {
        // Planar: each buffer is one channel, interleave them
        let ch0 = bytes_as_f32(first.data());
        let ch1 = audio_buffers.get(1)
            .map(|b| bytes_as_f32(b.data()))
            .unwrap_or(&[]);
        let frames = ch0.len().max(ch1.len());
        // Pre-allocate interleaved buffer
        let mut interleaved = Vec::with_capacity(frames * num_buffers);
        for f in 0..frames {
            interleaved.push(if f < ch0.len() { ch0[f] } else { 0.0 });
            interleaved.push(if f < ch1.len() { ch1[f] } else { 0.0 });
        }
        let _ = p.push_slice(&interleaved);
    } else {
        // Already interleaved (or single mono buffer) — push directly
        for i in 0..num_buffers {
            let Some(buffer) = audio_buffers.get(i) else { continue };
            let samples = bytes_as_f32(buffer.data());
            if !samples.is_empty() {
                let _ = p.push_slice(samples);
            }
        }
    }
}

/// Build an SCContentFilter + SCStreamConfiguration for app audio capture.
fn build_app_filter_and_config(
    app: &AppSource,
) -> Result<(SCContentFilter, SCStreamConfiguration), String> {
    let content = SCShareableContent::get()
        .map_err(|e| format!("Screen Recording permission required. \
            Grant permission in System Settings → Privacy & Security → Screen Recording, \
            then restart this process. ({})", e))?;

    // Find the display (required for content filter)
    let display = content.displays().into_iter().next()
        .ok_or_else(|| "No display found for ScreenCaptureKit".to_string())?;

    // Re-find the app by PID in fresh content
    let sck_app = content.applications().into_iter()
        .find(|a| a.process_id() == app.pid)
        .ok_or_else(|| format!("Application '{}' (PID {}) is no longer running", app.name, app.pid))?;

    // Content filter: capture only this app's audio
    let filter = SCContentFilter::create()
        .with_display(&display)
        .with_including_applications(&[&sck_app], &[])
        .build();

    // Minimal video (2x2) + audio at 48kHz stereo
    let config = SCStreamConfiguration::new()
        .with_width(2)
        .with_height(2)
        .with_captures_audio(true)
        .with_sample_rate(48000)
        .with_channel_count(2);

    Ok((filter, config))
}

/// Start capturing audio from a specific application.
/// Pushes interleaved f32 samples into the ring buffer producer.
pub fn start_app_capture(
    app: &AppSource,
    producer: ringbuf::HeapProd<f32>,
    _error_flag: Arc<AtomicBool>,
) -> Result<AppCaptureGuard, String> {
    let (filter, config) = build_app_filter_and_config(app)?;

    let producer = Arc::new(Mutex::new(producer));
    let mut stream = SCStream::new(&filter, &config);

    let prod = producer.clone();
    stream.add_output_handler(
        move |sample: CMSampleBuffer, output_type: SCStreamOutputType| {
            if output_type != SCStreamOutputType::Audio {
                return;
            }
            push_audio_samples(&sample, &prod);
        },
        SCStreamOutputType::Audio,
    );

    stream.start_capture()
        .map_err(|e| format!("Failed to start app audio capture: {}", e))?;

    Ok(AppCaptureGuard { stream })
}

/// Start capturing audio from an app into TWO ring buffer producers (metering + monitoring).
pub fn start_app_capture_dual(
    app: &AppSource,
    producer_a: ringbuf::HeapProd<f32>,
    producer_b: ringbuf::HeapProd<f32>,
    _error_flag: Arc<AtomicBool>,
) -> Result<AppCaptureGuard, String> {
    let (filter, config) = build_app_filter_and_config(app)?;

    let producer_a = Arc::new(Mutex::new(producer_a));
    let producer_b = Arc::new(Mutex::new(producer_b));
    let mut stream = SCStream::new(&filter, &config);

    let prod_a = producer_a.clone();
    let prod_b = producer_b.clone();
    stream.add_output_handler(
        move |sample: CMSampleBuffer, output_type: SCStreamOutputType| {
            if output_type != SCStreamOutputType::Audio {
                return;
            }
            push_audio_samples(&sample, &prod_a);
            push_audio_samples(&sample, &prod_b);
        },
        SCStreamOutputType::Audio,
    );

    stream.start_capture()
        .map_err(|e| format!("Failed to start app audio capture: {}", e))?;

    Ok(AppCaptureGuard { stream })
}
