//! The output file of a Service run: a file of the state directory its process appends both its streams to, which
//! the Daemon follows into the output buffer of the Service.
//!
//! A pipe belongs to the Daemon that opened it, so a Service writing to one after a crash of the Daemon dies of
//! `SIGPIPE`. A file outlives the Daemon: the Service keeps writing to it with no Daemon running, and the next one
//! re-attaching the Service reads it from its start, so the output written before and after the restart is kept.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::hash::BuildHasher as _;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::task::JoinHandle;

use super::process_group::DRAIN_TIMEOUT;
use crate::output::{self, OutputBuffer};

/// The size at which the Daemon empties an output file, once it has read it. As much as the output buffer holds, so
/// that a Daemon re-attaching the Service after a crash finds at most one buffer of its output.
const LIMIT: u64 = output::CAPACITY as u64;
/// How long the Daemon waits for more output once it has read an output file to its end.
const POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How much the Daemon reads from an output file at once.
const CHUNK: usize = 64 * 1024;

/// The directory of the state directory holding the output file of every running Service.
#[derive(Debug, Clone)]
pub(crate) struct OutputFiles {
    dir: PathBuf,
}

/// An output file just created, before the process writing to it is started.
#[derive(Debug)]
pub(crate) struct NewOutput {
    pub(crate) path: PathBuf,
    /// The handle the Daemon reads with, and empties the file through.
    pub(crate) reader: File,
    /// The handles the process writes its stdout and stderr to, both appending.
    pub(crate) stdout: File,
    pub(crate) stderr: File,
}

impl OutputFiles {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Creates an empty output file under a name no other file holds, readable by the user only.
    pub(crate) fn create(&self) -> io::Result<NewOutput> {
        DirBuilder::new().recursive(true).mode(0o700).create(&self.dir)?;
        let (path, writer) = loop {
            let random = std::hash::RandomState::new().hash_one(SystemTime::now());
            let path = self.dir.join(format!("{random:016x}.log"));
            match OpenOptions::new().append(true).create_new(true).mode(0o600).open(&path) {
                Ok(writer) => break (path, writer),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        };
        let handles = open(&path).and_then(|reader| Ok((reader, writer.try_clone()?)));
        match handles {
            Ok((reader, stderr)) => Ok(NewOutput {
                path,
                reader,
                stdout: writer,
                stderr,
            }),
            Err(error) => {
                let _ = fs::remove_file(&path);
                Err(error)
            }
        }
    }

    /// Removes every output file but the `kept` ones, which belong to the Services re-attached after a crash of the
    /// Daemon: the others belonged to Services that ended while no Daemon ran. Blocking.
    pub(crate) fn sweep(&self, kept: &[PathBuf]) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for path in entries.filter_map(|entry| Some(entry.ok()?.path())) {
            if !kept.contains(&path)
                && let Err(error) = fs::remove_file(&path)
            {
                eprintln!("cannot remove the output file {}: {error}", path.display());
            }
        }
    }
}

/// Opens the output file at `path` for the Daemon to read and empty.
pub(crate) fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

/// The Daemon following an output file into an output buffer, on a blocking thread.
#[derive(Debug)]
pub(crate) struct Tail {
    path: PathBuf,
    reader: JoinHandle<()>,
    /// Dropped to tell the thread to read what is left and return.
    done: mpsc::Sender<()>,
}

impl Tail {
    /// Follows the output file at `path`, read through `reader` from its current position, into `output`. Must run
    /// inside the Tokio runtime.
    pub(crate) fn follow(path: PathBuf, reader: File, output: Arc<Mutex<OutputBuffer>>) -> Self {
        let (done, finished) = mpsc::channel();
        let label = path.display().to_string();
        let reader = tokio::task::spawn_blocking(move || {
            if let Err(error) = follow(reader, &output, &finished) {
                eprintln!("cannot read the output file {label}: {error}");
            }
        });
        Self { path, reader, done }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Reads what is left of the file once its process is gone, for at most the drain timeout, then removes it.
    pub(crate) async fn finish(self) {
        drop(self.done);
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, self.reader).await;
        let path = self.path;
        let _ = tokio::task::spawn_blocking(move || {
            if let Err(error) = fs::remove_file(&path)
                && error.kind() != io::ErrorKind::NotFound
            {
                eprintln!("cannot remove the output file {}: {error}", path.display());
            }
        })
        .await;
    }
}

/// Reads the file into `output` as it grows, until `finished` is told, then reads it to its end one last time.
fn follow(mut file: File, output: &Mutex<OutputBuffer>, finished: &mpsc::Receiver<()>) -> io::Result<()> {
    let mut chunk = vec![0; CHUNK];
    loop {
        catch_up(&mut file, &mut chunk, output)?;
        match finished.recv_timeout(POLL_INTERVAL) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return catch_up(&mut file, &mut chunk, output),
        }
    }
}

/// Reads the file from the current position to its end, emptying it each time it reaches the limit, so that it never
/// grows without bound however long the Service runs.
fn catch_up(file: &mut File, chunk: &mut [u8], output: &Mutex<OutputBuffer>) -> io::Result<()> {
    loop {
        let len = file.metadata()?.len();
        if len >= LIMIT {
            // The buffer keeps the last bytes only, what it would drop at once is not even read.
            let start = file.stream_position()?.max(len.saturating_sub(output::CAPACITY as u64));
            file.seek(SeekFrom::Start(start))?;
            let mut rest = Vec::new();
            (&mut *file).take(len.saturating_sub(start)).read_to_end(&mut rest)?;
            output::lock(output).push(&rest);
            // The process appends at the end of the file, wherever it is, so it carries on from the start. What it
            // appended since the length was read is lost, at worst one read of output.
            file.set_len(0)?;
            file.rewind()?;
            continue;
        }
        let read = file.read(chunk)?;
        if read == 0 {
            return Ok(());
        }
        output::lock(output).push(&chunk[..read]);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn buffer() -> Mutex<OutputBuffer> {
        Mutex::new(OutputBuffer::new(output::CAPACITY))
    }

    #[test]
    fn reads_what_was_appended_since_the_last_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut new = OutputFiles::new(dir.path().join("output")).create().unwrap();
        let (output, mut chunk) = (buffer(), vec![0; CHUNK]);

        new.stdout.write_all(b"one\n").unwrap();
        catch_up(&mut new.reader, &mut chunk, &output).unwrap();
        new.stderr.write_all(b"two\n").unwrap();
        catch_up(&mut new.reader, &mut chunk, &output).unwrap();

        assert_eq!(output::lock(&output).text(), "one\ntwo\n");
    }

    #[test]
    fn empties_the_file_at_the_limit_and_keeps_reading_what_is_appended_after() {
        let dir = tempfile::tempdir().unwrap();
        let mut new = OutputFiles::new(dir.path().join("output")).create().unwrap();
        let (output, mut chunk) = (buffer(), vec![0; CHUNK]);
        let line = [b'x'; 1023].iter().chain(b"\n").copied().collect::<Vec<u8>>();

        for _ in 0..LIMIT / 1024 + 10 {
            new.stdout.write_all(&line).unwrap();
        }
        catch_up(&mut new.reader, &mut chunk, &output).unwrap();
        assert_eq!(fs::metadata(&new.path).unwrap().len(), 0);
        new.stdout.write_all(b"after\n").unwrap();
        catch_up(&mut new.reader, &mut chunk, &output).unwrap();

        assert!(output::lock(&output).text().ends_with("x\nafter\n"));
        assert_eq!(fs::metadata(&new.path).unwrap().len(), 6);
    }

    #[test]
    fn sweeping_removes_every_output_file_but_the_kept_ones() {
        let dir = tempfile::tempdir().unwrap();
        let files = OutputFiles::new(dir.path().join("output"));
        let (kept, gone) = (files.create().unwrap().path, files.create().unwrap().path);

        files.sweep(std::slice::from_ref(&kept));

        assert!(kept.exists());
        assert!(!gone.exists());
    }
}
