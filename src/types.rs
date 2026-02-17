//! Audio capture event types and configuration structs

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A named, addressable audio channel
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Channel {
    /// 0-based channel index within the device
    pub index: u16,
    /// Source device name
    pub device: String,
    /// Descriptive label (e.g. "TX1", "host")
    pub label: String,
}

/// Resolve channel labels for a device.
///
/// - DJI devices → `["TX1", "TX2"]`
/// - Other devices → `["{device_name}:0", "{device_name}:1", ...]`
/// - If `user_labels` provided (comma-separated), use those instead (must match channel count)
pub fn resolve_channel_labels(
    device_name: &str,
    is_dji: bool,
    num_channels: u16,
    user_labels: Option<&str>,
) -> Result<Vec<String>, String> {
    if let Some(labels_str) = user_labels {
        let labels: Vec<String> = labels_str.split(',').map(|s| s.trim().to_string()).collect();
        if labels.len() != num_channels as usize {
            return Err(format!(
                "Expected {} labels but got {}: {:?}",
                num_channels,
                labels.len(),
                labels
            ));
        }
        return Ok(labels);
    }

    if is_dji {
        let dji_labels = ["TX1", "TX2"];
        Ok((0..num_channels as usize)
            .map(|i| {
                if i < dji_labels.len() {
                    dji_labels[i].to_string()
                } else {
                    format!("{}:{}", device_name, i)
                }
            })
            .collect())
    } else {
        Ok((0..num_channels as usize)
            .map(|i| format!("{}:{}", device_name, i))
            .collect())
    }
}

/// Whether an audio source is a hardware device or an application
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Hardware,
    Application,
}

/// Audio device info for enumeration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AudioDevice {
    /// Device name from the OS audio subsystem
    pub name: String,
    /// Number of input channels
    pub channels: u16,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Whether this is the DJI MIC MINI device
    pub is_dji: bool,
    /// Whether this is a hardware device or an application audio source
    pub kind: DeviceKind,
}

/// Events emitted by audio capture plugin methods
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MicEvent {
    /// USB device information
    DeviceInfo {
        vendor_id: u16,
        product_id: u16,
        name: String,
        channels: u16,
        sample_rate: u32,
    },

    /// List of audio input devices
    DeviceList { devices: Vec<AudioDevice> },

    /// Audio capture has started
    CaptureStarted {
        sample_rate: u32,
        channels: u16,
    },

    /// Audio capture has stopped
    CaptureStopped,

    /// Real-time audio level metering
    Level {
        rms_left: f32,
        rms_right: f32,
        peak_left: f32,
        peak_right: f32,
        timestamp_ms: u64,
    },

    /// WAV recording has started
    RecordingStarted { path: String },

    /// WAV recording progress update
    RecordingProgress {
        duration_secs: f32,
        file_size_bytes: u64,
    },

    /// WAV recording has stopped
    RecordingStopped {
        path: String,
        duration_secs: f32,
    },

    /// Live monitoring has started
    MonitorStarted {
        sample_rate: u32,
        channels: u16,
    },

    /// Live monitoring has stopped
    MonitorStopped,

    /// Current plugin status
    Status {
        device_connected: bool,
    },

    /// Live audio data chunk (base64-encoded PCM f32le)
    AudioData {
        /// Base64-encoded interleaved f32le PCM samples
        data: String,
        /// Number of frames in this chunk
        frames: u32,
        /// Number of channels
        channels: u16,
        /// Sample rate in Hz
        sample_rate: u32,
        /// Chunk sequence number
        sequence: u64,
    },

    /// Per-channel probe analysis
    ChannelProbe {
        /// Channel index (0 = left/TX1, 1 = right/TX2)
        channel: u16,
        /// Channel label
        label: String,
        /// RMS level (linear 0.0-1.0)
        rms: f32,
        /// Peak level (linear 0.0-1.0)
        peak: f32,
        /// RMS in dBFS (0 dBFS = full scale)
        rms_dbfs: f32,
        /// Peak in dBFS
        peak_dbfs: f32,
        /// Estimated noise floor in dBFS
        noise_floor_dbfs: f32,
        /// Dynamic range: peak_dbfs - noise_floor_dbfs
        dynamic_range_db: f32,
        /// Whether this channel has meaningful signal (above noise floor)
        has_signal: bool,
        /// DC offset (mean sample value, ideally 0.0)
        dc_offset: f32,
    },

    /// Summary of channel independence analysis
    ProbeSummary {
        /// Total channels analyzed
        channels: u16,
        /// Sample rate used
        sample_rate: u32,
        /// Duration of analysis window in seconds
        analysis_duration_secs: f32,
        /// Pearson correlation between channels (-1.0 to 1.0)
        /// 1.0 = identical (mono duplicated), 0.0 = uncorrelated, -1.0 = inverted
        channel_correlation: f32,
        /// Whether channels appear to carry independent signals
        channels_independent: bool,
        /// Human-readable assessment
        assessment: String,
    },

    /// Enumeration of available channels
    ChannelList { channels: Vec<Channel> },

    /// Per-channel mono audio data (base64-encoded f32le PCM)
    ChannelAudioData {
        /// Channel index (0-based)
        channel: u16,
        /// Channel label
        label: String,
        /// Base64-encoded mono f32le PCM samples
        data: String,
        /// Number of frames in this chunk
        frames: u32,
        /// Sample rate in Hz
        sample_rate: u32,
        /// Sequence number shared across channels for the same tick
        sequence: u64,
    },

    /// Per-channel level metering
    ChannelLevel {
        /// Channel index (0-based)
        channel: u16,
        /// Channel label
        label: String,
        /// RMS level (linear 0.0-1.0)
        rms: f32,
        /// Peak level (linear 0.0-1.0)
        peak: f32,
        /// Timestamp in milliseconds
        timestamp_ms: u64,
    },

    /// Error event
    Error { message: String },
}
