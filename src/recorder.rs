//! WAV file recording wrapper using hound

use std::path::Path;

use hound::{WavSpec, WavWriter};

/// WAV recorder that writes f32 PCM samples to a file
pub struct WavRecorder {
    writer: WavWriter<std::io::BufWriter<std::fs::File>>,
    frames_written: u64,
    sample_rate: u32,
    channels: u16,
    path: String,
}

impl WavRecorder {
    /// Create a new WAV recorder at the given path
    pub fn new(path: &str, sample_rate: u32, channels: u16) -> Result<Self, String> {
        // Ensure parent directory exists
        if let Some(parent) = Path::new(path).parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create directory {}: {}", parent.display(), e))?;
            }
        }

        let spec = WavSpec {
            channels,
            sample_rate,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };

        let writer = WavWriter::create(path, spec)
            .map_err(|e| format!("Failed to create WAV file '{}': {}", path, e))?;

        Ok(Self {
            writer,
            frames_written: 0,
            sample_rate,
            channels,
            path: path.to_string(),
        })
    }

    /// Write interleaved f32 samples to the WAV file
    pub fn write_samples(&mut self, samples: &[f32]) -> Result<(), String> {
        for &sample in samples {
            self.writer
                .write_sample(sample)
                .map_err(|e| format!("WAV write error: {}", e))?;
        }
        self.frames_written += samples.len() as u64 / self.channels as u64;
        Ok(())
    }

    /// Duration of recorded audio in seconds
    pub fn duration_secs(&self) -> f32 {
        self.frames_written as f32 / self.sample_rate as f32
    }

    /// Approximate file size in bytes (header + raw samples)
    pub fn file_size_bytes(&self) -> u64 {
        // WAV header is 44 bytes, each sample is 4 bytes (f32)
        44 + self.frames_written * self.channels as u64 * 4
    }

    /// Path to the WAV file
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Finalize the WAV file (writes header with correct length)
    pub fn finalize(self) -> Result<(String, f32), String> {
        let path = self.path.clone();
        let duration = self.duration_secs();
        self.writer
            .finalize()
            .map_err(|e| format!("Failed to finalize WAV file: {}", e))?;
        Ok((path, duration))
    }
}
