//! Provider-free streaming conversion and bounded playback accounting.

use std::collections::VecDeque;

pub const PCM_RATE: u32 = 24_000;
pub const MIC_SAMPLES: usize = 480;

/// Linear streaming resampling. Phase survives callback/chunk boundaries.
pub struct Resampler {
    input_rate: u32,
    output_rate: u32,
    phase: u64,
    previous: Option<f32>,
}

impl Resampler {
    pub fn new(input_rate: u32, output_rate: u32) -> Self {
        Self { input_rate: input_rate.max(1), output_rate: output_rate.max(1), phase: 0, previous: None }
    }

    pub fn push(&mut self, sample: f32, mut emit: impl FnMut(f32)) {
        let sample = if sample.is_finite() { sample.clamp(-1.0, 1.0) } else { 0.0 };
        let previous = self.previous.replace(sample).unwrap_or(sample);
        self.phase += u64::from(self.output_rate);
        while self.phase >= u64::from(self.input_rate) {
            self.phase -= u64::from(self.input_rate);
            let fraction = 1.0 - self.phase as f32 / self.output_rate as f32;
            emit(previous + (sample - previous) * fraction);
        }
    }
}

pub fn pcm_sample(sample: f32) -> i16 {
    if sample.is_finite() { (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16 } else { 0 }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heard {
    pub item: String,
    pub content_index: u64,
    pub frames: u64,
}

struct Chunk {
    item: String,
    content_index: u64,
    samples: std::vec::IntoIter<f32>,
}

pub struct Playback {
    chunks: VecDeque<Chunk>,
    queued: usize,
    max_frames: usize,
    pub heard: Option<Heard>,
}

impl Playback {
    pub fn new(output_rate: u32) -> Self {
        Self { chunks: VecDeque::new(), queued: 0, max_frames: (output_rate as usize).saturating_mul(10), heard: None }
    }

    pub fn append(&mut self, item: String, content_index: u64, samples: Vec<f32>) -> Result<(), String> {
        if samples.is_empty() {
            return Ok(());
        }
        if self.queued.saturating_add(samples.len()) > self.max_frames || self.chunks.len() >= 512 {
            return Err("Voice playback fell behind. Restart voice to continue.".into());
        }
        self.queued += samples.len();
        self.chunks.push_back(Chunk { item, content_index, samples: samples.into_iter() });
        Ok(())
    }

    pub fn next_sample(&mut self) -> f32 {
        loop {
            let Some(chunk) = self.chunks.front_mut() else {
                return 0.0;
            };
            if let Some(sample) = chunk.samples.next() {
                self.queued = self.queued.saturating_sub(1);
                match self.heard.as_mut() {
                    Some(heard) if heard.item == chunk.item && heard.content_index == chunk.content_index => {
                        heard.frames = heard.frames.saturating_add(1);
                    }
                    _ => self.heard = Some(Heard { item: chunk.item.clone(), content_index: chunk.content_index, frames: 1 }),
                }
                return sample;
            }
            self.chunks.pop_front();
        }
    }

    /// Include queued-but-unheard items with a zero cursor so they are removed too.
    pub fn interrupt(&mut self, response_active: bool) -> Vec<Heard> {
        let heard = self.heard.take();
        let mut cursors = if response_active { heard.clone().into_iter().collect::<Vec<_>>() } else { Vec::new() };
        for chunk in self.chunks.drain(..) {
            if chunk.samples.len() == 0 {
                continue;
            }
            if !cursors.iter().any(|heard| heard.item == chunk.item && heard.content_index == chunk.content_index) {
                let frames = heard
                    .as_ref()
                    .filter(|heard| heard.item == chunk.item && heard.content_index == chunk.content_index)
                    .map_or(0, |heard| heard.frames);
                cursors.push(Heard { item: chunk.item, content_index: chunk.content_index, frames });
            }
        }
        self.queued = 0;
        cursors
    }
}

#[cfg(test)]
mod tests {
    use super::{Playback, Resampler, pcm_sample};

    #[test]
    fn resampling_is_continuous_across_callbacks() {
        for input in [16_000, 24_000, 44_100, 48_000] {
            let mut converter = Resampler::new(input, 24_000);
            let mut output = Vec::new();
            for _ in 0..input {
                converter.push(0.25, |sample| output.push(sample));
            }
            assert_eq!(output.len(), 24_000);
            assert!(output.iter().all(|sample| (*sample - 0.25).abs() < 0.0001));
        }
    }

    #[test]
    fn invalid_samples_and_clipping_are_safe() {
        assert_eq!(pcm_sample(f32::NAN), 0);
        assert_eq!(pcm_sample(2.0), i16::MAX);
        assert_eq!(pcm_sample(-2.0), -i16::MAX);
    }

    #[test]
    fn interruption_counts_only_played_frames_and_clears_every_item() {
        let mut playback = Playback::new(24_000);
        assert!(playback.append("a".into(), 0, vec![0.5; 240]).is_ok());
        assert!(playback.append("b".into(), 1, vec![0.2; 240]).is_ok());
        for _ in 0..120 {
            assert_eq!(playback.next_sample(), 0.5);
        }
        let cursors = playback.interrupt(true);
        assert_eq!(cursors.len(), 2);
        assert_eq!(cursors.first().map(|heard| heard.frames), Some(120));
        assert_eq!(cursors.last().map(|heard| heard.frames), Some(0));
        assert_eq!(playback.next_sample(), 0.0);
        assert!(playback.interrupt(false).is_empty());
    }

    #[test]
    fn completed_and_fully_played_audio_is_not_truncated() {
        let mut playback = Playback::new(24_000);
        assert!(playback.append("a".into(), 0, vec![0.5]).is_ok());
        assert_eq!(playback.next_sample(), 0.5);
        assert!(playback.interrupt(false).is_empty());
    }

    #[test]
    fn playback_backpressure_is_bounded() {
        let mut playback = Playback::new(24_000);
        assert!(playback.append("a".into(), 0, vec![0.0; 240_001]).is_err());
        assert_eq!(playback.next_sample(), 0.0);
    }
}
