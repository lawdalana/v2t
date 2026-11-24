pub const HEADER: usize = 44;
pub const SAMPLE_RATE: u32 = 16_000;
pub const BYTES_PER_SECOND: usize = SAMPLE_RATE as usize * 2;
pub const PCM_FORMAT: &[u8] = b"\x01\x00\x01\x00\x80\x3e\x00\x00\x00\x7d\x00\x00\x02\x00\x10\x00";
const SAMPLES: usize = 320;
const FRAME_BYTES: usize = SAMPLES * 2;
const MIN_RMS: i64 = 48;
const MIN_VOICED: usize = 3;

// Each frame is 20 ms. Prefer a pause, with a hard cap for continuous speech.
const PAUSE: usize = 50;
const SHORT_PAUSE: usize = 25;
const LONG_CHUNK: usize = 500;
const MAX_CHUNK: usize = 600;
const LEAD_IN: usize = 5;
const TAIL: usize = 10;

#[cfg(any(not(windows), test))]
pub fn has_speech(wave: &[u8]) -> bool {
    let Some(pcm) = recorder_samples(wave) else {
        return false;
    };
    let mut consecutive = 0;
    for frame in pcm.chunks_exact(FRAME_BYTES) {
        if is_voiced(frame) {
            consecutive += 1;
            if consecutive == MIN_VOICED {
                return true;
            }
        } else {
            consecutive = 0;
        }
    }
    false
}

fn is_voiced(frame: &[u8]) -> bool {
    let count = (frame.len() / 2) as i64;
    let mut sum = 0;
    let mut squares = 0;
    for sample in frame.chunks_exact(2) {
        let value = i64::from(i16::from_le_bytes([sample[0], sample[1]]));
        sum += value;
        squares += value * value;
    }
    count > 0 && count * squares - sum * sum >= (MIN_RMS * count).pow(2)
}

#[derive(Default)]
pub struct Chunker {
    frame: Vec<u8>,
    pcm: Vec<u8>,
    consecutive: usize,
    quiet: usize,
    has_speech: bool,
    continuing: bool,
}

impl Chunker {
    pub fn push(&mut self, mut pcm: &[u8], mut emit: impl FnMut(Vec<u8>)) {
        while !pcm.is_empty() {
            let count = pcm.len().min(FRAME_BYTES - self.frame.len());
            let (piece, rest) = pcm.split_at(count);
            pcm = rest;
            self.frame.extend_from_slice(piece);
            if self.frame.len() == FRAME_BYTES
                && let Some(wave) = self.complete_frame()
            {
                emit(wave);
            }
        }
    }

    pub fn finish(mut self) -> Option<Vec<u8>> {
        let aligned = self.frame.len() & !1;
        let tail = &self.frame[..aligned];
        if !(self.has_speech || (self.continuing && is_voiced(tail))) {
            return None;
        }
        self.pcm.extend_from_slice(tail);
        Some(encode_wave(&self.pcm))
    }

    fn complete_frame(&mut self) -> Option<Vec<u8>> {
        let voiced = is_voiced(&self.frame);
        self.pcm.extend_from_slice(&self.frame);
        self.frame.clear();
        if voiced {
            self.consecutive += 1;
            self.quiet = 0;
            self.has_speech |= self.continuing || self.consecutive >= MIN_VOICED;
        } else {
            self.consecutive = 0;
            self.quiet += 1;
        }
        self.continuing = false;

        if !self.has_speech {
            let keep = (LEAD_IN + self.consecutive) * FRAME_BYTES;
            let discard = self.pcm.len().saturating_sub(keep);
            self.pcm.drain(..discard);
            return None;
        }

        let frames = self.pcm.len() / FRAME_BYTES;
        if frames >= MAX_CHUNK
            || self.quiet >= PAUSE
            || (frames >= LONG_CHUNK && self.quiet >= SHORT_PAUSE)
        {
            return Some(self.split());
        }
        None
    }

    fn split(&mut self) -> Vec<u8> {
        let end = self.pcm.len() - self.quiet.saturating_sub(TAIL) * FRAME_BYTES;
        let wave = encode_wave(&self.pcm[..end]);
        let keep = self.quiet.min(LEAD_IN) * FRAME_BYTES;
        self.pcm.drain(..self.pcm.len() - keep);
        // A word can span the hard cap. Keep a voiced continuation even if it
        // is too short to pass the initial speech gate on its own.
        self.continuing = self.quiet == 0;
        self.has_speech = false;
        self.consecutive = 0;
        self.quiet = 0;
        wave
    }
}

pub fn encode_wave(samples: &[u8]) -> Vec<u8> {
    let bytes = samples.len() as u32;
    let mut wave = Vec::with_capacity(samples.len() + HEADER);
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(&(bytes + 36).to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt \x10\x00\x00\x00");
    wave.extend_from_slice(PCM_FORMAT);
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&bytes.to_le_bytes());
    wave.extend_from_slice(samples);
    wave
}

#[cfg(any(not(windows), test))]
fn recorder_samples(wave: &[u8]) -> Option<&[u8]> {
    if wave.len() < HEADER + FRAME_BYTES * MIN_VOICED || !wave.len().is_multiple_of(2) {
        return None;
    }
    let u32_at =
        |start| u32::from_le_bytes(wave[start..start + 4].try_into().expect("fixed PCM header"));
    (&wave[..4] == b"RIFF"
        && &wave[8..16] == b"WAVEfmt "
        && u32_at(4) as usize == wave.len() - 8
        && u32_at(16) == 16
        && &wave[20..36] == PCM_FORMAT
        && &wave[36..40] == b"data"
        && u32_at(40) as usize == wave.len() - HEADER)
        .then_some(&wave[HEADER..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_canonical_pcm_wire_format() {
        assert_eq!(
            encode_wave(&[1, 2, 3, 4]),
            b"RIFF\x28\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\x04\0\0\0\x01\x02\x03\x04"
        );
    }

    fn voiced(frames: usize) -> Vec<u8> {
        [400i16.to_le_bytes(), (-400i16).to_le_bytes()]
            .into_iter()
            .flatten()
            .cycle()
            .take(frames * 640)
            .collect()
    }

    #[test]
    fn emits_at_one_second_of_silence_and_keeps_listening() {
        let mut chunker = Chunker::default();
        let mut waves = Vec::new();
        for piece in voiced(10).chunks(749) {
            chunker.push(piece, |wave| waves.push(wave));
        }
        chunker.push(&[0; 49 * 640], |wave| waves.push(wave));
        assert!(waves.is_empty());
        chunker.push(&[0; 640], |wave| waves.push(wave));
        assert_eq!(waves.len(), 1);
        assert_eq!(waves[0].len(), 44 + 20 * 640);
        assert!(has_speech(&waves[0]));

        let next = voiced(7);
        chunker.push(&next, |wave| waves.push(wave));
        assert_eq!(waves.len(), 1);
        let last = chunker.finish().expect("the last phrase is flushed");
        assert_eq!(&last[44 + 5 * 640..], &next);
    }

    #[test]
    fn accepts_a_shorter_pause_only_after_ten_seconds() {
        let mut chunker = Chunker::default();
        let mut waves = Vec::new();
        chunker.push(&voiced(250), |wave| waves.push(wave));
        chunker.push(&[0; 25 * 640], |wave| waves.push(wave));
        assert!(waves.is_empty());
        chunker.push(&voiced(225), |wave| waves.push(wave));
        chunker.push(&[0; 24 * 640], |wave| waves.push(wave));
        assert!(waves.is_empty());
        chunker.push(&[0; 640], |wave| waves.push(wave));
        assert_eq!(waves.len(), 1);
    }

    #[test]
    fn cuts_continuous_speech_at_twelve_seconds_without_losing_the_tail() {
        let mut chunker = Chunker::default();
        let mut waves = Vec::new();
        let mut samples = voiced(601);
        samples.extend_from_slice(&[7, 0, 8, 0, 9]);
        chunker.push(&samples, |wave| waves.push(wave));
        assert_eq!(waves.len(), 1);
        assert_eq!(&waves[0][44..], &samples[..384_000]);
        let last = chunker
            .finish()
            .expect("continuous speech reaches the tail");
        assert_eq!(&last[44..], &samples[384_000..samples.len() - 1]);
    }

    #[test]
    fn flushes_partial_voiced_frame_immediately_after_a_hard_cut() {
        let mut chunker = Chunker::default();
        let mut samples = voiced(600);
        samples.extend_from_slice(&voiced(1)[..400]);
        chunker.push(&samples, |wave| assert_eq!(wave.len(), 384_044));
        let tail = chunker
            .finish()
            .expect("preserve the final 12.5 ms of speech");
        assert_eq!(&tail[44..], &samples[384_000..]);
    }

    #[test]
    fn ignores_silence_and_isolated_noise() {
        let mut chunker = Chunker::default();
        let silence = [0; 32_000];
        for _ in 0..13 {
            chunker.push(&silence, |_| panic!("silence must not be submitted"));
        }
        chunker.push(&voiced(2), |_| {
            panic!("a brief spike must not be submitted")
        });
        assert!(chunker.finish().is_none());
    }

    #[test]
    fn rejects_dc_and_short_noise_but_accepts_three_voiced_frames() {
        let mut wave = Vec::from(*b"RIFF\0\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\0\0\0\0");
        let pcm = [(1500i16).to_le_bytes(); SAMPLES * 4];
        for sample in pcm {
            wave.extend(sample);
        }
        let len = wave.len() as u32;
        wave[4..8].copy_from_slice(&(len - 8).to_le_bytes());
        wave[40..44].copy_from_slice(&(len - HEADER as u32).to_le_bytes());
        assert!(!has_speech(&wave));
        for (i, sample) in wave[44..].chunks_exact_mut(2).take(SAMPLES * 2).enumerate() {
            sample.copy_from_slice(&(if i % 2 == 0 { 400i16 } else { -400 }).to_le_bytes());
        }
        assert!(!has_speech(&wave));
        for (i, sample) in wave[44..].chunks_exact_mut(2).take(SAMPLES * 3).enumerate() {
            sample.copy_from_slice(&(if i % 2 == 0 { 400i16 } else { -400 }).to_le_bytes());
        }
        assert!(has_speech(&wave));
        assert!(!has_speech(&wave[..wave.len() - 1]));
    }
}
