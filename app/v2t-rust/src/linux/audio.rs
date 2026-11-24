use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

pub(super) use crate::speech_gate::{BYTES_PER_SECOND, HEADER};
use crate::speech_gate::{Chunker, PCM_FORMAT, encode_wave};

pub(super) const MAX_SECONDS: u16 = 300;
const MAX_PCM_BYTES: usize = MAX_SECONDS as usize * BYTES_PER_SECOND;
const MAX_WAVE_BYTES: u64 = 25_000_000;

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

pub(super) fn record(
    seconds: u16,
    submit: impl FnMut(Vec<u8>) -> io::Result<()> + Send,
) -> io::Result<()> {
    if !(1..=MAX_SECONDS).contains(&seconds) {
        return Err(invalid("Recording duration must be from 1 to 300 seconds."));
    }
    let mut child = recorder_command();
    child
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
        io::ErrorKind::NotFound => io::Error::new(io::ErrorKind::NotFound, RECORDER_REQUIRED),
        _ => io::Error::other("Could not start the microphone recorder."),
    })?);
    let stream = capture
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Could not open the microphone stream."))?;
    let bytes = usize::from(seconds) * BYTES_PER_SECOND;
    thread::scope(move |scope| {
        let (send, receive) = mpsc::sync_channel(1);
        let reader = thread::Builder::new()
            .name("v2t-microphone".into())
            .spawn_scoped(scope, move || {
                let _ = send.send(read_chunks(stream, bytes, submit));
            })?;
        let result = receive.recv_timeout(Duration::from_secs(u64::from(seconds) + 5));
        drop(capture);
        reader
            .join()
            .map_err(|_| io::Error::other("The microphone reader stopped unexpectedly."))?;
        match result {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Microphone recording timed out.",
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other(
                "The microphone reader stopped unexpectedly.",
            )),
        }
    })
}

fn read_chunks(
    mut stream: impl Read,
    mut remaining: usize,
    mut submit: impl FnMut(Vec<u8>) -> io::Result<()>,
) -> io::Result<()> {
    let mut chunker = Chunker::default();
    let mut samples = [0; BYTES_PER_SECOND / 50];
    while remaining > 0 {
        let count = remaining.min(samples.len());
        let frame = &mut samples[..count];
        stream
            .read_exact(frame)
            .map_err(|_| io::Error::other(RECORDER_FAILED))?;
        remaining -= count;
        let mut submitted = Ok(());
        chunker.push(frame, |wave| {
            if submitted.is_ok() {
                submitted = submit(wave);
            }
        });
        submitted?;
    }
    if let Some(wave) = chunker.finish() {
        submit(wave)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
const RECORDER_REQUIRED: &str =
    "Install parec (pulse-utils or pulseaudio-utils) for microphone recording.";
#[cfg(target_os = "linux")]
const RECORDER_FAILED: &str =
    "Microphone recording failed. Check PulseAudio or pipewire-pulse and the default source.";

#[cfg(target_os = "linux")]
fn recorder_command() -> Command {
    let mut command = Command::new("parec");
    command.args([
        "--raw",
        "--format=s16le",
        "--rate=16000",
        "--channels=1",
        "--latency-msec=100",
        "--client-name=v2t",
    ]);
    command
}

#[cfg(target_os = "macos")]
const RECORDER_REQUIRED: &str =
    "Install ffmpeg with AVFoundation support for microphone recording.";
#[cfg(target_os = "macos")]
const RECORDER_FAILED: &str = "Microphone recording failed. Check microphone access for your terminal and the default audio input.";

#[cfg(target_os = "macos")]
fn recorder_command() -> Command {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-nostdin",
        "-loglevel",
        "error",
        "-f",
        "avfoundation",
        "-i",
        "none:default",
        "-ac",
        "1",
        "-ar",
        "16000",
        "-c:a",
        "pcm_s16le",
        "-f",
        "s16le",
        "pipe:1",
    ]);
    command
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
        (true, Some(samples)) => Ok(encode_wave(samples)),
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

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn truncated() -> io::Error {
    invalid("The WAV contains a truncated chunk.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, io::Cursor};

    #[test]
    fn submits_a_complete_chunk_while_more_microphone_audio_remains() {
        let mut audio = [500i16, -500]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .cycle()
            .take(BYTES_PER_SECOND)
            .collect::<Vec<_>>();
        audio.resize(3 * BYTES_PER_SECOND, 0);
        let submitted = Cell::new(false);
        struct Microphone<'a> {
            input: Cursor<Vec<u8>>,
            submitted: &'a Cell<bool>,
        }
        impl Read for Microphone<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.input.position() >= 2 * BYTES_PER_SECOND as u64 && !self.submitted.get() {
                    return Err(io::Error::other(
                        "Speech was not submitted before the next read.",
                    ));
                }
                self.input.read(buf)
            }
        }
        let mut microphone = Microphone {
            input: Cursor::new(audio),
            submitted: &submitted,
        };
        read_chunks(&mut microphone, 3 * BYTES_PER_SECOND, |chunk| {
            assert!(crate::speech_gate::has_speech(&chunk));
            assert!(!submitted.replace(true));
            Ok(())
        })
        .unwrap();
        assert!(submitted.get());
        assert_eq!(microphone.input.position(), 3 * BYTES_PER_SECOND as u64);
    }

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
