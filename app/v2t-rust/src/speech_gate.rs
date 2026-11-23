const HEADER: usize = 44;
const SAMPLES: usize = 320;
const FRAME_BYTES: usize = SAMPLES * 2;
const MIN_ENERGY: i64 = 48 * 48 * (SAMPLES as i64).pow(2);

pub fn has_speech(wave: &[u8]) -> bool {
    let Some(pcm) = recorder_samples(wave) else {
        return false;
    };
    let mut consecutive = 0;
    for frame in pcm.chunks_exact(FRAME_BYTES) {
        let mut sum = 0;
        let mut squares = 0;
        for sample in frame.chunks_exact(2) {
            let value = i64::from(i16::from_le_bytes([sample[0], sample[1]]));
            sum += value;
            squares += value * value;
        }
        if SAMPLES as i64 * squares - sum * sum >= MIN_ENERGY {
            consecutive += 1;
            if consecutive == 3 {
                return true;
            }
        } else {
            consecutive = 0;
        }
    }
    false
}

fn recorder_samples(wave: &[u8]) -> Option<&[u8]> {
    if wave.len() < HEADER + FRAME_BYTES * 3 || !wave.len().is_multiple_of(2) {
        return None;
    }
    let u16_at =
        |start| u16::from_le_bytes(wave[start..start + 2].try_into().expect("fixed PCM header"));
    let u32_at =
        |start| u32::from_le_bytes(wave[start..start + 4].try_into().expect("fixed PCM header"));
    (&wave[..4] == b"RIFF"
        && &wave[8..16] == b"WAVEfmt "
        && u32_at(4) as usize == wave.len() - 8
        && u32_at(16) == 16
        && u16_at(20) == 1
        && u16_at(22) == 1
        && u32_at(24) == 16_000
        && u32_at(28) == 32_000
        && u16_at(32) == 2
        && u16_at(34) == 16
        && &wave[36..40] == b"data"
        && u32_at(40) as usize == wave.len() - HEADER)
        .then_some(&wave[HEADER..])
}

#[cfg(test)]
mod tests {
    use super::*;

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
