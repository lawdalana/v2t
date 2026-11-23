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

#[cfg(target_os = "linux")]
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
fn main() {
    if let Err(error) = desktop::run() {
        desktop::startup_error(&error);
    }
}

#[cfg(target_os = "linux")]
fn main() -> std::process::ExitCode {
    linux::run()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn main() -> std::process::ExitCode {
    eprintln!("v2t supports Windows and Linux.");
    std::process::ExitCode::FAILURE
}
