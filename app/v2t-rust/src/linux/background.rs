use std::{
    fs::{self, DirBuilder, File, OpenOptions, TryLockError},
    io::{self, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{Storage, audio};
use crate::{
    inference::{InferenceError, MIXED, SpeechResult},
    models::LocalModels,
    speech_gate,
    text::Corrections,
};

const SOCKET: &str = "worker-v1.sock";
const REQUEST_LIMIT: usize = 64 * 1024;
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const MAX_TIMEOUT_MS: u32 = 90_000;
pub(super) const CORRECTION_STARTUP_TIMEOUT: Duration = Duration::from_secs(55);

#[derive(Serialize, Deserialize)]
enum Request {
    Speech {
        language: Option<String>,
        timeout_ms: u32,
        wave_bytes: u32,
    },
    Correct {
        text: String,
        protected: Vec<String>,
        timeout_ms: u32,
    },
    Stop,
}

#[derive(Serialize, Deserialize)]
struct SpeechReply {
    model: Option<String>,
    result: Result<SpeechResult, Failure>,
}

#[derive(Serialize, Deserialize)]
enum Failure {
    UnsupportedLanguage,
    InvalidInput,
    Timeout,
    Unavailable,
}

impl From<InferenceError> for Failure {
    fn from(error: InferenceError) -> Self {
        match error {
            InferenceError::UnsupportedLanguage => Self::UnsupportedLanguage,
            InferenceError::InvalidInput(_) => Self::InvalidInput,
            InferenceError::Timeout(_) => Self::Timeout,
            _ => Self::Unavailable,
        }
    }
}

impl From<Failure> for InferenceError {
    fn from(error: Failure) -> Self {
        match error {
            Failure::UnsupportedLanguage => Self::UnsupportedLanguage,
            Failure::InvalidInput => {
                Self::InvalidInput("The background worker rejected the input.")
            }
            Failure::Timeout => Self::Timeout("Local model"),
            Failure::Unavailable => {
                Self::Unavailable("The local background model could not complete the request.")
            }
        }
    }
}

pub(super) fn run(storage: &Storage) -> io::Result<()> {
    let Some(socket) = ServiceSocket::bind(&storage.data)? else {
        return Ok(());
    };
    let mut models = LocalModels::new(storage.data.clone()).map_err(io::Error::other)?;
    let quit = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, quit.clone())?;
    }

    // Preload installed assets only. Login launches never download models or send
    // cloud requests. A later request can load a different installed speech model.
    let correction_enabled = Corrections::load(&storage.config)
        .ok()
        .is_some_and(|settings| settings.enable_ai);
    #[cfg(target_os = "macos")]
    let correction_enabled = !super::macos::uses_litellm(&storage.config)
        && (correction_enabled || super::macos::correction_enabled(&storage.config));
    let correction = correction_enabled && models.correction_available();
    let speech = models.speech_available(Some(MIXED));
    let warmup_budget = Duration::from_secs(55);
    match (speech, correction) {
        (true, true) => {
            let _ = models.warm_both(Some(MIXED), warmup_budget);
        }
        (true, false) => {
            let _ = models.warm_speech(Some(MIXED), warmup_budget);
        }
        (false, true) => {
            let _ = models.warm_correction(warmup_budget);
        }
        (false, false) => {}
    }

    while !quit.load(Ordering::Relaxed) {
        match socket.listener.accept() {
            Ok((mut connection, _)) => match serve(&mut connection, &mut models) {
                Ok(true) => break,
                Ok(false) => {}
                Err(_) => eprintln!("v2t: Could not complete a local background request."),
            },
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100))
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn serve(connection: &mut UnixStream, models: &mut LocalModels) -> io::Result<bool> {
    // macOS can inherit nonblocking mode from the listener on accept.
    connection.set_nonblocking(false)?;
    connection.set_read_timeout(Some(Duration::from_secs(3)))?;
    connection.set_write_timeout(Some(Duration::from_secs(3)))?;
    let reply_timeout =
        |millis: u32| Duration::from_millis(u64::from(millis.clamp(1, MAX_TIMEOUT_MS)));
    match receive(connection, REQUEST_LIMIT)? {
        Request::Speech {
            language,
            timeout_ms,
            wave_bytes,
        } => {
            if !(audio::HEADER
                ..=audio::HEADER + usize::from(audio::MAX_SECONDS) * audio::BYTES_PER_SECOND)
                .contains(&(wave_bytes as usize))
            {
                return Err(invalid_frame());
            }
            let mut wave = vec![0; wave_bytes as usize];
            connection.read_exact(&mut wave)?;
            let result = if speech_gate::has_speech(&wave) {
                models
                    .speech(&wave, language.as_deref(), reply_timeout(timeout_ms))
                    .map_err(Failure::from)
            } else {
                Err(Failure::InvalidInput)
            };
            let model = models.loaded_speech().map(|model| model.file.to_owned());
            send(connection, &SpeechReply { model, result }, RESPONSE_LIMIT)?;
        }
        Request::Correct {
            text,
            protected,
            timeout_ms,
        } => {
            let timeout = reply_timeout(timeout_ms);
            let result = models
                .warm_correction(CORRECTION_STARTUP_TIMEOUT)
                .and_then(|()| models.correct(&text, &protected, timeout))
                .map_err(Failure::from);
            send(connection, &result, RESPONSE_LIMIT)?;
        }
        Request::Stop => {
            send(connection, &true, RESPONSE_LIMIT)?;
            return Ok(true);
        }
    }
    Ok(false)
}

type SpeechAttempt = (Option<String>, Result<SpeechResult, InferenceError>);

pub(super) fn speech(
    data: &Path,
    wave: &[u8],
    language: Option<&str>,
    timeout: Duration,
) -> Option<SpeechAttempt> {
    let connection = connect(data)?;
    let timeout_ms = millis(timeout);
    let wave_bytes = wave.len().try_into().ok()?;
    let request = Request::Speech {
        language: language.map(str::to_owned),
        timeout_ms,
        wave_bytes,
    };
    let result: io::Result<SpeechReply> = request_reply(connection, &request, wave, timeout);
    Some(match result {
        Ok(reply) => (reply.model, reply.result.map_err(InferenceError::from)),
        Err(error) => (None, Err(io_failure(error))),
    })
}

pub(super) fn correct(
    data: &Path,
    text: &str,
    protected: &[String],
    timeout: Duration,
) -> Option<Result<Option<String>, InferenceError>> {
    let connection = connect(data)?;
    let request = Request::Correct {
        text: text.to_owned(),
        protected: protected.to_vec(),
        timeout_ms: millis(timeout),
    };
    let result: io::Result<Result<Option<String>, Failure>> = request_reply(
        connection,
        &request,
        &[],
        CORRECTION_STARTUP_TIMEOUT + timeout,
    );
    Some(
        result
            .map_err(io_failure)
            .and_then(|result| result.map_err(InferenceError::from)),
    )
}

pub(super) fn stop(data: &Path) -> io::Result<bool> {
    let Some(connection) = connect(data) else {
        return Ok(false);
    };
    request_reply(connection, &Request::Stop, &[], Duration::from_secs(5))
}

fn millis(duration: Duration) -> u32 {
    duration.as_millis().clamp(1, u128::from(MAX_TIMEOUT_MS)) as u32
}

fn connect(data: &Path) -> Option<UnixStream> {
    let dir = data.join("run");
    private_directory(&dir).ok()?;
    let path = dir.join(SOCKET);
    let metadata = fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_socket() || metadata.uid() != dir.metadata().ok()?.uid() {
        return None;
    }
    UnixStream::connect(path).ok()
}

fn request_reply<T: DeserializeOwned>(
    mut connection: UnixStream,
    request: &Request,
    wave: &[u8],
    timeout: Duration,
) -> io::Result<T> {
    connection.set_write_timeout(Some(Duration::from_secs(3)))?;
    connection.set_read_timeout(Some(timeout))?;
    send(&mut connection, request, REQUEST_LIMIT)?;
    connection.write_all(wave)?;
    receive(&mut connection, RESPONSE_LIMIT)
}

fn io_failure(error: io::Error) -> InferenceError {
    if matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        InferenceError::Timeout("Local background model")
    } else {
        InferenceError::Unavailable("Could not communicate with the local background model.")
    }
}

fn send(connection: &mut impl Write, value: &impl Serialize, limit: usize) -> io::Result<()> {
    let json = serde_json::to_vec(value).map_err(io::Error::other)?;
    if json.is_empty() || json.len() > limit {
        return Err(invalid_frame());
    }
    connection.write_all(&(json.len() as u32).to_le_bytes())?;
    connection.write_all(&json)
}

fn receive<T: DeserializeOwned>(connection: &mut impl Read, limit: usize) -> io::Result<T> {
    let mut header = [0; 4];
    connection.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > limit {
        return Err(invalid_frame());
    }
    let mut payload = vec![0; length];
    connection.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(|_| invalid_frame())
}

fn invalid_frame() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid local background request.",
    )
}

struct ServiceSocket {
    listener: UnixListener,
    path: PathBuf,
    lock: File,
}

impl ServiceSocket {
    fn bind(data: &Path) -> io::Result<Option<Self>> {
        let dir = data.join("run");
        if !dir.exists() {
            DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        }
        private_directory(&dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("worker.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => return Err(error),
        }
        let path = dir.join(SOCKET);
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_socket() && metadata.uid() == dir.metadata()?.uid() =>
            {
                fs::remove_file(&path)?
            }
            Ok(_) => {
                return Err(io::Error::other(
                    "The background socket path belongs to another file.",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        let socket = Self {
            listener,
            path,
            lock,
        };
        socket.listener.set_nonblocking(true)?;
        Ok(Some(socket))
    }
}

impl Drop for ServiceSocket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        // Release the open-file lock even if a child briefly inherited its descriptor.
        let _ = self.lock.unlock();
    }
}

fn private_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The v2t background socket directory must be private.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let mut id = [0u8; 4];
            getrandom::fill(&mut id).unwrap();
            // Keep even the test path beneath the AF_UNIX length limit on macOS.
            Self(std::env::temp_dir().join(format!("v2t-{:08x}", u32::from_le_bytes(id))))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn length_prefix_rejects_oversized_payloads_before_reading() {
        let mut input = &u32::MAX.to_le_bytes()[..];
        assert_eq!(
            receive::<Request>(&mut input, REQUEST_LIMIT)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let mut wire = Vec::new();
        send(&mut wire, &Request::Stop, REQUEST_LIMIT).unwrap();
        assert!(matches!(
            receive::<Request>(&mut wire.as_slice(), REQUEST_LIMIT),
            Ok(Request::Stop)
        ));
    }

    #[test]
    fn accepted_connection_waits_for_the_request() {
        let fixture = Fixture::new();
        let socket = ServiceSocket::bind(&fixture.0).unwrap().unwrap();
        let client = UnixStream::connect(&socket.path).unwrap();
        let (mut connection, _) = socket.listener.accept().unwrap();
        let mut models = LocalModels::new(fixture.0.clone()).unwrap();
        let client = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            request_reply::<bool>(client, &Request::Stop, &[], Duration::from_secs(2))
        });
        let served = serve(&mut connection, &mut models);
        assert!(served.unwrap());
        assert!(client.join().unwrap().unwrap());
    }

    #[test]
    fn private_socket_prevents_duplicate_daemons_and_removes_its_own_path() {
        let fixture = Fixture::new();
        let server = ServiceSocket::bind(&fixture.0).unwrap().unwrap();
        assert!(ServiceSocket::bind(&fixture.0).unwrap().is_none());
        let mut client = connect(&fixture.0).unwrap();
        send(&mut client, &Request::Stop, REQUEST_LIMIT).unwrap();
        let (mut server_connection, _) = server.listener.accept().unwrap();
        assert!(matches!(
            receive::<Request>(&mut server_connection, REQUEST_LIMIT),
            Ok(Request::Stop)
        ));
        // A concurrent fork before exec may temporarily inherit the open lock.
        let inherited_lock = server.lock.try_clone().unwrap();
        drop(server);
        assert!(connect(&fixture.0).is_none());
        assert!(ServiceSocket::bind(&fixture.0).unwrap().is_some());
        drop(inherited_lock);
    }

    #[test]
    fn refuses_a_socket_directory_readable_by_other_users() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("run");
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            ServiceSocket::bind(&fixture.0).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(connect(&fixture.0).is_none());
    }
}
