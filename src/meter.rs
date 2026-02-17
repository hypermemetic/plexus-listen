//! Real-time RMS and peak level metering

/// Audio level measurements for a single metering window
#[derive(Debug, Clone, Copy)]
pub struct LevelReading {
    pub rms_left: f32,
    pub rms_right: f32,
    pub peak_left: f32,
    pub peak_right: f32,
}

/// Compute RMS and peak levels from interleaved stereo f32 samples
pub fn compute_levels(samples: &[f32], channels: u16) -> LevelReading {
    if samples.is_empty() || channels == 0 {
        return LevelReading {
            rms_left: 0.0,
            rms_right: 0.0,
            peak_left: 0.0,
            peak_right: 0.0,
        };
    }

    let channels = channels as usize;

    let mut sum_sq_left = 0.0f64;
    let mut sum_sq_right = 0.0f64;
    let mut peak_left = 0.0f32;
    let mut peak_right = 0.0f32;
    let mut count_left = 0u64;
    let mut count_right = 0u64;

    for (i, &sample) in samples.iter().enumerate() {
        let ch = i % channels;
        let abs = sample.abs();

        if ch == 0 {
            sum_sq_left += (sample as f64) * (sample as f64);
            if abs > peak_left {
                peak_left = abs;
            }
            count_left += 1;
        } else if ch == 1 {
            sum_sq_right += (sample as f64) * (sample as f64);
            if abs > peak_right {
                peak_right = abs;
            }
            count_right += 1;
        }
    }

    let rms_left = if count_left > 0 {
        (sum_sq_left / count_left as f64).sqrt() as f32
    } else {
        0.0
    };

    let rms_right = if count_right > 0 {
        (sum_sq_right / count_right as f64).sqrt() as f32
    } else {
        0.0
    };

    // For mono sources, mirror left to right
    if channels == 1 {
        return LevelReading {
            rms_left,
            rms_right: rms_left,
            peak_left,
            peak_right: peak_left,
        };
    }

    LevelReading {
        rms_left,
        rms_right,
        peak_left,
        peak_right,
    }
}

/// Lightweight per-channel level reading (RMS + peak only)
#[derive(Debug, Clone, Copy)]
pub struct ChannelLevelReading {
    pub rms: f32,
    pub peak: f32,
}

/// Compute RMS and peak levels from mono (single-channel) f32 samples.
/// Single-pass, no allocations.
pub fn compute_channel_level(samples: &[f32]) -> ChannelLevelReading {
    if samples.is_empty() {
        return ChannelLevelReading { rms: 0.0, peak: 0.0 };
    }

    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;

    for &s in samples {
        sum_sq += (s as f64) * (s as f64);
        let abs = s.abs();
        if abs > peak {
            peak = abs;
        }
    }

    let rms = (sum_sq / samples.len() as f64).sqrt() as f32;

    ChannelLevelReading { rms, peak }
}

/// Per-channel analysis result
#[derive(Debug, Clone)]
pub struct ChannelAnalysis {
    pub rms: f32,
    pub peak: f32,
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
    pub noise_floor_dbfs: f32,
    pub dynamic_range_db: f32,
    pub has_signal: bool,
    pub dc_offset: f32,
}

/// Convert linear amplitude to dBFS (0 dBFS = 1.0 linear)
fn to_dbfs(linear: f32) -> f32 {
    if linear <= 0.0 {
        -120.0 // floor
    } else {
        20.0 * linear.log10()
    }
}

/// Analyze a single channel's samples
pub fn analyze_channel(samples: &[f32]) -> ChannelAnalysis {
    if samples.is_empty() {
        return ChannelAnalysis {
            rms: 0.0,
            peak: 0.0,
            rms_dbfs: -120.0,
            peak_dbfs: -120.0,
            noise_floor_dbfs: -120.0,
            dynamic_range_db: 0.0,
            has_signal: false,
            dc_offset: 0.0,
        };
    }

    let n = samples.len() as f64;

    // DC offset (mean)
    let sum: f64 = samples.iter().map(|&s| s as f64).sum();
    let dc_offset = (sum / n) as f32;

    // RMS and peak
    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f32;
    for &s in samples {
        sum_sq += (s as f64) * (s as f64);
        let abs = s.abs();
        if abs > peak {
            peak = abs;
        }
    }
    let rms = (sum_sq / n).sqrt() as f32;

    // Noise floor estimation: sort absolute values, take the median of the
    // quietest 20% as the noise floor estimate
    let mut sorted_abs: Vec<f32> = samples.iter().map(|s| s.abs()).collect();
    sorted_abs.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let quiet_end = (sorted_abs.len() as f64 * 0.2) as usize;
    let quiet_end = quiet_end.max(1);
    let noise_sum: f64 = sorted_abs[..quiet_end].iter().map(|&s| (s as f64) * (s as f64)).sum();
    let noise_rms = (noise_sum / quiet_end as f64).sqrt() as f32;

    let rms_dbfs = to_dbfs(rms);
    let peak_dbfs = to_dbfs(peak);
    let noise_floor_dbfs = to_dbfs(noise_rms);
    let dynamic_range_db = peak_dbfs - noise_floor_dbfs;

    // Signal detection: is RMS more than 6dB above noise floor?
    let has_signal = rms_dbfs > noise_floor_dbfs + 6.0;

    ChannelAnalysis {
        rms,
        peak,
        rms_dbfs,
        peak_dbfs,
        noise_floor_dbfs,
        dynamic_range_db,
        has_signal,
        dc_offset,
    }
}

/// Extract samples for a single channel from interleaved audio
pub fn deinterleave_channel(samples: &[f32], channel: usize, total_channels: usize) -> Vec<f32> {
    samples.iter()
        .skip(channel)
        .step_by(total_channels)
        .copied()
        .collect()
}

/// Compute Pearson correlation coefficient between two sample vectors
pub fn channel_correlation(ch0: &[f32], ch1: &[f32]) -> f32 {
    let n = ch0.len().min(ch1.len());
    if n == 0 {
        return 0.0;
    }

    let n_f = n as f64;
    let mean0: f64 = ch0[..n].iter().map(|&s| s as f64).sum::<f64>() / n_f;
    let mean1: f64 = ch1[..n].iter().map(|&s| s as f64).sum::<f64>() / n_f;

    let mut cov = 0.0f64;
    let mut var0 = 0.0f64;
    let mut var1 = 0.0f64;

    for i in 0..n {
        let d0 = ch0[i] as f64 - mean0;
        let d1 = ch1[i] as f64 - mean1;
        cov += d0 * d1;
        var0 += d0 * d0;
        var1 += d1 * d1;
    }

    let denom = (var0 * var1).sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        (cov / denom) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_silence() {
        let samples = vec![0.0f32; 960]; // 10ms at 48kHz stereo
        let levels = compute_levels(&samples, 2);
        assert_eq!(levels.rms_left, 0.0);
        assert_eq!(levels.rms_right, 0.0);
        assert_eq!(levels.peak_left, 0.0);
        assert_eq!(levels.peak_right, 0.0);
    }

    #[test]
    fn test_full_scale() {
        // Constant 1.0 on left, 0.5 on right (interleaved stereo)
        let mut samples = Vec::new();
        for _ in 0..480 {
            samples.push(1.0f32);
            samples.push(0.5f32);
        }
        let levels = compute_levels(&samples, 2);
        assert!((levels.rms_left - 1.0).abs() < 0.001);
        assert!((levels.rms_right - 0.5).abs() < 0.001);
        assert!((levels.peak_left - 1.0).abs() < 0.001);
        assert!((levels.peak_right - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_empty() {
        let levels = compute_levels(&[], 2);
        assert_eq!(levels.rms_left, 0.0);
    }
}
