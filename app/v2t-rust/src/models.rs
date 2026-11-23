mod assets;
#[cfg(target_os = "linux")]
mod linux;

use std::fmt::Write as _;
use std::net::{Ipv4Addr, TcpListener};
#[cfg(windows)]
use std::os::windows::{
    io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
    process::CommandExt as _,
};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use reqwest::blocking::Client;

use self::assets::{Asset, install_correction_assets, install_speech_asset};
use crate::inference::{self, InferenceError, MIXED, SpeechResult};

#[cfg(not(target_os = "linux"))]
const SPEECH_ENGINE_PATH: &str = "b5454/Release/whisper-server.exe";
#[cfg(not(target_os = "linux"))]
const CUDA_ENGINE_PATH: &str = "b5454-cuda/Release/whisper-server.exe";
#[cfg(not(target_os = "linux"))]
const CORRECTION_ENGINE_PATH: &str = "b12633/llama-server.exe";
pub const CORRECTION_MODEL_FILE: &str = "typhoon2.5-qwen3-4b-q4_k_m.gguf";
const STARTUP_POLL: Duration = Duration::from_millis(120);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpeechModel {
    pub name: &'static str,
    pub file: &'static str,
    asset: Asset,
    mixed_prompt: bool,
}

const WHISPER: SpeechModel = SpeechModel {
    name: "Whisper Large V3 Turbo",
    file: "ggml-large-v3-turbo-q5_0.bin",
    asset: Asset {
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin?download=true",
        sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    },
    mixed_prompt: true,
};

const TYPHOON: SpeechModel = SpeechModel {
    name: "Typhoon Whisper Turbo",
    file: "ggml-typhoon-whisper-turbo-q5_0.bin",
    asset: Asset {
        url: "https://huggingface.co/earyh774/typhoon-whisper-turbo-ggml/resolve/1881c5ba3e838e6ae22e11227d36736dde9c924e/ggml-typhoon-whisper-turbo-q5_0.bin?download=true",
        sha256: "2ffbb16719beda21f92f351ce66cef1e65cc2996b78556d1f77cbcc94ab88e67",
    },
    mixed_prompt: false,
};

fn preferred(language: Option<&str>) -> SpeechModel {
    if language.is_some_and(|language| {
        language.eq_ignore_ascii_case("th") || language.eq_ignore_ascii_case(MIXED)
    }) {
        TYPHOON
    } else {
        WHISPER
    }
}

fn select_speech(
    language: Option<&str>,
    installed: impl Fn(SpeechModel) -> bool,
) -> Option<SpeechModel> {
    let model = preferred(language);
    if installed(model) {
        Some(model)
    } else if model == TYPHOON && installed(WHISPER) {
        Some(WHISPER)
    } else {
        None
    }
}

pub struct LocalModels {
    speech_dir: PathBuf,
    correction_dir: PathBuf,
    http: Client,
    speech_worker: Option<SpeechWorker>,
    correction_worker: Option<CorrectionWorker>,
}

struct SpeechWorker {
    worker: Worker,
    model: SpeechModel,
    cuda: bool,
}

struct CorrectionWorker {
    worker: Worker,
    key: String,
}

impl LocalModels {
    pub fn new(data_dir: PathBuf) -> Result<Self, InferenceError> {
        Ok(Self {
            speech_dir: data_dir.join("Local"),
            correction_dir: data_dir.join("Correction"),
            http: inference::loopback_client()?,
            speech_worker: None,
            correction_worker: None,
        })
    }

    fn speech_installed(&self, model: SpeechModel) -> bool {
        self.speech_dir.join(model.file).is_file()
            && self.speech_engine().is_ok_and(|path| path.is_file())
    }

    fn speech_engine(&self) -> Result<PathBuf, InferenceError> {
        #[cfg(target_os = "linux")]
        {
            linux::Engine::Speech.resolve()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(self.speech_dir.join(SPEECH_ENGINE_PATH))
        }
    }

    fn correction_engine(&self) -> Result<PathBuf, InferenceError> {
        #[cfg(target_os = "linux")]
        {
            linux::Engine::Correction.resolve()
        }
        #[cfg(not(target_os = "linux"))]
        {
            Ok(self.correction_dir.join(CORRECTION_ENGINE_PATH))
        }
    }

    pub fn speech_available(&self, language: Option<&str>) -> bool {
        select_speech(language, |model| self.speech_installed(model)).is_some()
    }

    pub fn preferred_speech_installed(&self, language: Option<&str>) -> bool {
        self.speech_installed(preferred(language))
    }

    pub fn correction_available(&self) -> bool {
        self.correction_engine().is_ok_and(|path| path.is_file())
            && self.correction_dir.join(CORRECTION_MODEL_FILE).is_file()
    }

    pub fn speech_is_cuda(&self) -> bool {
        self.speech_worker
            .as_ref()
            .is_some_and(|worker| worker.cuda)
    }

    pub fn loaded_speech(&self) -> Option<SpeechModel> {
        self.speech_worker.as_ref().map(|worker| worker.model)
    }

    pub fn warm_both(
        &mut self,
        language: Option<&str>,
        timeout: Duration,
    ) -> (
        Result<SpeechModel, InferenceError>,
        Result<(), InferenceError>,
    ) {
        let mut speech = Self {
            speech_dir: self.speech_dir.clone(),
            correction_dir: self.correction_dir.clone(),
            http: self.http.clone(),
            speech_worker: self.speech_worker.take(),
            correction_worker: None,
        };
        thread::scope(|scope| {
            let speech = thread::Builder::new().spawn_scoped(scope, move || {
                let result = speech.warm_speech(language, timeout);
                (result, speech.speech_worker)
            });
            let correction = self.warm_correction(timeout);
            let speech = match speech.ok().and_then(|handle| handle.join().ok()) {
                Some((result, worker)) => {
                    self.speech_worker = worker;
                    result
                }
                None => Err(InferenceError::Unavailable(
                    "Could not warm the local speech worker.",
                )),
            };
            (speech, correction)
        })
    }

    pub fn warm_speech(
        &mut self,
        language: Option<&str>,
        timeout: Duration,
    ) -> Result<SpeechModel, InferenceError> {
        let language = inference::normalize_language(language)?;
        let cpu = self.speech_engine()?;
        let model = select_speech(language.as_deref(), |model| {
            self.speech_dir.join(model.file).is_file() && cpu.is_file()
        })
        .ok_or(InferenceError::Unavailable(speech_setup_message()))?;
        if self
            .speech_worker
            .as_mut()
            .is_some_and(|loaded| loaded.model == model && loaded.worker.running())
        {
            return Ok(model);
        }
        self.suspend_speech();

        let budget = Budget::new(timeout);
        #[cfg(not(target_os = "linux"))]
        let gpu = self.speech_dir.join(CUDA_ENGINE_PATH);
        #[cfg(not(target_os = "linux"))]
        let worker = if gpu.is_file() {
            match self.start_speech(&gpu, model, true, budget) {
                Ok(worker) => worker,
                Err(_) => self.start_speech(&cpu, model, false, budget)?,
            }
        } else {
            self.start_speech(&cpu, model, false, budget)?
        };
        #[cfg(target_os = "linux")]
        let worker = self.start_speech(&cpu, model, false, budget)?;
        self.speech_worker = Some(worker);
        Ok(model)
    }

    fn start_speech(
        &self,
        executable: &Path,
        model: SpeechModel,
        cuda: bool,
        total_budget: Budget,
    ) -> Result<SpeechWorker, InferenceError> {
        let stage = "Local speech model startup";
        let budget = Budget::new(total_budget.remaining(stage)?.min(Duration::from_secs(35)));
        let listener = reserve_port()?;
        let port = port_string(&listener)?;
        let route = format!("/{}", random_hex::<16>()?);
        let threads = thread::available_parallelism()
            .map_or(2, |threads| (threads.get() / 2).clamp(2, 8))
            .to_string();
        let mut command = command_for(executable);
        command
            .args(["--host", "127.0.0.1", "--port", &port, "--model"])
            .arg(self.speech_dir.join(model.file))
            .args([
                "--language",
                "auto",
                "--threads",
                &threads,
                "--no-timestamps",
                "--request-path",
                &route,
            ]);
        let worker = Worker::start(command, listener, format!("http://127.0.0.1:{port}{route}"))?;
        let worker = worker.wait_ready(&self.http, budget, stage)?;
        Ok(SpeechWorker {
            worker,
            model,
            cuda,
        })
    }

    pub fn speech(
        &mut self,
        wave: &[u8],
        language: Option<&str>,
        timeout: Duration,
    ) -> Result<SpeechResult, InferenceError> {
        let budget = Budget::new(timeout);
        let model = self.warm_speech(language, timeout)?;
        let loaded = self
            .speech_worker
            .as_ref()
            .ok_or(InferenceError::Unavailable(
                "The local model worker is unavailable.",
            ))?;
        inference::local_speech(
            &self.http,
            &loaded.worker.endpoint("inference"),
            wave,
            language,
            model.mixed_prompt,
            budget.remaining("Local transcription")?,
        )
    }

    pub fn warm_correction(&mut self, timeout: Duration) -> Result<(), InferenceError> {
        let executable = self.correction_engine()?;
        if !executable.is_file() || !self.correction_dir.join(CORRECTION_MODEL_FILE).is_file() {
            #[cfg(target_os = "linux")]
            let message = "Install the local AI model with v2t setup --correction.";
            #[cfg(not(target_os = "linux"))]
            let message = "Install the local AI model in Dictionary & AI.";
            return Err(InferenceError::Unavailable(message));
        }
        if self
            .correction_worker
            .as_mut()
            .is_some_and(|worker| worker.worker.running())
        {
            return Ok(());
        }
        self.suspend_correction();
        let budget = Budget::new(timeout);
        let worker = if vulkan_present() {
            match self.start_correction(&executable, true, budget) {
                Ok(worker) => worker,
                Err(_) => self.start_correction(&executable, false, budget)?,
            }
        } else {
            self.start_correction(&executable, false, budget)?
        };
        self.correction_worker = Some(worker);
        Ok(())
    }

    fn start_correction(
        &self,
        executable: &Path,
        gpu: bool,
        total_budget: Budget,
    ) -> Result<CorrectionWorker, InferenceError> {
        let stage = "Local correction model startup";
        let budget = Budget::new(total_budget.remaining(stage)?.min(Duration::from_secs(30)));
        let listener = reserve_port()?;
        let port = port_string(&listener)?;
        let key = random_hex::<32>()?;
        let mut command = command_for(executable);
        command
            .args(["--host", "127.0.0.1", "--port", &port, "--model"])
            .arg(self.correction_dir.join(CORRECTION_MODEL_FILE))
            .args([
                "--alias",
                "v2t-correction",
                "--api-key",
                &key,
                "--no-webui",
                "--jinja",
                "--reasoning",
                "off",
                "--ctx-size",
                "4096",
                "--parallel",
                "1",
                "--threads",
                "6",
                "--n-gpu-layers",
                if gpu { "99" } else { "0" },
                "--log-disable",
            ]);
        if !gpu {
            command.args(["--device", "none"]);
        }
        let worker = Worker::start(command, listener, format!("http://127.0.0.1:{port}"))?;
        let worker = worker.wait_ready(&self.http, budget, stage)?;
        let prime = "ทดสอบ OpenRouter.";
        inference::local_correction(
            &self.http,
            &worker.endpoint("v1/chat/completions"),
            &key,
            prime,
            &["OpenRouter".to_owned()],
            total_budget.remaining(stage)?.min(Duration::from_secs(8)),
        )?;
        Ok(CorrectionWorker { worker, key })
    }

    pub fn correct(
        &mut self,
        text: &str,
        protected: &[String],
        timeout: Duration,
    ) -> Result<Option<String>, InferenceError> {
        let budget = Budget::new(timeout);
        self.warm_correction(timeout)?;
        let loaded = self
            .correction_worker
            .as_ref()
            .ok_or(InferenceError::Unavailable(
                "The local correction worker is unavailable.",
            ))?;
        inference::local_correction(
            &self.http,
            &loaded.worker.endpoint("v1/chat/completions"),
            &loaded.key,
            text,
            protected,
            budget
                .remaining("Local correction")?
                .min(Duration::from_secs(8)),
        )
    }

    pub fn suspend(&mut self) {
        self.suspend_speech();
        self.suspend_correction();
    }

    pub fn suspend_speech(&mut self) {
        self.speech_worker = None;
    }

    pub fn suspend_correction(&mut self) {
        self.correction_worker = None;
    }

    pub fn install_speech(
        &self,
        language: Option<&str>,
        report: impl FnMut(&str),
    ) -> Result<(), InferenceError> {
        let language = inference::normalize_language(language)?;
        #[cfg(target_os = "linux")]
        self.speech_engine()?;
        install_speech_asset(&self.speech_dir, preferred(language.as_deref()), report)
    }

    pub fn install_correction(&self, report: impl FnMut(&str)) -> Result<(), InferenceError> {
        #[cfg(target_os = "linux")]
        self.correction_engine()?;
        install_correction_assets(&self.correction_dir, report)
    }
}

fn speech_setup_message() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "Install the speech model with v2t setup and the same language."
    }
    #[cfg(not(target_os = "linux"))]
    {
        "Open v2t settings and install the local model."
    }
}

#[derive(Clone, Copy)]
struct Budget {
    start: Instant,
    duration: Duration,
}

impl Budget {
    fn new(duration: Duration) -> Self {
        Self {
            start: Instant::now(),
            duration,
        }
    }

    fn remaining(self, stage: &'static str) -> Result<Duration, InferenceError> {
        self.duration
            .checked_sub(self.start.elapsed())
            .filter(|duration| !duration.is_zero())
            .ok_or(InferenceError::Timeout(stage))
    }
}

struct Worker {
    child: Child,
    base: String,
    #[cfg(windows)]
    job: Option<OwnedHandle>,
}

impl Worker {
    fn start(
        mut command: Command,
        listener: TcpListener,
        base: String,
    ) -> Result<Self, InferenceError> {
        #[cfg(windows)]
        let job = create_kill_job()?;
        // The native workers bind their own port. Release the reservation immediately before spawning.
        drop(listener);
        let child = command
            .spawn()
            .map_err(|_| InferenceError::Unavailable("Could not launch the local model worker."))?;
        #[cfg(windows)]
        let child = attach_job(&job, child)?;
        Ok(Self {
            child,
            base,
            #[cfg(windows)]
            job: Some(job),
        })
    }

    fn wait_ready(
        mut self,
        http: &Client,
        budget: Budget,
        stage: &'static str,
    ) -> Result<Self, InferenceError> {
        let health = self.endpoint("health");
        loop {
            let probe = budget.remaining(stage)?.min(Duration::from_millis(400));
            if !self.running() {
                return Err(InferenceError::Unavailable(
                    "The local model worker exited before becoming ready.",
                ));
            }
            if http
                .get(&health)
                .timeout(probe)
                .send()
                .is_ok_and(|response| response.status().is_success())
                && self.running()
            {
                return Ok(self);
            }
            thread::sleep(budget.remaining(stage)?.min(STARTUP_POLL));
        }
    }

    fn running(&mut self) -> bool {
        self.child.try_wait().is_ok_and(|status| status.is_none())
    }

    fn endpoint(&self, route: &str) -> String {
        format!("{}/{route}", self.base)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        #[cfg(windows)]
        drop(self.job.take());
        if self.running() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(windows)]
fn create_kill_job() -> Result<OwnedHandle, InferenceError> {
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    // SAFETY: Both Win32 arguments may be null. The returned handle is owned and noninheritable.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(InferenceError::Unavailable(
            "Could not set up local model process ownership.",
        ));
    }
    // SAFETY: CreateJobObjectW returned a valid handle and this conversion takes sole ownership.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: The information pointer is valid for the supplied C structure size and the job is open.
    let success = unsafe {
        SetInformationJobObject(
            handle.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    };
    if success == 0 {
        return Err(InferenceError::Unavailable(
            "Could not set up local model process ownership.",
        ));
    }
    Ok(handle)
}

#[cfg(windows)]
fn attach_job(job: &OwnedHandle, mut child: Child) -> Result<Child, InferenceError> {
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    // SAFETY: Both handles are valid and retained for the Win32 call.
    let success = unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
    if success == 0 {
        if child.kill().is_ok() {
            let _ = child.wait();
        }
        return Err(InferenceError::Unavailable(
            "Could not attach the local worker to its process job.",
        ));
    }
    Ok(child)
}

fn command_for(executable: &Path) -> Command {
    let mut command = Command::new(executable);
    if let Some(directory) = executable.parent() {
        command.current_dir(directory);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    #[cfg(target_os = "linux")]
    linux::configure_command(&mut command);
    command
}

fn vulkan_present() -> bool {
    #[cfg(target_os = "linux")]
    {
        false
    }
    #[cfg(not(target_os = "linux"))]
    std::env::var_os("SystemRoot").is_some_and(|root| {
        PathBuf::from(root)
            .join("System32")
            .join("vulkan-1.dll")
            .is_file()
    })
}

fn reserve_port() -> Result<TcpListener, InferenceError> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|_| InferenceError::Unavailable("Could not reserve a port for the local model."))
}

fn port_string(listener: &TcpListener) -> Result<String, InferenceError> {
    listener
        .local_addr()
        .map(|address| address.port().to_string())
        .map_err(|_| InferenceError::Unavailable("Could not read the local model port."))
}

fn random_hex<const N: usize>() -> Result<String, InferenceError> {
    let mut random = [0u8; N];
    getrandom::fill(&mut random)
        .map_err(|_| InferenceError::Unavailable("Could not initialize secure randomness."))?;
    let mut output = String::with_capacity(N * 2);
    for byte in random {
        write!(output, "{byte:02x}").expect("writing to a String does not fail");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn dropping_a_local_worker_reaps_its_child() {
        let mut command = command_for(Path::new("/bin/sleep"));
        command.arg("30");
        let mut worker = Worker::start(command, reserve_port().unwrap(), String::new()).unwrap();
        let proc_entry = PathBuf::from("/proc").join(worker.child.id().to_string());
        assert!(worker.running());
        drop(worker);
        assert!(!proc_entry.exists());
    }

    #[cfg(windows)]
    #[test]
    fn closing_model_job_terminates_the_worker() {
        let system_root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let mut command = command_for(&system_root.join("System32").join("cmd.exe"));
        command.args(["/d", "/c", "ping -t 127.0.0.1 >NUL"]);
        let mut worker = Worker::start(command, reserve_port().unwrap(), String::new()).unwrap();
        assert!(worker.running());

        // Closing the only job handle simulates Windows closing this handle on application exit.
        drop(worker.job.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        while worker.running() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!worker.running(), "model worker survived closing its job");
    }

    #[test]
    fn thai_prefers_typhoon_and_uses_whisper_only_when_needed() {
        assert_eq!(preferred(Some("th")), TYPHOON);
        assert_eq!(preferred(Some("th+en")), TYPHOON);
        assert_eq!(preferred(Some("en")), WHISPER);
        assert_eq!(preferred(None), WHISPER);
        assert_eq!(
            select_speech(Some("th"), |model| model == WHISPER),
            Some(WHISPER)
        );
        assert_eq!(select_speech(Some("en"), |model| model == TYPHOON), None);
        assert_eq!(select_speech(Some("th+en"), |_| true), Some(TYPHOON));
    }
}
