//! The bounded buffer holding the recent output of a Job or a Service.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::daemon::clock::Clock;

/// The recent output kept per Job or Service.
pub(crate) const CAPACITY: usize = 1024 * 1024;

/// A ring buffer of raw output bytes that drops its oldest lines once full.
#[derive(Debug)]
pub(crate) struct OutputBuffer {
    bytes: VecDeque<u8>,
    capacity: usize,
    /// The clock that dates the output, when its time matters.
    clock: Option<Clock>,
    /// The reading of `clock` when the last bytes were pushed.
    printed_at: Option<Duration>,
}

impl OutputBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            capacity,
            clock: None,
            printed_at: None,
        }
    }

    /// A buffer that also tells when output last arrived, read on `clock` as it arrives, so that idleness is counted
    /// from when the Service printed rather than from when the Daemon next looked.
    pub(crate) fn dated(capacity: usize, clock: Clock) -> Self {
        Self {
            clock: Some(clock),
            ..Self::new(capacity)
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.printed_at = self.clock.as_ref().map(Clock::now);
        self.bytes.extend(chunk);
        if self.bytes.len() <= self.capacity {
            return;
        }
        self.bytes.drain(..self.bytes.len() - self.capacity);
        // Drop the line cut in half, unless the buffer holds nothing but that one line.
        if let Some(newline) = self.bytes.iter().position(|&byte| byte == b'\n') {
            self.bytes.drain(..=newline);
        }
    }

    /// The reading of the clock of a [`dated`](Self::dated) buffer when output last arrived, `None` before any did.
    pub(crate) fn printed_at(&self) -> Option<Duration> {
        self.printed_at
    }

    /// The buffered output as text, with the terminal's `\r\n` line endings turned back into `\n`.
    pub(crate) fn text(&self) -> String {
        let (front, back) = self.bytes.as_slices();
        String::from_utf8_lossy(&[front, back].concat()).replace("\r\n", "\n")
    }
}

/// Locks a shared output buffer.
pub(crate) fn lock(output: &Mutex<OutputBuffer>) -> MutexGuard<'_, OutputBuffer> {
    output
        .lock()
        .expect("an output buffer mutex is never poisoned, no code panics while holding it")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_everything_below_capacity() {
        let mut buffer = OutputBuffer::new(64);
        buffer.push(b"one\n");
        buffer.push(b"two\n");
        assert_eq!(buffer.text(), "one\ntwo\n");
    }

    #[test]
    fn drops_the_oldest_whole_lines_once_full() {
        let mut buffer = OutputBuffer::new(10);
        buffer.push(b"first\nsecond\nthird\n");
        assert_eq!(buffer.text(), "third\n");
    }

    #[test]
    fn keeps_the_tail_of_a_single_line_longer_than_the_capacity() {
        let mut buffer = OutputBuffer::new(4);
        buffer.push(b"abcdefgh");
        assert_eq!(buffer.text(), "efgh");
    }

    #[test]
    fn a_dated_buffer_tells_when_output_last_arrived_on_its_clock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clock");
        std::fs::write(&path, "1000").unwrap();
        let mut buffer = OutputBuffer::dated(4, Clock::file(path.clone()));
        assert_eq!(buffer.printed_at(), None);
        buffer.push(b"abcdefgh");
        std::fs::write(&path, "5000").unwrap();
        assert_eq!(buffer.printed_at(), Some(Duration::from_secs(1)));
        buffer.push(b"");
        assert_eq!(buffer.printed_at(), Some(Duration::from_secs(1)), "nothing was printed");
        buffer.push(b"ij");
        assert_eq!(buffer.printed_at(), Some(Duration::from_secs(5)));
    }

    #[test]
    fn turns_terminal_line_endings_into_newlines() {
        let mut buffer = OutputBuffer::new(64);
        buffer.push(b"hello\r");
        buffer.push(b"\nworld\r\n");
        assert_eq!(buffer.text(), "hello\nworld\n");
    }
}
