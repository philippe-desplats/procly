//! The output file of a Service run: a file of the state directory its process appends both its streams to, which
//! the Daemon follows into the output buffer of the Service.
//!
//! A pipe belongs to the Daemon that opened it, so a Service writing to one after a crash of the Daemon dies of
//! `SIGPIPE`. A file outlives the Daemon: the Service keeps writing to it with no Daemon running, and the next one
//! re-attaching the Service reads it from its start, so the output written before and after the restart is kept.
//!
//! The Service writes into a pipe read by a writer of its own, `procly output-file`, which appends to the file and
//! never belongs to the Daemon, so that the file stays bounded while no Daemon runs to empty it. The writer empties
//! the file itself once it reaches [`WRITER_LIMIT`], keeping its last buffer in the previous file as the Daemon does.
//! When the writer cannot be started, the Service appends to the file directly, as it did before the writer existed.

use std::fs::{self, File, OpenOptions};
use std::hash::BuildHasher as _;
use std::io::{self, PipeWriter, Read, Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::task::JoinHandle;

use super::process_group::{self, DRAIN_TIMEOUT};
use super::worktree_dir;
use crate::output::{self, OutputBuffer};
use crate::topology::Topology;

/// The size at which the Daemon empties an output file, once it has read it. As much as the output buffer holds, the
/// last of it kept in the previous file, so that a Daemon re-attaching the Service after a crash finds between one
/// and two buffers of its output.
const LIMIT: u64 = output::CAPACITY as u64;
/// The size at which the writer of an output file empties it itself, the same way, so that the file stays bounded
/// while no Daemon runs. Well above [`LIMIT`], so that a running Daemon, which reads the file as it grows, empties it
/// first.
const WRITER_LIMIT: u64 = 4 * LIMIT;
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

    /// Creates an empty output file under a name no other file holds, readable by the user only, in the directory of
    /// the Worktree of `topology`, which `procly gc` deletes once git no longer lists the Worktree.
    pub(crate) fn create(&self, topology: &Topology) -> io::Result<NewOutput> {
        let dir = worktree_dir::create(&self.dir, topology).map_err(io::Error::other)?;
        let (path, writer) = loop {
            let random = std::hash::RandomState::new().hash_one(SystemTime::now());
            let path = dir.join(format!("{random:016x}.log"));
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

    /// Removes every output file but the `kept` ones and their previous files, which belong to the Services
    /// re-attached after a crash of the Daemon: the others belonged to Services that ended while no Daemon ran. The
    /// directories of the Worktrees stay, with their record. Blocking.
    pub(crate) fn sweep(&self, kept: &[PathBuf]) {
        // The files of the directory itself were written before output files went into the directory of their
        // Worktree.
        let dirs =
            std::iter::once(self.dir.clone()).chain(worktree_dir::list(&self.dir).into_iter().map(|dir| dir.path));
        for dir in dirs {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                let is_file = entry.file_type().is_ok_and(|kind| !kind.is_dir());
                if is_file
                    && entry.file_name() != worktree_dir::RECORD_FILE
                    && !kept.iter().any(|kept| *kept == path || previous(kept) == path)
                    && let Err(error) = fs::remove_file(&path)
                {
                    eprintln!("cannot remove the output file {}: {error}", path.display());
                }
            }
        }
    }
}

/// The file holding the last buffer of output read from the output file at `path` before the Daemon last emptied it.
fn previous(path: &Path) -> PathBuf {
    path.with_extension("prev")
}

/// Opens the output file at `path` for the Daemon to read and empty.
pub(crate) fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}

/// The writer of an output file, and the 2 ends of the pipe the Service writes its stdout and its stderr into.
#[derive(Debug)]
pub(crate) struct Writer {
    pub(crate) stdout: PipeWriter,
    pub(crate) stderr: PipeWriter,
    /// Waited for once the Service is gone, so that the last lines it piped are in the file before it is read.
    pub(crate) process: tokio::process::Child,
}

/// Starts the writer of the output file at `path`, `procly output-file`. It leads a process group of its own and ends
/// once every end of its pipe is closed, whether a Daemon still runs or not.
pub(crate) fn start_writer(path: &Path) -> io::Result<Writer> {
    let (reader, stdout) = io::pipe()?;
    let stderr = stdout.try_clone()?;
    let mut command = tokio::process::Command::new(std::env::current_exe()?);
    command
        .arg("output-file")
        .arg(path)
        .env_clear()
        .stdin(reader)
        .stdout(Stdio::null())
        // Into the log of the Daemon that started it, which it keeps after the Daemon is gone.
        .stderr(Stdio::inherit());
    process_group::detach(&mut command);
    let process = command.spawn()?;
    // The read end this Daemon held is closed with the command, so that the writer ends with the Service.
    drop(command);
    Ok(Writer {
        stdout,
        stderr,
        process,
    })
}

/// Appends what `input` carries to the output file at `path` until `input` ends, emptying the file once it reaches
/// [`WRITER_LIMIT`]: the writer side of an output file, which `procly output-file <path>` runs with the output of the
/// Service piped into it. A file that can no longer be written, a full disk say, is given up on, but `input` is still
/// read to its end, so that the Service never dies of writing into a pipe nobody reads. Blocking.
pub(crate) fn write(mut input: impl Read, path: &Path) -> io::Result<()> {
    let mut chunk = vec![0; CHUNK];
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|appending| Ok((appending, open(path)?)));
    loop {
        let read = match input.read(&mut chunk) {
            Ok(0) => return file.map(|_| ()),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if let Ok((appending, reader)) = &mut file
            && let Err(error) = append(appending, reader, path, &chunk[..read])
        {
            eprintln!(
                "cannot write the output file {}, its output is dropped from now on: {error}",
                path.display()
            );
            file = Err(error);
        }
    }
}

/// Appends `bytes` to the output file at `path`, then empties it once it reaches [`WRITER_LIMIT`], holding the lock of
/// the file so that the Daemon never empties it in the middle.
fn append(appending: &mut File, reader: &mut File, path: &Path, bytes: &[u8]) -> io::Result<()> {
    reader.lock()?;
    let appended = append_locked(appending, reader, path, bytes);
    reader.unlock()?;
    appended
}

fn append_locked(appending: &mut File, reader: &mut File, path: &Path, bytes: &[u8]) -> io::Result<()> {
    appending.write_all(bytes)?;
    let len = reader.metadata()?.len();
    if len >= WRITER_LIMIT {
        empty(reader, path, len)?;
    }
    Ok(())
}

/// Empties the output file at `path`, `len` bytes long and open as `file`, keeping its last buffer in the previous
/// file for the next Daemon, and returns that buffer with the offset it started at. Whoever appends to the file
/// carries on from its start. The caller holds the lock of the file, which the writer takes for each write, so that
/// nothing is appended between reading `len` and emptying the file. A Service that appends to the file itself, when
/// its writer could not be started, takes no lock, and what it appends meanwhile is lost, at worst one write.
fn empty(file: &mut File, path: &Path, len: u64) -> io::Result<(u64, Vec<u8>)> {
    let start = len.saturating_sub(output::CAPACITY as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut last = Vec::new();
    (&mut *file).take(len - start).read_to_end(&mut last)?;
    fs::write(previous(path), &last)?;
    file.set_len(0)?;
    file.rewind()?;
    Ok((start, last))
}

/// The Daemon following an output file into an output buffer, on a blocking thread.
#[derive(Debug)]
pub(crate) struct Tail {
    path: PathBuf,
    /// The writer of the file, when this Daemon started it.
    writer: Option<tokio::process::Child>,
    reader: JoinHandle<()>,
    /// Dropped to tell the thread to read what is left and return.
    done: mpsc::Sender<()>,
}

impl Tail {
    /// Follows the output file at `path`, read through `reader` from its current position, into `output`, `writer`
    /// being its writer when this Daemon started it. Must run inside the Tokio runtime.
    pub(crate) fn follow(
        path: PathBuf,
        reader: File,
        output: Arc<Mutex<OutputBuffer>>,
        writer: Option<tokio::process::Child>,
    ) -> Self {
        let (done, finished) = mpsc::channel();
        let followed = path.clone();
        let reader = tokio::task::spawn_blocking(move || {
            if let Err(error) = follow(reader, &followed, &output, &finished) {
                eprintln!("cannot read the output file {}: {error}", followed.display());
            }
        });
        Self {
            path,
            writer,
            reader,
            done,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Reads what is left of the file once its process is gone, for at most the drain timeout, then removes it. The
    /// writer gets the drain timeout too, to put into the file what the Service piped last.
    pub(crate) async fn finish(self) {
        if let Some(mut writer) = self.writer {
            let _ = tokio::time::timeout(DRAIN_TIMEOUT, writer.wait()).await;
        }
        drop(self.done);
        let _ = tokio::time::timeout(DRAIN_TIMEOUT, self.reader).await;
        let path = self.path;
        let _ = tokio::task::spawn_blocking(move || {
            for path in [previous(&path), path] {
                if let Err(error) = fs::remove_file(&path)
                    && error.kind() != io::ErrorKind::NotFound
                {
                    eprintln!("cannot remove the output file {}: {error}", path.display());
                }
            }
        })
        .await;
    }
}

/// Reads the file at `path` into `output` as it grows, after what a previous Daemon kept of it, until `finished` is
/// told, then reads it to its end one last time.
fn follow(mut file: File, path: &Path, output: &Mutex<OutputBuffer>, finished: &mpsc::Receiver<()>) -> io::Result<()> {
    restore(path, output)?;
    let mut chunk = vec![0; CHUNK];
    loop {
        catch_up(&mut file, path, &mut chunk, output)?;
        match finished.recv_timeout(POLL_INTERVAL) {
            Err(RecvTimeoutError::Timeout) => {}
            Ok(()) | Err(RecvTimeoutError::Disconnected) => return catch_up(&mut file, path, &mut chunk, output),
        }
    }
}

/// Puts back into `output` the last buffer a previous Daemon read from the file at `path` before it emptied it, which
/// exists only for a Service re-attached after a crash.
fn restore(path: &Path, output: &Mutex<OutputBuffer>) -> io::Result<()> {
    match fs::read(previous(path)) {
        Ok(kept) => {
            output::lock(output).push(&kept);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Reads the file at `path` from the current position to its end, emptying it each time it reaches the limit, so that
/// it never grows without bound however long the Service runs.
fn catch_up(file: &mut File, path: &Path, chunk: &mut [u8], output: &Mutex<OutputBuffer>) -> io::Result<()> {
    loop {
        let len = file.metadata()?.len();
        if file.stream_position()? > len {
            // Emptied by its writer while this Daemon lagged behind, it carries on from the start.
            file.rewind()?;
        }
        if len >= LIMIT {
            // The last buffer of the file, kept for the next Daemon should this one crash before the file holds a
            // buffer again. What comes before it would be dropped by the buffer at once, and is not even read. The
            // file is emptied before the buffer takes its share of time.
            let read = file.stream_position()?;
            // Under the lock, the length is read again: the writer may have appended since.
            file.lock()?;
            let emptied = file.metadata().and_then(|locked| empty(file, path, locked.len()));
            file.unlock()?;
            let (start, last) = emptied?;
            let unread = usize::try_from(read.saturating_sub(start)).map_or(last.len(), |read| read.min(last.len()));
            output::lock(output).push(&last[unread..]);
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
    use super::*;

    fn buffer() -> Mutex<OutputBuffer> {
        Mutex::new(OutputBuffer::new(output::CAPACITY))
    }

    fn topology(worktree: &str) -> Topology {
        Topology {
            git_common_dir: PathBuf::from("/src/app/.git"),
            worktree: crate::topology::Worktree {
                path: PathBuf::from(worktree),
                slug: crate::topology::slug(worktree),
                main: false,
            },
        }
    }

    #[test]
    fn reads_what_was_appended_since_the_last_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut new = OutputFiles::new(dir.path().join("output"))
            .create(&topology("/src/app"))
            .unwrap();
        let (output, mut chunk) = (buffer(), vec![0; CHUNK]);

        new.stdout.write_all(b"one\n").unwrap();
        catch_up(&mut new.reader, &new.path, &mut chunk, &output).unwrap();
        new.stderr.write_all(b"two\n").unwrap();
        catch_up(&mut new.reader, &new.path, &mut chunk, &output).unwrap();

        assert_eq!(output::lock(&output).text(), "one\ntwo\n");
    }

    #[test]
    fn empties_the_file_at_the_limit_and_keeps_reading_what_is_appended_after() {
        let dir = tempfile::tempdir().unwrap();
        let mut new = OutputFiles::new(dir.path().join("output"))
            .create(&topology("/src/app"))
            .unwrap();
        let (output, mut chunk) = (buffer(), vec![0; CHUNK]);
        let line = [b'x'; 1023].iter().chain(b"\n").copied().collect::<Vec<u8>>();

        for _ in 0..LIMIT / 1024 + 10 {
            new.stdout.write_all(&line).unwrap();
        }
        catch_up(&mut new.reader, &new.path, &mut chunk, &output).unwrap();
        assert_eq!(fs::metadata(&new.path).unwrap().len(), 0);
        new.stdout.write_all(b"after\n").unwrap();
        catch_up(&mut new.reader, &new.path, &mut chunk, &output).unwrap();

        assert!(output::lock(&output).text().ends_with("x\nafter\n"));
        assert_eq!(fs::metadata(&new.path).unwrap().len(), 6);
    }

    #[test]
    fn a_new_daemon_gets_back_what_was_read_before_the_file_was_emptied_then_what_followed() {
        let dir = tempfile::tempdir().unwrap();
        let mut new = OutputFiles::new(dir.path().join("output"))
            .create(&topology("/src/app"))
            .unwrap();
        let mut chunk = vec![0; CHUNK];
        let line = [b'x'; 1023].iter().chain(b"\n").copied().collect::<Vec<u8>>();
        for _ in 0..LIMIT / 1024 {
            new.stdout.write_all(&line).unwrap();
        }
        catch_up(&mut new.reader, &new.path, &mut chunk, &buffer()).unwrap();
        new.stdout.write_all(b"after\n").unwrap();

        let output = buffer();
        restore(&new.path, &output).unwrap();
        catch_up(&mut open(&new.path).unwrap(), &new.path, &mut chunk, &output).unwrap();

        // The buffer dropped the oldest whole line to make room for the last one.
        let text = output::lock(&output).text();
        assert_eq!(
            text.lines().filter(|line| line.len() == 1023).count(),
            output::CAPACITY / 1024 - 1
        );
        assert!(text.ends_with("x\nafter\n"));
    }

    #[test]
    fn the_writer_empties_the_file_at_its_own_limit_and_keeps_the_last_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let new = OutputFiles::new(dir.path().join("output"))
            .create(&topology("/src/app"))
            .unwrap();
        let line = [b'x'; 1023].iter().chain(b"\n").copied().collect::<Vec<u8>>();
        let mut input = line.repeat(usize::try_from(WRITER_LIMIT).unwrap() / 1024 * 3 + 7);
        input.extend_from_slice(b"last\n");

        write(input.as_slice(), &new.path).unwrap();

        let len = fs::metadata(&new.path).unwrap().len();
        assert!(len < WRITER_LIMIT, "{len}");
        let kept = fs::read(previous(&new.path)).unwrap();
        assert_eq!(kept.len(), output::CAPACITY);
        assert!(fs::read_to_string(&new.path).unwrap().ends_with("x\nlast\n"));
    }

    #[test]
    fn a_writer_that_cannot_write_its_file_still_reads_its_input_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut input = io::Cursor::new(vec![b'x'; CHUNK * 3]);

        assert!(write(&mut input, &dir.path().join("missing").join("out.log")).is_err());

        assert_eq!(input.position(), u64::try_from(CHUNK * 3).unwrap());
    }

    #[test]
    fn sweeping_removes_every_output_file_but_the_kept_ones() {
        let dir = tempfile::tempdir().unwrap();
        let files = OutputFiles::new(dir.path().join("output"));
        let kept = files.create(&topology("/src/app")).unwrap().path;
        let gone = files.create(&topology("/src/app-other")).unwrap().path;
        let older = dir.path().join("output").join("0123456789abcdef.log");
        fs::write(previous(&kept), "kept").unwrap();
        fs::write(previous(&gone), "gone").unwrap();
        fs::write(&older, "older").unwrap();

        files.sweep(std::slice::from_ref(&kept));

        assert!(kept.exists() && previous(&kept).exists());
        assert!(!gone.exists() && !previous(&gone).exists() && !older.exists());
        // The directory of each Worktree stays with its record, for gc to place it.
        let worktree_dir = gone.parent().unwrap();
        assert!(worktree_dir.join(worktree_dir::RECORD_FILE).exists());
    }
}
