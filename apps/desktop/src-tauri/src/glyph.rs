//! The glyph of the menu bar icon: which of the 3 template images the last answers of the poller call for.
//!
//! The mark says the Daemon answers and nothing needs the developer, the mark with a bite says something does, and the
//! outline says no Daemon can be read. The count stays in the menu, never in the glyph.

use procly_client::ClientError;
use procly_client::protocol::{DecodeError, Response};

use crate::daemon::CommandError;
use crate::poller::Answer;

/// The mark, 36 px tall, as `scripts/export-glyphs.ts` draws it from `glyphs/mark.svg`.
const MARK_PNG: &[u8] = include_bytes!("../icons/tray-mark.png");

/// The mark with a bite and a dot, from `glyphs/mark-needs-you.svg`.
const NEEDS_YOU_PNG: &[u8] = include_bytes!("../icons/tray-mark-needs-you.png");

/// The outer outline of the mark, from `glyphs/outline.svg`.
const OUTLINE_PNG: &[u8] = include_bytes!("../icons/tray-outline.png");

/// How many answers in a row that say nothing about the Daemon it takes to call it unreadable.
const DOUBTFUL_BEFORE_OUTLINE: u32 = 2;

/// What the menu bar icon shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Glyph {
    /// The Daemon answers and "Needs you" is empty.
    Mark,
    /// The Daemon answers and "Needs you" is not empty.
    NeedsYou,
    /// No Daemon can be read.
    Outline,
}

impl Glyph {
    /// The template image of the glyph, black on transparent so that the system tints it.
    pub(crate) fn png(self) -> &'static [u8] {
        match self {
            Self::Mark => MARK_PNG,
            Self::NeedsYou => NEEDS_YOU_PNG,
            Self::Outline => OUTLINE_PNG,
        }
    }
}

/// What an answer tells about the Daemon.
enum Reading {
    /// The Daemon answered a `Status`, and "Needs you" is or is not empty.
    Answered { needs_you: bool },
    /// No Daemon can be read: no socket, a refused connection, or a Daemon of another protocol version.
    Unreadable,
    /// Something went wrong that does not say whether a Daemon runs, such as a request that timed out or a
    /// connection that broke while it was answered.
    Doubtful,
    /// A Daemon answered, but not with a `Status`, so it says nothing about "Needs you".
    Unrelated,
}

fn read(answer: &Answer, needs_you_is_empty: bool) -> Reading {
    match answer {
        Answer::Response(Response::Status(_)) => Reading::Answered {
            needs_you: !needs_you_is_empty,
        },
        Answer::Response(_) => Reading::Unrelated,
        Answer::Failure(CommandError::Client(
            ClientError::Unreachable { .. } | ClientError::Decode(DecodeError::Version(_)),
        )) => Reading::Unreadable,
        Answer::Failure(_) => Reading::Doubtful,
    }
}

/// The glyph on show and what the answers since have said, which is what a change of glyph is decided on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlyphTracker {
    shown: Glyph,
    /// How many answers in a row said nothing about the Daemon.
    doubtful: u32,
}

impl GlyphTracker {
    /// The state of an app that just launched: the mark, since no answer has failed yet. Showing the outline before
    /// the first failed connect would flash "cannot read" at every login for the length of one poll.
    pub(crate) fn launched() -> Self {
        Self {
            shown: Glyph::Mark,
            doubtful: 0,
        }
    }

    /// The glyph on show.
    pub(crate) fn shown(self) -> Glyph {
        self.shown
    }

    /// Takes an answer of the poller into account, and returns the glyph to show. `needs_you_is_empty` is the state
    /// of "Needs you" for that answer.
    ///
    /// A single answer that says nothing about the Daemon, a timeout for one, leaves the glyph as it was. A second one
    /// in a row means the Daemon cannot be read.
    pub(crate) fn observe(&mut self, answer: &Answer, needs_you_is_empty: bool) -> Glyph {
        match read(answer, needs_you_is_empty) {
            Reading::Answered { needs_you } => {
                self.doubtful = 0;
                self.shown = if needs_you { Glyph::NeedsYou } else { Glyph::Mark };
            }
            Reading::Unreadable => {
                self.doubtful = 0;
                self.shown = Glyph::Outline;
            }
            Reading::Doubtful => {
                self.doubtful = self.doubtful.saturating_add(1);
                if self.doubtful >= DOUBTFUL_BEFORE_OUTLINE {
                    self.shown = Glyph::Outline;
                }
            }
            Reading::Unrelated => {}
        }
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::path::PathBuf;

    use procly_client::protocol::StatusReport;

    use super::*;

    fn status() -> Answer {
        Answer::Response(Response::Status(StatusReport::default()))
    }

    fn no_socket() -> Answer {
        Answer::Failure(CommandError::Client(ClientError::Unreachable {
            path: PathBuf::from("/p/state/procly.sock"),
            source: io::Error::from(io::ErrorKind::NotFound),
        }))
    }

    fn timed_out() -> Answer {
        Answer::Failure(CommandError::Client(ClientError::Connection {
            path: PathBuf::from("/p/state/procly.sock"),
            source: io::Error::from(io::ErrorKind::TimedOut),
        }))
    }

    #[test]
    fn the_app_shows_the_mark_at_launch_before_any_answer() {
        assert_eq!(GlyphTracker::launched().shown(), Glyph::Mark);
    }

    #[test]
    fn a_daemon_that_answers_with_nothing_for_the_developer_is_the_mark() {
        let mut tracker = GlyphTracker::launched();

        assert_eq!(tracker.observe(&status(), true), Glyph::Mark);
    }

    #[test]
    fn a_daemon_that_answers_while_something_needs_the_developer_is_the_mark_with_a_bite() {
        let mut tracker = GlyphTracker::launched();

        assert_eq!(tracker.observe(&status(), false), Glyph::NeedsYou);
    }

    #[test]
    fn the_bite_goes_away_once_nothing_needs_the_developer() {
        let mut tracker = GlyphTracker::launched();
        tracker.observe(&status(), false);

        assert_eq!(tracker.observe(&status(), true), Glyph::Mark);
    }

    #[test]
    fn the_outline_comes_only_after_a_failed_connect() {
        let mut tracker = GlyphTracker::launched();
        assert_eq!(tracker.shown(), Glyph::Mark);

        assert_eq!(tracker.observe(&no_socket(), true), Glyph::Outline);
    }

    #[test]
    fn a_daemon_of_another_protocol_is_read_like_no_daemon() {
        let mut tracker = GlyphTracker::launched();
        let other = Answer::Failure(CommandError::Client(ClientError::Decode(DecodeError::Version(1))));

        assert_eq!(tracker.observe(&other, true), Glyph::Outline);
    }

    #[test]
    fn the_mark_comes_back_when_a_daemon_answers_again() {
        let mut tracker = GlyphTracker::launched();
        tracker.observe(&no_socket(), true);

        assert_eq!(tracker.observe(&status(), true), Glyph::Mark);
    }

    #[test]
    fn a_single_timeout_leaves_the_glyph_unchanged() {
        for before in [true, false] {
            let mut tracker = GlyphTracker::launched();
            let shown = tracker.observe(&status(), before);

            assert_eq!(tracker.observe(&timed_out(), true), shown);
        }
    }

    #[test]
    fn a_timeout_after_the_outline_leaves_the_outline() {
        let mut tracker = GlyphTracker::launched();
        tracker.observe(&no_socket(), true);

        assert_eq!(tracker.observe(&timed_out(), true), Glyph::Outline);
    }

    #[test]
    fn a_second_timeout_in_a_row_is_the_outline() {
        let mut tracker = GlyphTracker::launched();
        tracker.observe(&status(), false);
        tracker.observe(&timed_out(), true);

        assert_eq!(tracker.observe(&timed_out(), true), Glyph::Outline);
    }

    #[test]
    fn an_answer_between_two_timeouts_makes_each_of_them_the_first() {
        let mut tracker = GlyphTracker::launched();
        tracker.observe(&timed_out(), true);
        tracker.observe(&status(), true);

        assert_eq!(tracker.observe(&timed_out(), true), Glyph::Mark);
    }

    #[test]
    fn an_answer_that_is_not_a_status_changes_nothing() {
        let mut tracker = GlyphTracker::launched();
        tracker.observe(&status(), false);
        let error = Answer::Response(Response::Error {
            message: "no".to_owned(),
        });

        assert_eq!(tracker.observe(&error, true), Glyph::NeedsYou);
    }

    #[test]
    fn each_glyph_has_its_own_png_image() {
        let mut images = [Glyph::Mark, Glyph::NeedsYou, Glyph::Outline].map(Glyph::png).to_vec();
        for image in &images {
            assert!(image.starts_with(&[0x89, b'P', b'N', b'G']));
        }
        images.sort();
        images.dedup();

        assert_eq!(images.len(), 3);
    }
}
