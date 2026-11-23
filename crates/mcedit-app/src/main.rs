#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod editor;
mod jobs;
mod tools;
mod ui;
#[cfg(target_os = "linux")]
mod wayland_dnd;

use std::path::PathBuf;

fn main() {
    tracing_subscriber::fmt().init();

    let initial_world = std::env::args_os().nth(1).map(PathBuf::from);
    if let Err(err) = app::McEditApp::run(initial_world) {
        eprintln!("MCEdit failed to start: {err}");
        std::process::exit(1);
    }
}
