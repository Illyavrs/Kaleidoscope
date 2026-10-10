//! Launcher foundations. Network redirection and client compatibility are separate gates.

#[cfg(unix)]
pub mod appimage;
pub mod endpoint;
pub mod host;
pub mod install;
pub mod process;
pub mod runtime;
mod storage;
pub mod updater;
