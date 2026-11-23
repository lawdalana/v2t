use std::{
    io,
    mem::{forget, size_of, take},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{NonNull, addr_of, null, null_mut},
    slice,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Sender, channel, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Media::{
        Audio::{
            CALLBACK_FUNCTION, HWAVEIN, PlaySoundW, SND_MEMORY, SND_NODEFAULT, WAVE_MAPPER,
            WAVEFORMATEX, WAVEHDR, waveInAddBuffer, waveInClose, waveInOpen, waveInPrepareHeader,
            waveInReset, waveInStart, waveInStop, waveInUnprepareHeader,
        },
        MM_WIM_DATA,
    },
    System::Threading::{CreateEventW, SetEvent, WaitForSingleObject},
};

pub const SAMPLE_RATE: u32 = 16_000;
pub const PCM_BYTES_PER_SECOND: u32 = SAMPLE_RATE * 2;
pub const HEADER_BYTES: usize = 44;
const MAX_PCM_BYTES: usize = PCM_BYTES_PER_SECOND as usize * 5 * 60;
const BUFFER_COUNT: usize = 4;
const BUFFER_BYTES: usize = 1_600;
const HEADER_SIZE: u32 = size_of::<WAVEHDR>() as u32;
const CUE_BYTES: &[u8] = include_bytes!("../../V2t.Windows/Audio/listening.wav");

#[repr(align(8))]
struct AlignedCue([u8; CUE_BYTES.len()]);

static CUE: AlignedCue = AlignedCue(*include_bytes!("../../V2t.Windows/Audio/listening.wav"));
static PLAYING_CUE: Mutex<()> = Mutex::new(());

/// Plays the embedded cue synchronously and allows the output device to settle.
/// Run off the UI thread and start recording only after this completes.
pub fn play_cue() -> io::Result<()> {
    let _playback = PLAYING_CUE
        .lock()
        .map_err(|_| io::Error::other("Listening cue interrupted."))?;
    // SAFETY: SND_MEMORY interprets this aligned address as a complete WAV file.
    // The static bytes outlive the synchronous playback and no module handle is required.
    if unsafe {
        PlaySoundW(
            CUE.0.as_ptr().cast(),
            null_mut(),
            SND_MEMORY | SND_NODEFAULT,
        )
    } == 0
    {
        return Err(io::Error::other("Could not play the listening sound."));
    }
    thread::sleep(Duration::from_millis(55));
    Ok(())
}

/// Native handles and audio buffers live solely on the recorder thread. This
/// controller sends start/stop signals and always joins that thread on disposal.
pub struct Recorder {
    commands: Option<Sender<Sender<io::Result<()>>>>,
    notification: Arc<Notification>,
    worker: Option<JoinHandle<io::Result<Vec<u8>>>>,
}

impl Recorder {
    pub fn prepare() -> io::Result<Self> {
        let notification = Arc::new(Notification::new()?);
        let worker_notification = Arc::clone(&notification);
        let (prepared, result) = sync_channel(1);
        let (commands, start) = channel::<Sender<io::Result<()>>>();
        let worker = thread::Builder::new()
            .name("v2t microphone".into())
            .spawn(move || {
                let mut device = match WaveInput::open(worker_notification) {
                    Ok(device) => device,
                    Err(error) => {
                        let _ = prepared.send(Err(error));
                        return Ok(Vec::new());
                    }
                };
                if prepared.send(Ok(())).is_err() {
                    return Ok(Vec::new());
                }
                let Ok(started) = start.recv() else {
                    return Ok(Vec::new());
                };
                let result = device.start();
                let running = result.is_ok();
                if started.send(result).is_err() || !running {
                    return Ok(Vec::new());
                }
                device.record()
            })?;
        let recorder = Self {
            commands: Some(commands),
            notification,
            worker: Some(worker),
        };
        result
            .recv()
            .map_err(|_| io::Error::other("Could not prepare the microphone."))??;
        Ok(recorder)
    }

    pub fn start(&mut self) -> io::Result<()> {
        let Some(commands) = self.commands.take() else {
            return Err(io::Error::other(
                "The microphone recorder cannot be started again.",
            ));
        };
        let (started, result) = channel();
        commands
            .send(started)
            .map_err(|_| io::Error::other("Could not start the microphone."))?;
        result
            .recv()
            .map_err(|_| io::Error::other("Could not start the microphone."))?
    }

    pub fn stop(mut self) -> io::Result<Vec<u8>> {
        self.signal_stop();
        self.join()
    }

    fn signal_stop(&mut self) {
        self.notification.stopping.store(true, Ordering::Release);
        self.notification.signal();
        self.commands.take();
    }

    fn join(&mut self) -> io::Result<Vec<u8>> {
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .map_err(|_| io::Error::other("Microphone worker stopped unexpectedly."))?,
            None => Ok(Vec::new()),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.signal_stop();
        let _ = self.join();
    }
}

struct Notification {
    event: OwnedHandle,
    ready: [AtomicBool; BUFFER_COUNT],
    stopping: AtomicBool,
}

impl Notification {
    fn new() -> io::Result<Self> {
        // SAFETY: create an unnamed, initially clear auto-reset event and assume its ownership.
        let event = unsafe { CreateEventW(null(), 0, 0, null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            // SAFETY: the handle came from a successful CreateEventW call that transfers ownership.
            event: unsafe { OwnedHandle::from_raw_handle(event) },
            ready: std::array::from_fn(|_| AtomicBool::new(false)),
            stopping: AtomicBool::new(false),
        })
    }

    fn signal(&self) {
        // SAFETY: the owned event remains valid for this synchronous Win32 call.
        unsafe { SetEvent(self.event.as_raw_handle() as HANDLE) };
    }

    fn wait(&self) -> io::Result<()> {
        // SAFETY: the handle refers to the live event owned by this Notification.
        match unsafe { WaitForSingleObject(self.event.as_raw_handle() as HANDLE, 200) } {
            WAIT_OBJECT_0 | WAIT_TIMEOUT => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

unsafe extern "system" fn wave_input_callback(
    _device: HWAVEIN,
    message: u32,
    instance: usize,
    header: usize,
    _unused: usize,
) {
    if message == MM_WIM_DATA && instance != 0 && header != 0 {
        // SAFETY: after returning a queued buffer, WinMM supplies that WAVEHDR and
        // the live Notification pointer given to waveInOpen. dwUser is initialized
        // once before queueing and remains read-only through waveInClose.
        let Some(ready) = (unsafe {
            let queue = &*(instance as *const Notification);
            let slot = addr_of!((*(header as *const WAVEHDR)).dwUser).read_unaligned();
            queue.ready.get(slot).map(|ready| (queue, ready))
        }) else {
            return;
        };
        let (queue, ready) = ready;
        ready.store(true, Ordering::Release);
        queue.signal();
    }
}

struct WaveBuffer {
    data: NonNull<[u8; BUFFER_BYTES]>,
    header: NonNull<WAVEHDR>,
    prepared: bool,
    queued: bool,
}

impl WaveBuffer {
    fn new(slot: usize) -> Self {
        // Raw owning pointers keep storage stable and avoid producing Rust references
        // to native-owned memory when moving the WaveBuffer into its device.
        let data = Box::into_raw(Box::new([0u8; BUFFER_BYTES]));
        let header = Box::into_raw(Box::new(WAVEHDR {
            lpData: data.cast(),
            dwBufferLength: BUFFER_BYTES as u32,
            dwUser: slot,
            ..Default::default()
        }));
        Self {
            data: NonNull::new(data).expect("Box allocation is non-null"),
            header: NonNull::new(header).expect("Box allocation is non-null"),
            prepared: false,
            queued: false,
        }
    }

    fn append_returned(&self, output: &mut PcmWave) -> io::Result<bool> {
        // SAFETY: the caller only reaches this method after the WIM_DATA notification
        // or a successful waveInReset, which returns ownership of this buffer.
        let size =
            unsafe { addr_of!((*self.header.as_ptr()).dwBytesRecorded).read_unaligned() as usize };
        if size > BUFFER_BYTES {
            return Err(io::Error::other(
                "Windows returned an invalid microphone buffer.",
            ));
        }
        // SAFETY: the zero-initialized allocation is BUFFER_BYTES long and this
        // synchronous borrow ends before WinMM could requeue it.
        let samples = unsafe { slice::from_raw_parts(self.data.as_ptr().cast::<u8>(), size) };
        Ok(output.append(samples))
    }
}

impl Drop for WaveBuffer {
    fn drop(&mut self) {
        // SAFETY: WaveInput releases native ownership before dropping these two Box
        // allocations, or intentionally leaks them if native release fails.
        unsafe {
            drop(Box::from_raw(self.header.as_ptr()));
            drop(Box::from_raw(self.data.as_ptr()));
        }
    }
}

struct WaveInput {
    handle: HWAVEIN,
    notification: Arc<Notification>,
    buffers: Vec<WaveBuffer>,
    next: usize,
}

impl WaveInput {
    fn open(notification: Arc<Notification>) -> io::Result<Self> {
        let mut handle = null_mut();
        let format = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 1,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: PCM_BYTES_PER_SECOND,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        // SAFETY: WinMM reads format during this call. The callback and allocation
        // remain valid until waveInClose succeeds, including asynchronous WIM_DATA.
        multimedia(
            unsafe {
                waveInOpen(
                    &mut handle,
                    WAVE_MAPPER,
                    &format,
                    wave_input_callback as *const () as usize,
                    Arc::as_ptr(&notification) as usize,
                    CALLBACK_FUNCTION,
                )
            },
            "open",
        )?;
        let mut device = Self {
            handle,
            notification,
            buffers: Vec::with_capacity(BUFFER_COUNT),
            next: 0,
        };
        for slot in 0..BUFFER_COUNT {
            device.buffers.push(WaveBuffer::new(slot));
            let buffer = &mut device.buffers[slot];
            // SAFETY: handle is open and the stable WAVEHDR is initialized for WinMM.
            multimedia(
                unsafe { waveInPrepareHeader(handle, buffer.header.as_ptr(), HEADER_SIZE) },
                "prepare buffers for",
            )?;
            buffer.prepared = true;
            device.queue(slot)?;
        }
        Ok(device)
    }

    fn start(&self) -> io::Result<()> {
        // SAFETY: handle belongs to this initialized device and has not closed.
        multimedia(unsafe { waveInStart(self.handle) }, "start")
    }

    fn queue(&mut self, slot: usize) -> io::Result<()> {
        self.notification.ready[slot].store(false, Ordering::Relaxed);
        let buffer = &mut self.buffers[slot];
        // SAFETY: WinMM owns the prepared storage from success until its WIM_DATA event
        // or a successful stop/reset. Rust does not read or free memory during that time.
        multimedia(
            unsafe {
                addr_of!((*buffer.header.as_ptr()).dwBytesRecorded)
                    .cast_mut()
                    .write_unaligned(0);
                waveInAddBuffer(self.handle, buffer.header.as_ptr(), HEADER_SIZE)
            },
            "queue buffers for",
        )?;
        buffer.queued = true;
        Ok(())
    }

    fn record(&mut self) -> io::Result<Vec<u8>> {
        let mut pcm = PcmWave::new();
        while !self.notification.stopping.load(Ordering::Acquire) {
            self.notification.wait()?;
            if self.drain(&mut pcm, true)? {
                break;
            }
        }
        // SAFETY: both calls take the live device handle. Reset also synchronously
        // returns any buffers still owned by the driver, including the partial frame.
        multimedia(unsafe { waveInStop(self.handle) }, "stop")?;
        multimedia(unsafe { waveInReset(self.handle) }, "reset")?;
        self.drain(&mut pcm, false)?;
        Ok(pcm.finish())
    }

    fn drain(&mut self, pcm: &mut PcmWave, requeue: bool) -> io::Result<bool> {
        for _ in 0..BUFFER_COUNT {
            let slot = self.next;
            if !self.buffers[slot].queued
                || (requeue && !self.notification.ready[slot].swap(false, Ordering::AcqRel))
            {
                break;
            }
            let buffer = &mut self.buffers[slot];
            buffer.queued = false;
            self.next = (slot + 1) % BUFFER_COUNT;
            if buffer.append_returned(pcm)? {
                return Ok(true);
            }
            if requeue && !self.notification.stopping.load(Ordering::Acquire) {
                self.queue(slot)?;
            }
        }
        Ok(false)
    }
}

impl Drop for WaveInput {
    fn drop(&mut self) {
        // SAFETY: this thread exclusively manages the device. The pointer allocations
        // remain live until all native release calls have succeeded. On driver failure,
        // keep any storage (including callback data) it may still reference.
        let released = unsafe {
            let mut released = waveInReset(self.handle) == 0;
            for buffer in self.buffers.iter().filter(|buffer| buffer.prepared) {
                released &=
                    waveInUnprepareHeader(self.handle, buffer.header.as_ptr(), HEADER_SIZE) == 0;
            }
            (waveInClose(self.handle) == 0) && released
        };
        if !released {
            forget(take(&mut self.buffers));
            forget(Arc::clone(&self.notification));
        }
    }
}

fn multimedia(status: u32, operation: &str) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Could not {operation} the microphone (Windows audio error {status})."
        )))
    }
}

struct PcmWave(Vec<u8>);

impl PcmWave {
    fn new() -> Self {
        Self(vec![0; HEADER_BYTES])
    }

    fn append(&mut self, samples: &[u8]) -> bool {
        let remaining = HEADER_BYTES + MAX_PCM_BYTES - self.0.len();
        let count = samples.len().min(remaining) & !1;
        self.0.extend_from_slice(&samples[..count]);
        count == remaining
    }

    fn finish(mut self) -> Vec<u8> {
        let bytes = (self.0.len() - HEADER_BYTES) as u32;
        let header = &mut self.0[..HEADER_BYTES];
        header[0..4].copy_from_slice(b"RIFF");
        header[4..8].copy_from_slice(&(bytes + 36).to_le_bytes());
        header[8..16].copy_from_slice(b"WAVEfmt ");
        header[16..20].copy_from_slice(&16u32.to_le_bytes());
        header[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
        header[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
        header[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
        header[28..32].copy_from_slice(&PCM_BYTES_PER_SECOND.to_le_bytes());
        header[32..34].copy_from_slice(&2u16.to_le_bytes());
        header[34..36].copy_from_slice(&16u16.to_le_bytes());
        header[36..40].copy_from_slice(b"data");
        header[40..44].copy_from_slice(&bytes.to_le_bytes());
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_winforms_pcm_wire_format() {
        let mut capture = PcmWave::new();
        capture.append(&[1, 2, 3, 4, 5]);
        assert_eq!(
            capture.finish(),
            b"RIFF\x28\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\x04\0\0\0\x01\x02\x03\x04"
        );
    }

    #[test]
    fn caps_capture_at_five_minutes_and_trims_partial_samples() {
        let mut capture = PcmWave::new();
        assert!(capture.append(&vec![0xa3; MAX_PCM_BYTES + 3]));
        assert!(capture.append(&[0, 0]));
        let wav = capture.finish();
        assert_eq!(wav.len(), HEADER_BYTES + MAX_PCM_BYTES);
        assert_eq!(&wav[40..44], &(MAX_PCM_BYTES as u32).to_le_bytes());
        assert_eq!(*wav.last().unwrap(), 0xa3);
    }
}
