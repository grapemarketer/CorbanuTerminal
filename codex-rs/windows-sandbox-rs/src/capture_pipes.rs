//! Reads a capture's output pipes (#1826, #1830).
//!
//! A process the command leaves running can hold the pipes' write ends: a
//! child started with handle inheritance gets them (PowerShell's
//! `Start-Process -RedirectStandardOutput` is one), so the pipes reach end of
//! file only when it exits. Once the command's own process has exited, the
//! readers get [`OUTPUT_DRAIN_TIMEOUT`] to read what is left; then their
//! reads are cancelled and the capture keeps what they read. The legacy
//! capture and the elevated command runner both read this way.

use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::ReadFile;
use windows_sys::Win32::System::IO::CancelSynchronousIo;

/// How long the readers may go on after the command's process exits.
pub const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Reads one pipe on a thread of its own until end of file or [`finish`].
pub struct PipeReader<T = Vec<u8>> {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<T>,
}

impl PipeReader {
    /// Starts reading `pipe`, a read end the reader owns and closes, into a
    /// buffer.
    pub(crate) fn spawn(pipe: HANDLE) -> Self {
        Self::spawn_with(pipe, Vec::new(), |buf, chunk| buf.extend_from_slice(chunk))
    }
}

impl<T: Send + 'static> PipeReader<T> {
    /// Starts reading `pipe`, a read end the reader owns and closes, handing
    /// each chunk read to `on_chunk` with `state`; the reader ends with
    /// `state`.
    pub fn spawn_with(
        pipe: HANDLE,
        mut state: T,
        mut on_chunk: impl FnMut(&mut T, &[u8]) + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut tmp = [0u8; 8192];
            while !thread_stop.load(Ordering::Acquire) {
                let mut read_bytes: u32 = 0;
                // SAFETY: `pipe` stays open until this thread closes it below.
                let ok = unsafe {
                    ReadFile(
                        pipe,
                        tmp.as_mut_ptr(),
                        tmp.len() as u32,
                        &mut read_bytes,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 || read_bytes == 0 {
                    break;
                }
                on_chunk(&mut state, &tmp[..read_bytes as usize]);
            }
            // SAFETY: the reader owns `pipe` and is done with it.
            unsafe {
                CloseHandle(pipe);
            }
            state
        });
        Self { stop, thread }
    }
}

/// What `readers` read: each runs until end of file or, at the latest,
/// until `timeout` has passed, when its read in progress is cancelled.
pub(crate) fn finish<const N: usize>(readers: [PipeReader; N], timeout: Duration) -> [Vec<u8>; N] {
    finish_each(readers, timeout).map(|read| read.unwrap_or_default())
}

/// [`finish`] for any reader; each result is `Err` if its reader panicked.
pub fn finish_each<T, const N: usize>(
    readers: [PipeReader<T>; N],
    timeout: Duration,
) -> [std::thread::Result<T>; N] {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline && readers.iter().any(|r| !r.thread.is_finished()) {
        std::thread::sleep(Duration::from_millis(10));
    }
    for reader in &readers {
        reader.stop.store(true, Ordering::Release);
    }
    // A reader may be between reads when cancelled (nothing to cancel), then
    // start one more; cancel until it sees the stop flag.
    while readers.iter().any(|r| !r.thread.is_finished()) {
        for reader in readers.iter().filter(|r| !r.thread.is_finished()) {
            // SAFETY: the thread handle is valid while its JoinHandle lives.
            unsafe {
                CancelSynchronousIo(reader.thread.as_raw_handle() as HANDLE);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    readers.map(|reader| reader.thread.join())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use windows_sys::Win32::Storage::FileSystem::WriteFile;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    fn pipe() -> (HANDLE, HANDLE) {
        let (mut read, mut write): (HANDLE, HANDLE) = (0, 0);
        // SAFETY: valid out pointers.
        let ok = unsafe { CreatePipe(&mut read, &mut write, std::ptr::null_mut(), 0) };
        assert_ne!(ok, 0, "CreatePipe");
        (read, write)
    }

    fn write(pipe: HANDLE, bytes: &[u8]) {
        let mut written = 0_u32;
        // SAFETY: a valid pipe and buffer.
        let ok = unsafe {
            WriteFile(
                pipe,
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(ok, 0, "WriteFile");
        assert_eq!(written as usize, bytes.len());
    }

    /// #1826: a write end left open (as a process the command left running
    /// holds it) keeps a reader from end of file; the reader is cancelled
    /// after the timeout and what it read is kept.
    #[test]
    fn a_write_end_left_open_stops_the_readers_at_the_timeout() {
        let (out_read, out_write) = pipe();
        let (err_read, err_write) = pipe();
        write(out_write, b"before exit");
        // SAFETY: this test's own write end; the other stays open.
        unsafe {
            CloseHandle(err_write);
        }
        let started = Instant::now();
        let [out, err] = finish(
            [PipeReader::spawn(out_read), PipeReader::spawn(err_read)],
            Duration::from_secs(1),
        );
        let took = started.elapsed();
        assert_eq!(
            (out.as_slice(), err.as_slice()),
            (&b"before exit"[..], &b""[..])
        );
        assert!(
            took >= Duration::from_secs(1) && took < Duration::from_secs(10),
            "took {took:?}"
        );
        // SAFETY: the open write end, now without a reader.
        unsafe {
            CloseHandle(out_write);
        }
    }

    /// Readers whose pipes reach end of file finish without waiting for
    /// the timeout, with everything written.
    #[test]
    fn closed_write_ends_finish_the_readers_at_end_of_file() {
        let (read, write_end) = pipe();
        let reader = PipeReader::spawn(read);
        let big = vec![b'x'; 100_000];
        write(write_end, &big);
        // SAFETY: this test's own write end.
        unsafe {
            CloseHandle(write_end);
        }
        let started = Instant::now();
        let [out] = finish([reader], Duration::from_secs(60));
        assert_eq!(out.len(), big.len());
        assert!(started.elapsed() < Duration::from_secs(30));
    }
}
