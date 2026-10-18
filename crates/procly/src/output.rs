//! The bounded buffer holding the recent output of a Job or a Service.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

/// The recent output kept per Job or Service.
pub(crate) const CAPACITY: usize = 1024 * 1024;

/// A ring buffer of raw output bytes that drops its oldest lines once full.
#[derive(Debug)]
pub(crate) struct OutputBuffer {
    bytes: VecDeque<u8>,
    capacity: usize,
}

impl OutputBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            capacity,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
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
    fn turns_terminal_line_endings_into_newlines() {
        let mut buffer = OutputBuffer::new(64);
        buffer.push(b"hello\r");
        buffer.push(b"\nworld\r\n");
        assert_eq!(buffer.text(), "hello\nworld\n");
    }
}
