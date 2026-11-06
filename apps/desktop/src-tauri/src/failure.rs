//! What a Tauri command of the app fails with, as the window receives it: a rejected promise holding a `ClientFailure`
//! of `procly-client`, an [`AppFailure`], or a [`ProtocolMismatch`].
//!
//! The kinds are one enum, exported to TypeScript by ts-rs with the types of the protocol (ADR 0015), so that the
//! window reads the same list Rust writes and a kind added here is a type the window has to follow.

use serde::{Serialize, Serializer};

/// Which failure of the app an [`AppFailure`] stands for, the failures that are not the client's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub(crate) enum AppFailureKind {
    /// The thread that did the work stopped before it could.
    Task,
    /// The embedded `procly` cannot run.
    Sidecar,
    /// `procly daemon stop` failed.
    DaemonStop,
    /// `procly doctor` gave no document.
    Doctor,
    /// No editor is set, so the window opens Settings on the editor.
    NoEditor,
    /// The Worktree to open is not a folder any more.
    Missing,
    /// The app chosen to open a Worktree did not start.
    Launch,
    /// The Finder did not reveal the Worktree.
    Reveal,
    /// The settings could not be written.
    Settings,
    /// Open at login could not be read or changed.
    LoginItem,
    /// The permission to notify could not be asked, or the notification settings could not be opened.
    Notifications,
    /// The Seen set could not be written.
    Seen,
}

/// A failure of the app as it crosses to the window, with the message of the error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub(crate) struct AppFailure {
    pub(crate) kind: AppFailureKind,
    pub(crate) message: String,
}

/// Serializes `error` as an [`AppFailure`] of `kind`, its message the text of the error.
pub(crate) fn serialize<S: Serializer>(
    kind: AppFailureKind,
    error: &impl ToString,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    AppFailure {
        kind,
        message: error.to_string(),
    }
    .serialize(serializer)
}

/// A Daemon that answers in another protocol version. The window compares the 2 numbers to tell an older Daemon, which
/// can be restarted, from a newer one, which asks for an update of the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(export))]
pub(crate) struct ProtocolMismatch {
    #[cfg_attr(test, ts(type = "\"protocol_mismatch\""))]
    pub(crate) kind: &'static str,
    pub(crate) message: String,
    pub(crate) daemon_protocol: u32,
    pub(crate) app_protocol: u32,
}

/// The kind of a [`ProtocolMismatch`].
pub(crate) const PROTOCOL_MISMATCH: &str = "protocol_mismatch";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_of_the_app_reaches_the_window_with_its_kind_in_snake_case_and_its_message() {
        let json = serde_json::to_value(AppFailure {
            kind: AppFailureKind::DaemonStop,
            message: "the Daemon did not stop".to_owned(),
        })
        .expect("serializes");

        assert_eq!(
            json,
            serde_json::json!({ "kind": "daemon_stop", "message": "the Daemon did not stop" })
        );
    }
}
