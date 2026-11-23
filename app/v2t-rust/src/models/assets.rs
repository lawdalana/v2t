use std::ffi::OsStr;
#[cfg(not(target_os = "linux"))]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::redirect::Policy;
use sha2::{Digest as _, Sha256};
#[cfg(not(target_os = "linux"))]
use zip::ZipArchive;

#[cfg(not(target_os = "linux"))]
use super::{CORRECTION_ENGINE_PATH, SPEECH_ENGINE_PATH};
use super::{CORRECTION_MODEL_FILE, SpeechModel, random_hex};
use crate::inference::InferenceError;

#[cfg(not(target_os = "linux"))]
const MAX_ARCHIVE_BYTES: u64 = 1 << 30;
#[cfg(not(target_os = "linux"))]
const MAX_EXPANDED_BYTES: u64 = 2 << 30;
const MAX_MODEL_BYTES: u64 = 4 << 30;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Asset {
    pub(super) url: &'static str,
    pub(super) sha256: &'static str,
}

#[cfg(not(target_os = "linux"))]
const SPEECH_ENGINE: Asset = Asset {
    url: "https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip",
    sha256: "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
};

#[cfg(not(target_os = "linux"))]
const CORRECTION_ENGINE: Asset = Asset {
    url: "https://github.com/ggml-org/llama.cpp/releases/download/b12633/llama-b12633-bin-win-vulkan-x64.zip",
    sha256: "720d59998a51423a1c50c99326a5d5358b80e89d75f8fdafc50a26479d1bad68",
};

const CORRECTION_MODEL: Asset = Asset {
    url: "https://huggingface.co/typhoon-ai/typhoon2.5-qwen3-4b-gguf/resolve/386874d02430a544367cb0878687f8983751bbd2/typhoon2.5-qwen3-4b-q4_k_m.gguf?download=true",
    sha256: "b365302922688f3a3c9ac8e3c00ab97a152cac0cdbf4eb5a734ecb483ae3e511",
};

pub(super) fn install_speech_asset(
    directory: &Path,
    model: SpeechModel,
    mut report: impl FnMut(&str),
) -> Result<(), InferenceError> {
    fs::create_dir_all(directory).map_err(installation_error)?;
    let client = download_client(Duration::from_secs(600))?;
    #[cfg(not(target_os = "linux"))]
    if !directory.join(SPEECH_ENGINE_PATH).is_file() {
        report("Downloading the local engine…");
        install_engine(
            &client,
            directory,
            "b5454",
            Path::new("Release").join("whisper-server.exe").as_path(),
            SPEECH_ENGINE,
        )?;
    }
    let destination = directory.join(model.file);
    if !destination.is_file() {
        report(&format!("Downloading {} (547 MiB)…", model.name));
        download(
            &client,
            model.asset,
            &destination,
            MAX_MODEL_BYTES,
            |percent| {
                report(&format!("Downloading {}… {percent}%", model.name));
            },
        )?;
    }
    Ok(())
}

pub(super) fn install_correction_assets(
    directory: &Path,
    mut report: impl FnMut(&str),
) -> Result<(), InferenceError> {
    fs::create_dir_all(directory).map_err(installation_error)?;
    let client = download_client(Duration::from_secs(900))?;
    #[cfg(not(target_os = "linux"))]
    if !directory.join(CORRECTION_ENGINE_PATH).is_file() {
        report("Downloading the AI engine…");
        install_engine(
            &client,
            directory,
            "b12633",
            Path::new("llama-server.exe"),
            CORRECTION_ENGINE,
        )?;
    }
    let destination = directory.join(CORRECTION_MODEL_FILE);
    if !destination.is_file() {
        report("Downloading the Typhoon correction model (2.3 GiB)…");
        download(
            &client,
            CORRECTION_MODEL,
            &destination,
            MAX_MODEL_BYTES,
            |percent| {
                report(&format!("Downloading Typhoon… {percent}%"));
            },
        )?;
    }
    Ok(())
}

fn download_client(timeout: Duration) -> Result<Client, InferenceError> {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .redirect(Policy::custom(|attempt| {
            if attempt.url().scheme() != "https" || attempt.previous().len() >= 10 {
                attempt.error("Unsafe model download redirect")
            } else {
                attempt.follow()
            }
        }))
        .build()
        .map_err(|_| InferenceError::Unavailable("Could not initialize model downloads."))
}

#[cfg(not(target_os = "linux"))]
fn install_engine(
    client: &Client,
    directory: &Path,
    version: &str,
    executable: &Path,
    asset: Asset,
) -> Result<(), InferenceError> {
    let staging = TemporaryPath::new(&directory.join(version), ".staging", true)?;
    fs::create_dir(&staging.path).map_err(installation_error)?;
    let archive = staging.path.join("engine.zip");
    download(client, asset, &archive, MAX_ARCHIVE_BYTES, |_| {})?;
    let expanded = staging.path.join("engine");
    fs::create_dir(&expanded).map_err(installation_error)?;
    unpack(&archive, &expanded)?;
    if !expanded.join(executable).is_file() {
        return Err(InferenceError::Unavailable(
            "The verified archive did not contain the local engine.",
        ));
    }
    publish_asset(&expanded, &directory.join(version), Some(executable))
}

fn download(
    client: &Client,
    asset: Asset,
    destination: &Path,
    limit: u64,
    mut report: impl FnMut(u64),
) -> Result<(), InferenceError> {
    let temporary = TemporaryPath::new(destination, ".part", false)?;
    let mut response = client
        .get(asset.url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|_| InferenceError::Transport("Model download"))?;
    let length = response.content_length().filter(|length| *length != 0);
    if length.is_some_and(|length| length > limit) {
        return Err(InferenceError::Unavailable(
            "The model download exceeded its size limit.",
        ));
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary.path)
        .map_err(installation_error)?;
    let mut previous = None;
    stream_verified(&mut response, &mut file, asset.sha256, limit, |received| {
        if let Some(length) = length {
            let percent = (100 * received / length).min(100);
            if previous != Some(percent) {
                previous = Some(percent);
                report(percent);
            }
        }
    })?;
    file.sync_all().map_err(installation_error)?;
    drop(file);
    publish_asset(&temporary.path, destination, None)
}

fn stream_verified(
    mut input: impl Read,
    mut output: impl Write,
    expected: &str,
    limit: u64,
    mut progress: impl FnMut(u64),
) -> Result<(), InferenceError> {
    let mut received = 0;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 128 * 1024];
    loop {
        let count = input
            .read(&mut buf)
            .map_err(|_| InferenceError::Transport("Model download"))?;
        if count == 0 {
            break;
        }
        received += count as u64;
        if received > limit {
            return Err(InferenceError::Unavailable(
                "The model download exceeded its size limit.",
            ));
        }
        let block = &buf[..count];
        output.write_all(block).map_err(installation_error)?;
        hash.update(block);
        progress(received);
    }
    if !format!("{:x}", hash.finalize()).eq_ignore_ascii_case(expected) {
        return Err(InferenceError::Unavailable(
            "Downloaded file failed its checksum.",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn unpack(archive: &Path, directory: &Path) -> Result<(), InferenceError> {
    let file = File::open(archive).map_err(installation_error)?;
    let mut archive = ZipArchive::new(file).map_err(|_| invalid_archive())?;
    let mut size = 0;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|_| invalid_archive())?;
        let name = entry.enclosed_name().ok_or_else(invalid_archive)?;
        if name.as_os_str().is_empty() || entry.is_symlink() {
            return Err(invalid_archive());
        }
        size = u64::checked_add(size, entry.size()).ok_or_else(invalid_archive)?;
        if size > MAX_EXPANDED_BYTES {
            return Err(invalid_archive());
        }
        let target = directory.join(name);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(installation_error)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(installation_error)?;
            }
            let mut output = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)
                .map_err(installation_error)?;
            let expected_size = entry.size();
            let written = io::copy(&mut entry.by_ref().take(expected_size + 1), &mut output)
                .map_err(installation_error)?;
            if written != expected_size {
                return Err(invalid_archive());
            }
        }
    }
    Ok(())
}

fn publish_asset(
    temporary: &Path,
    destination: &Path,
    required: Option<&Path>,
) -> Result<(), InferenceError> {
    match fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(_)
            if required.map_or_else(
                || destination.is_file(),
                |required| destination.join(required).is_file(),
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(installation_error(error)),
    }
}

struct TemporaryPath {
    path: PathBuf,
    directory: bool,
}

impl TemporaryPath {
    fn new(destination: &Path, suffix: &str, directory: bool) -> Result<Self, InferenceError> {
        let mut name = destination
            .file_name()
            .unwrap_or(OsStr::new("model"))
            .to_os_string();
        name.push(".");
        name.push(random_hex::<16>()?);
        name.push(suffix);
        Ok(Self {
            path: destination.with_file_name(name),
            directory,
        })
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        if self.directory {
            let _ = fs::remove_dir_all(&self.path);
        } else {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn installation_error(_error: io::Error) -> InferenceError {
    InferenceError::Unavailable(
        "Could not install the local model. Check available disk space and permissions.",
    )
}

#[cfg(not(target_os = "linux"))]
fn invalid_archive() -> InferenceError {
    InferenceError::Unavailable("The engine archive could not be safely extracted.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_checksum_never_produces_a_verified_asset() {
        let bytes = b"verified asset";
        let expected = format!("{:x}", Sha256::digest(bytes));
        let mut output = Vec::new();
        stream_verified(&bytes[..], &mut output, &expected, 64, |_| {}).unwrap();
        assert_eq!(output, bytes);
        let mut output = Vec::new();
        assert!(stream_verified(&bytes[..], &mut output, &"0".repeat(64), 64, |_| {}).is_err());
        assert!(stream_verified(&bytes[..], &mut output, &expected, 3, |_| {}).is_err());
    }
}
