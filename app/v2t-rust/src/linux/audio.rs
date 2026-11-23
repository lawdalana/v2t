use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

pub(super) const BYTES_PER_SECOND: usize = 32_000;
pub(super) const HEADER: usize = 44;
pub(super) const MAX_SECONDS: u16 = 300;
const MAX_PCM_BYTES: usize = MAX_SECONDS as usize * BYTES_PER_SECOND;
const MAX_WAVE_BYTES: u64 = 25_000_000;

const PCM_FORMAT: &[u8] = b"\x01\x00\x01\x00\x80\x3e\x00\x00\x00\x7d\x00\x00\x02\x00\x10\x00";
const EXTENSIBLE_PCM: &[u8] = b"\x01\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71";

pub(super) fn read_wave(path: &Path) -> io::Result<Vec<u8>> {
    if !path.metadata()?.is_file() {
        return Err(invalid("The input WAV must be a regular file."));
    }
    let mut data = Vec::new();
    File::open(path)?
        .take(MAX_WAVE_BYTES + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_WAVE_BYTES {
        return Err(invalid("The input WAV exceeds the 25 MB file limit."));
    }
    normalize(&data)
}

pub(super) fn record(seconds: u16) -> io::Result<Vec<u8>> {
    if !(1..=MAX_SECONDS).contains(&seconds) {
        return Err(invalid("Recording duration must be from 1 to 300 seconds."));
    }
    let mut child = Command::new("parec");
    child
        .args([
            "--raw",
            "--format=s16le",
            "--rate=16000",
            "--channels=1",
            "--latency-msec=100",
            "--client-name=v2t",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for key in [
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "OPENROUTER_KEY_TOKEN",
        "OPENROUTER_KEY",
    ] {
        child.env_remove(key);
    }
    let mut capture = Capture(child.spawn().map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => io::Error::new(
            io::ErrorKind::NotFound,
            "Install parec (pulse-utils or pulseaudio-utils) for microphone recording.",
        ),
        _ => io::Error::other("Could not start the microphone recorder."),
    })?);
    let mut stream = capture
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Could not open the microphone stream."))?;
    let bytes = usize::from(seconds) * BYTES_PER_SECOND;
    let (send, receive) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("v2t-microphone".into())
        .spawn(move || {
            let mut samples = vec![0; bytes];
            let result = stream.read_exact(&mut samples).map(|()| samples);
            let _ = send.send(result);
        })?;
    let result = receive.recv_timeout(Duration::from_secs(u64::from(seconds) + 5));
    drop(capture);
    reader
        .join()
        .map_err(|_| io::Error::other("The microphone reader stopped unexpectedly."))?;
    match result {
        Ok(Ok(samples)) => Ok(encode(&samples)),
        Ok(Err(_)) => Err(io::Error::other(
            "Microphone recording failed. Check PulseAudio or pipewire-pulse and the default source.",
        )),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Microphone recording timed out.",
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
            "The microphone reader stopped unexpectedly.",
        )),
    }
}

struct Capture(Child);

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn normalize(input: &[u8]) -> io::Result<Vec<u8>> {
    if input.len() < 12
        || &input[..4] != b"RIFF"
        || &input[8..12] != b"WAVE"
        || u32::from_le_bytes(input[4..8].try_into().unwrap()) as usize != input.len() - 8
    {
        return Err(invalid("The input file is not a valid RIFF WAV."));
    }
    let mut chunks = &input[12..];
    let mut found_format = false;
    let mut samples = None;
    while !chunks.is_empty() {
        let Some(header) = chunks.get(..8) else {
            return Err(truncated());
        };
        let size = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let Some(padded) = size.checked_add(size % 2) else {
            return Err(truncated());
        };
        let (name, remain) = (&header[..4], &chunks[8..]);
        let Some((chunk, next)) = remain.split_at_checked(padded) else {
            return Err(truncated());
        };
        let chunk = &chunk[..size];
        match name {
            b"fmt " if !found_format => {
                if !pcm_format(chunk) {
                    return Err(invalid(
                        "Use a 16 kHz mono WAV with signed 16-bit PCM samples.",
                    ));
                }
                found_format = true;
            }
            b"data"
                if samples.is_none()
                    && chunk.len() <= MAX_PCM_BYTES
                    && chunk.len().is_multiple_of(2) =>
            {
                samples = Some(chunk);
            }
            b"fmt " | b"data" => {
                return Err(invalid(
                    "Invalid WAV chunks or audio longer than five minutes.",
                ));
            }
            _ => (),
        }
        chunks = next;
    }
    match (found_format, samples) {
        (true, Some(samples)) => Ok(encode(samples)),
        _ => Err(invalid("The WAV is missing its format or audio data.")),
    }
}

fn pcm_format(format: &[u8]) -> bool {
    format.starts_with(PCM_FORMAT)
        || (format.len() >= 40
            && format[..2] == [0xfe, 0xff]
            && format[2..16] == PCM_FORMAT[2..]
            && u16::from_le_bytes([format[16], format[17]]) >= 22
            && format[18..20] == [16, 0]
            && &format[24..40] == EXTENSIBLE_PCM)
}

fn encode(samples: &[u8]) -> Vec<u8> {
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

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn truncated() -> io::Error {
    invalid("The WAV contains a truncated chunk.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        Vec::from(*b"RIFF\x2a\x00\x00\x00WAVEfmt \x10\x00\x00\x00\x01\x00\x01\x00\x80\x3e\x00\x00\x00\x7d\x00\x00\x02\x00\x10\x00data\x06\x00\x00\x00\xab\x00\xab\x00\xab\x00")
    }

    #[test]
    fn accepts_aligned_wav_with_metadata_and_preserves_samples() {
        let expected = fixture();
        let mut input = expected.clone();
        input.splice(36..36, *b"JUNK\x03\x00\x00\x00foo\x00");
        let size = input.len() as u32 - 8;
        input[4..8].copy_from_slice(&size.to_le_bytes());
        assert_eq!(normalize(&input).unwrap(), expected);
    }

    #[test]
    fn rejects_malformed_or_mismatched_pcm_without_reading_out_of_bounds() {
        let mut mismatched = fixture();
        mismatched[22] = 2;
        assert!(normalize(&mismatched).is_err());
        let mut truncated = fixture();
        truncated.pop();
        assert!(normalize(&truncated).is_err());
    }
}
