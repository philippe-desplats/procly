//! The bounded buffer holding the recent output of a Job or a Service, and the followers its output is handed to as it
//! arrives.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::mpsc::{self, error::TrySendError};

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
    /// Every follower of the output, each handed what is pushed after its backlog.
    followers: Vec<Follower>,
    /// The identifier of the next follower.
    next_follower: u64,
}

impl OutputBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::new(),
            capacity,
            clock: None,
            printed_at: None,
            followers: Vec::new(),
            next_follower: 0,
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
        self.hand_out(chunk);
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

impl OutputBuffer {
    /// Hands `chunk` to every follower without ever waiting for one: a follower whose queue is full misses it and is
    /// told how much it missed with the next chunk that gets through. A follower gone is forgotten.
    fn hand_out(&mut self, chunk: &[u8]) {
        self.followers.retain_mut(|follower| {
            let next = Chunk {
                dropped: follower.dropped,
                bytes: chunk.to_vec(),
            };
            match follower.sender.try_send(next) {
                Ok(()) => {
                    follower.dropped = 0;
                    true
                }
                Err(TrySendError::Full(_)) => {
                    follower.dropped += chunk.len() as u64;
                    true
                }
                Err(TrySendError::Closed(_)) => false,
            }
        });
    }

    /// Forgets the follower `id`, returning how many bytes it missed since the last chunk that got through.
    fn unfollow(&mut self, id: u64) -> u64 {
        let position = self.followers.iter().position(|follower| follower.id == id);
        position.map_or(0, |position| self.followers.swap_remove(position).dropped)
    }
}

/// How many chunks of output wait for a follower at most. Past that, the follower misses what is pushed until it has
/// read some, so that a reader too slow never holds the Job or the Service that prints.
pub(crate) const FOLLOW_QUEUE: usize = 64;

/// A follower of the output of a buffer, as the buffer sees it.
#[derive(Debug)]
struct Follower {
    id: u64,
    sender: mpsc::Sender<Chunk>,
    /// The bytes it missed since the last chunk that got through.
    dropped: u64,
}

/// Output pushed after the backlog of a [`Follow`].
#[derive(Debug)]
pub(crate) struct Chunk {
    /// The bytes the follower missed just before this chunk, as its queue was full.
    pub(crate) dropped: u64,
    pub(crate) bytes: Vec<u8>,
}

/// The output of a buffer as it arrives: its backlog, then every chunk pushed after it. Dropping it stops the follow.
#[derive(Debug)]
pub(crate) struct Follow {
    /// The buffered output when the follow started, as [`OutputBuffer::text`] gives it.
    pub(crate) backlog: String,
    buffer: Arc<Mutex<OutputBuffer>>,
    id: u64,
    receiver: mpsc::Receiver<Chunk>,
}

/// Follows the output of `buffer`. The backlog and the first chunk after it are read under one lock, so that nothing
/// is lost nor told twice between them.
pub(crate) fn follow(buffer: &Arc<Mutex<OutputBuffer>>) -> Follow {
    let (sender, receiver) = mpsc::channel(FOLLOW_QUEUE);
    let mut locked = lock(buffer);
    let id = locked.next_follower;
    locked.next_follower += 1;
    locked.followers.push(Follower { id, sender, dropped: 0 });
    Follow {
        backlog: locked.text(),
        buffer: Arc::clone(buffer),
        id,
        receiver,
    }
}

impl Follow {
    /// Waits for the next chunk. Cancel safe.
    pub(crate) async fn next(&mut self) -> Option<Chunk> {
        self.receiver.recv().await
    }

    /// The next chunk if one is queued.
    pub(crate) fn try_next(&mut self) -> Option<Chunk> {
        self.receiver.try_recv().ok()
    }

    /// Stops the follow once nothing more is pushed, returning the chunks still queued and how many bytes were missed
    /// after the last of them.
    pub(crate) fn finish(mut self) -> (Vec<Chunk>, u64) {
        let mut queued = Vec::new();
        while let Some(chunk) = self.try_next() {
            queued.push(chunk);
        }
        let dropped = lock(&self.buffer).unfollow(self.id);
        (queued, dropped)
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        lock(&self.buffer).unfollow(self.id);
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

    fn shared(buffer: OutputBuffer) -> Arc<Mutex<OutputBuffer>> {
        Arc::new(Mutex::new(buffer))
    }

    #[test]
    fn a_follower_gets_the_backlog_then_what_is_pushed_after_it() {
        let buffer = shared(OutputBuffer::new(64));
        lock(&buffer).push(b"one\n");
        let mut follow = follow(&buffer);
        lock(&buffer).push(b"two\n");
        assert_eq!(follow.backlog, "one\n");
        let chunk = follow.try_next().expect("the chunk pushed after the backlog");
        assert_eq!((chunk.dropped, chunk.bytes.as_slice()), (0, b"two\n".as_slice()));
        assert!(follow.try_next().is_none());
    }

    #[test]
    fn a_follower_that_does_not_keep_up_is_told_how_much_it_missed_and_never_holds_the_push() {
        let buffer = shared(OutputBuffer::new(64));
        let mut follow = follow(&buffer);
        for _ in 0..FOLLOW_QUEUE + 3 {
            lock(&buffer).push(b"xy");
        }
        for _ in 0..FOLLOW_QUEUE {
            assert_eq!(follow.try_next().expect("a queued chunk").dropped, 0);
        }
        assert!(follow.try_next().is_none());
        lock(&buffer).push(b"z");
        let chunk = follow.try_next().expect("the chunk after the drop");
        assert_eq!((chunk.dropped, chunk.bytes.as_slice()), (6, b"z".as_slice()));
    }

    #[test]
    fn the_end_of_a_follow_tells_what_was_dropped_after_the_last_chunk_that_got_through() {
        let buffer = shared(OutputBuffer::new(64));
        let follow = follow(&buffer);
        for _ in 0..FOLLOW_QUEUE + 2 {
            lock(&buffer).push(b"abc");
        }
        let (queued, dropped) = follow.finish();
        assert_eq!((queued.len(), dropped), (FOLLOW_QUEUE, 6));
        assert!(lock(&buffer).followers.is_empty());
    }

    #[test]
    fn a_follower_gone_is_forgotten() {
        let buffer = shared(OutputBuffer::new(64));
        drop(follow(&buffer));
        assert!(lock(&buffer).followers.is_empty());
    }

    #[test]
    fn turns_terminal_line_endings_into_newlines() {
        let mut buffer = OutputBuffer::new(64);
        buffer.push(b"hello\r");
        buffer.push(b"\nworld\r\n");
        assert_eq!(buffer.text(), "hello\nworld\n");
    }
}
