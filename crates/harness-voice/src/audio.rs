use std::io::Cursor;
use hound::{WavSpec, WavWriter};
use harness_core::HarnessError;

pub struct AudioProcessor;

impl AudioProcessor {
    /// Encode mono 16-bit PCM samples into standard WAV bytes
    pub fn samples_to_wav(samples: &[i16], sample_rate: u32) -> Result<Vec<u8>, HarnessError> {
        let spec = WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };

        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = WavWriter::new(&mut cursor, spec)
                .map_err(|e| HarnessError::VoiceError(format!("Failed to create WAV writer: {}", e)))?;

            for &s in samples {
                writer
                    .write_sample(s)
                    .map_err(|e| HarnessError::VoiceError(format!("Failed to write sample: {}", e)))?;
            }
            writer
                .finalize()
                .map_err(|e| HarnessError::VoiceError(format!("Failed to finalize WAV: {}", e)))?;
        }

        Ok(cursor.into_inner())
    }

    /// Calculate RMS energy of audio frame to determine if speech is present
    pub fn calculate_rms_energy(samples: &[i16]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        ((sum_sq / samples.len() as f64).sqrt()) as f32
    }
}
