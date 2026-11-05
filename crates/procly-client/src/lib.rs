//! Everything a client needs to speak to the Procly Daemon: the protocol, the framing of its socket and the state
//! directory that holds it. Shared by the `procly` CLI and the desktop app (ADR 0009), it renders nothing and knows no
//! `--json` document nor exit code.
//!
//! The client is blocking, built on `std::os::unix::net`: an async caller runs it on a thread that may block, such as
//! one of `tokio::task::spawn_blocking`.

mod client;
pub mod protocol;
pub mod state_dir;

pub use client::{ClientError, Connection, Responses, connect, connect_or_start};
