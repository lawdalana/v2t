use std::{fs::Permissions, io, os::unix::fs::PermissionsExt};

use objc2::{AnyThread, rc::Retained};
use objc2_avf_audio::{AVAudioCommonFormat, AVAudioFormat, AVAudioRecorder};
use objc2_foundation::{NSString, NSURL};
use tempfile::TempDir;

use crate::linux::audio;

const RECORDING_FILE: &str = "recording.wav";

pub(super) struct Recording {
    recorder: Retained<AVAudioRecorder>,
    directory: TempDir,
}

impl Recording {
    pub(super) fn start() -> io::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix("v2t-audio-")
            .permissions(Permissions::from_mode(0o700))
            .tempdir()?;
        let path = directory.path().join(RECORDING_FILE);
        let path = path
            .to_str()
            .ok_or_else(|| io::Error::other("Could not open the temporary recording path"))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(path));
        // SAFETY: This uses the pipeline's supported mono, signed 16-bit PCM format
        // and writes to a fresh WAV path in a private temporary directory.
        unsafe {
            let format = AVAudioFormat::initWithCommonFormat_sampleRate_channels_interleaved(
                AVAudioFormat::alloc(),
                AVAudioCommonFormat::PCMFormatInt16,
                16_000.0,
                1,
                true,
            )
            .ok_or_else(|| io::Error::other("Could not configure the recording format"))?;
            let recorder =
                AVAudioRecorder::initWithURL_format_error(AVAudioRecorder::alloc(), &url, &format)
                    .map_err(|error| io::Error::other(error.localizedDescription().to_string()))?;
            if !recorder.recordForDuration(f64::from(audio::MAX_SECONDS)) {
                return Err(io::Error::other(
                    "Could not start the microphone. Check the selected input in System Settings",
                ));
            }
            Ok(Self {
                recorder,
                directory,
            })
        }
    }

    pub(super) fn is_recording(&self) -> bool {
        // SAFETY: The controller only uses the recorder on the main thread.
        unsafe { self.recorder.isRecording() }
    }

    pub(super) fn finish(self) -> io::Result<Vec<u8>> {
        // SAFETY: Stopping on the main thread closes the WAV before it is read.
        unsafe { self.recorder.stop() };
        audio::read_wave(&self.directory.path().join(RECORDING_FILE))
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if self.is_recording() {
            // SAFETY: Close the file before TempDir removes the recording directory.
            unsafe { self.recorder.stop() };
        }
    }
}
