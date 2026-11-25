use std::{
    ffi::c_void,
    io, mem,
    ptr::{NonNull, null, null_mut},
    slice,
    sync::{Mutex, MutexGuard, mpsc},
    time::{Duration, Instant},
};

use objc2_audio_toolbox::{
    AudioQueueAllocateBuffer, AudioQueueBufferRef, AudioQueueDispose, AudioQueueEnqueueBuffer,
    AudioQueueFlush, AudioQueueGetProperty, AudioQueueNewInput, AudioQueueRef, AudioQueueStart,
    AudioQueueStop, kAudioQueueProperty_IsRunning,
};
use objc2_core_audio_types::{
    AudioStreamBasicDescription, AudioStreamPacketDescription, AudioTimeStamp,
    kAudioFormatFlagIsPacked, kAudioFormatFlagIsSignedInteger, kAudioFormatLinearPCM,
};

use crate::{
    linux::{
        audio,
        pipeline::{RecordingResult, RecordingSpeech},
    },
    speech_gate::{BYTES_PER_SECOND, Chunker, SAMPLE_RATE},
};

// Forty milliseconds of interleaved mono PCM per input buffer.
const BUFFER_BYTES: u32 = (BYTES_PER_SECOND * 40 / 1_000) as u32;
const BUFFER_COUNT: usize = 4;
const MAX_PCM_BYTES: usize = audio::MAX_SECONDS as usize * BYTES_PER_SECOND;
const PCM_FORMAT: AudioStreamBasicDescription = AudioStreamBasicDescription {
    mSampleRate: SAMPLE_RATE as f64,
    mFormatID: kAudioFormatLinearPCM,
    mFormatFlags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
    mBytesPerPacket: 2,
    mFramesPerPacket: 1,
    mBytesPerFrame: 2,
    mChannelsPerFrame: 1,
    mBitsPerChannel: 16,
    mReserved: 0,
};

pub(super) struct PendingTranscript {
    receive: mpsc::Receiver<RecordingResult>,
    capture_failure: Option<String>,
}

impl PendingTranscript {
    pub(super) fn try_recv(&self) -> Result<RecordingResult, mpsc::TryRecvError> {
        match (self.receive.try_recv(), &self.capture_failure) {
            (Ok(Ok(mut transcript)), Some(error)) if transcript.text.is_some() => {
                transcript.partial_failure = Some(match transcript.partial_failure {
                    Some(transcription_error) => format!("{error} {transcription_error}"),
                    None => error.clone(),
                });
                Ok(Ok(transcript))
            }
            (Ok(Ok(_)) | Err(mpsc::TryRecvError::Disconnected), Some(error)) => {
                Ok(Err(error.clone()))
            }
            (result, _) => result,
        }
    }
}

pub(super) struct Recording {
    input: InputQueue,
    transcript: mpsc::Receiver<RecordingResult>,
    started: Instant,
}

impl Recording {
    pub(super) fn start(
        speech: RecordingSpeech,
        transcript: mpsc::Receiver<RecordingResult>,
    ) -> io::Result<Self> {
        Ok(Self {
            input: InputQueue::start(speech)?,
            transcript,
            started: Instant::now(),
        })
    }

    pub(super) fn is_recording(&self) -> bool {
        let elapsed = self.started.elapsed();
        self.input.can_requeue()
            && elapsed < Duration::from_secs(u64::from(audio::MAX_SECONDS))
            // The running property may not change until the input hardware starts.
            && (elapsed < Duration::from_secs(1) || self.input.is_running())
    }

    pub(super) fn finish(self) -> PendingTranscript {
        let elapsed = self.started.elapsed();
        let interrupted = elapsed >= Duration::from_secs(1)
            && elapsed < Duration::from_secs(u64::from(audio::MAX_SECONDS))
            && self.input.can_requeue()
            && !self.input.is_running();
        PendingTranscript {
            receive: self.transcript,
            capture_failure: self
                .input
                .finish()
                .map(|error| error.to_string())
                .or_else(|| interrupted.then(|| "The microphone stopped unexpectedly.".to_owned())),
        }
    }
}

struct InputQueue {
    queue: AudioQueueRef,
    callback: NonNull<Mutex<Capture>>,
}

impl InputQueue {
    fn start(speech: RecordingSpeech) -> io::Result<Self> {
        let capture = Box::new(Mutex::new(Capture {
            audio: CapturedAudio::new(MAX_PCM_BYTES),
            speech: Some(speech),
            requeue: true,
            error: None,
        }));
        let mut input = Self {
            queue: null_mut(),
            callback: NonNull::from(Box::leak(capture)),
        };

        // SAFETY: The callback context lives through synchronous disposal. A null run loop
        // runs callbacks on the queue's own thread, where access is protected by its mutex.
        unsafe {
            let mut queue = null_mut();
            status(
                AudioQueueNewInput(
                    NonNull::from(&PCM_FORMAT),
                    Some(receive_pcm),
                    input.callback.as_ptr().cast(),
                    None,
                    None,
                    0,
                    NonNull::from(&mut queue),
                ),
                "open",
            )?;
            if queue.is_null() {
                return Err(io::Error::other("Could not open the microphone."));
            }
            input.queue = queue;
            for _ in 0..BUFFER_COUNT {
                let mut buffer = null_mut();
                status(
                    AudioQueueAllocateBuffer(input.queue, BUFFER_BYTES, NonNull::from(&mut buffer)),
                    "allocate buffers for",
                )?;
                status(
                    AudioQueueEnqueueBuffer(input.queue, buffer, 0, null()),
                    "queue buffers for",
                )?;
            }
            status(AudioQueueStart(input.queue, null()), "start")?;
        }
        Ok(input)
    }

    fn capture(&self) -> MutexGuard<'_, Capture> {
        // SAFETY: The owned allocation persists while this InputQueue is borrowed.
        unsafe { self.callback.as_ref() }
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn can_requeue(&self) -> bool {
        self.capture().requeue
    }

    fn is_running(&self) -> bool {
        let mut running = 0u32;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: The queue is live. IsRunning reports a UInt32 into the supplied space.
        let status = unsafe {
            AudioQueueGetProperty(
                self.queue,
                kAudioQueueProperty_IsRunning,
                NonNull::from(&mut running).cast(),
                NonNull::from(&mut size),
            )
        };
        status == 0 && running != 0
    }

    fn finish(mut self) -> Option<io::Error> {
        self.capture().requeue = false;
        // SAFETY: The queue is live and no callback lock is held. Apple recommends
        // flushing pending audio before an immediate stop. See AudioQueueFlush.
        let flush_error = status(unsafe { AudioQueueFlush(self.queue) }, "flush").err();
        let stop_error = status(unsafe { AudioQueueStop(self.queue, true) }, "stop").err();
        let dispose_error = self.dispose().err();
        let mut capture = self.capture();
        let mut error = capture
            .error
            .take()
            .or(flush_error)
            .or(stop_error)
            .or(dispose_error);
        if let Some(speech) = capture.speech.take() {
            if let Some(wave) = mem::take(&mut capture.audio).finish()
                && let Err(send_error) = speech.send(wave)
            {
                error.get_or_insert(send_error);
            }
            speech.finish();
        }
        error
    }

    fn dispose(&mut self) -> io::Result<()> {
        if !self.queue.is_null() {
            // SAFETY: Only this owner disposes the queue, and never from a callback.
            status(unsafe { AudioQueueDispose(self.queue, true) }, "close")?;
            self.queue = null_mut();
        }
        Ok(())
    }
}

impl Drop for InputQueue {
    fn drop(&mut self) {
        {
            let mut capture = self.capture();
            capture.requeue = false;
            capture.speech.take();
            capture.audio = CapturedAudio::default();
        }
        match self.dispose() {
            // SAFETY: Native disposal stops callbacks and the allocation was owned by Self.
            Ok(()) => unsafe { drop(Box::from_raw(self.callback.as_ptr())) },
            Err(error) => eprintln!("{error}"),
        }
    }
}

struct Capture {
    audio: CapturedAudio,
    speech: Option<RecordingSpeech>,
    requeue: bool,
    error: Option<io::Error>,
}

unsafe extern "C-unwind" fn receive_pcm(
    context: *mut c_void,
    queue: AudioQueueRef,
    buffer: AudioQueueBufferRef,
    _time: NonNull<AudioTimeStamp>,
    _packets: u32,
    _descriptions: *const AudioStreamPacketDescription,
) {
    // SAFETY: AudioQueue returns the registered context and its live input buffer.
    let (Some(context), Some(data)) = (
        unsafe { context.cast::<Mutex<Capture>>().as_ref() },
        unsafe { buffer.as_ref() },
    ) else {
        return;
    };
    let Ok(mut capture) = context.lock() else {
        return;
    };
    let Capture {
        audio,
        speech,
        requeue,
        error,
    } = &mut *capture;
    let Some(speech) = speech else {
        return;
    };
    if error.is_some() {
        return;
    }
    let bytes = data.mAudioDataByteSize;
    if bytes > data.mAudioDataBytesCapacity || !bytes.is_multiple_of(2) {
        *error = Some(io::Error::other(
            "macOS returned an invalid microphone buffer.",
        ));
        *requeue = false;
        return;
    }

    // SAFETY: The initialized PCM bytes are valid until we re-enqueue the buffer.
    let pcm = unsafe { slice::from_raw_parts(data.mAudioData.as_ptr().cast(), bytes as usize) };
    let reached_limit = audio.push(pcm, |wave| {
        if error.is_none() {
            *error = speech.send(wave).err();
        }
    });
    if reached_limit || error.is_some() {
        *requeue = false;
    }
    if *requeue {
        // SAFETY: The queue returned this buffer, and the borrow of its PCM data has ended.
        *error = status(
            unsafe { AudioQueueEnqueueBuffer(queue, buffer, 0, null()) },
            "queue buffers for",
        )
        .err();
        *requeue = error.is_none();
    }
}

#[derive(Default)]
struct CapturedAudio {
    chunks: Chunker,
    remaining: usize,
}

impl CapturedAudio {
    fn new(remaining: usize) -> Self {
        Self {
            remaining,
            ..Self::default()
        }
    }

    fn push(&mut self, pcm: &[u8], emit: impl FnMut(Vec<u8>)) -> bool {
        let bytes = pcm.len().min(self.remaining);
        self.chunks.push(&pcm[..bytes], emit);
        self.remaining -= bytes;
        self.remaining == 0
    }

    fn finish(self) -> Option<Vec<u8>> {
        self.chunks.finish()
    }
}

fn status(code: i32, operation: &str) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Could not {operation} the microphone (macOS audio error {code})."
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux::pipeline::RecordingTranscript;

    #[test]
    fn capture_failure_keeps_the_transcribed_audio() {
        let (send, receive) = mpsc::channel();
        send.send(Ok(RecordingTranscript {
            text: Some("earlier speech".to_owned()),
            partial_failure: None,
            correction_fallback: false,
        }))
        .unwrap();
        let pending = PendingTranscript {
            receive,
            capture_failure: Some("Microphone disconnected.".to_owned()),
        };
        let result = pending.try_recv().unwrap().unwrap();
        assert_eq!(result.text.as_deref(), Some("earlier speech"));
        assert_eq!(
            result.partial_failure.as_deref(),
            Some("Microphone disconnected.")
        );
    }

    #[test]
    fn limits_the_last_buffer_before_flushing_speech() {
        let voiced_frame = [0x90, 0x01, 0x70, 0xfe].repeat(160);
        let mut audio = CapturedAudio::new(1_920);
        assert!(!audio.push(&voiced_frame.repeat(2), |_| panic!("early chunk")));
        assert!(audio.push(&voiced_frame.repeat(2), |_| panic!("early chunk")));
        let wave = audio
            .finish()
            .expect("the remaining speech stays available");
        assert_eq!(&wave[44..], voiced_frame.repeat(3));
    }
}
