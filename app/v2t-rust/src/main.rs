#![cfg_attr(windows, windows_subsystem = "windows")]
#![cfg_attr(not(windows), allow(dead_code))]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

mod config;
mod inference;
#[cfg_attr(windows, allow(unsafe_code))]
mod models;
mod resource;
mod shortcut;
mod speech_gate;
mod text;
mod usage;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod linux;

#[cfg(windows)]
mod app;
#[cfg(windows)]
#[allow(unsafe_code)]
mod capture;
#[cfg(windows)]
#[allow(unsafe_code)]
mod desktop;
#[cfg(windows)]
#[allow(unsafe_code)]
mod dialog;
#[cfg(windows)]
#[allow(unsafe_code)]
mod input;
#[cfg(windows)]
#[allow(unsafe_code)]
mod system;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_startup;

#[cfg(windows)]
fn main() {
    if let Err(error) = desktop::run() {
        desktop::startup_error(&error);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn main() -> std::process::ExitCode {
    linux::run()
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn main() -> std::process::ExitCode {
    eprintln!("v2t supports Windows, Linux, and macOS.");
    std::process::ExitCode::FAILURE
}
