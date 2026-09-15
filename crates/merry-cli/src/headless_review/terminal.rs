//! Cancellable terminal input for headless permission review.
//!
//! A `-` task leaves a run with only a terminal to read answers from. The
//! terminal is opened non-blocking and driven through Tokio's reactor, so an
//! answer that never comes is an await that cancellation simply drops: no
//! read is parked on the blocking thread pool, and dropping the runtime does
//! not wait for a keystroke. `O_NOCTTY` keeps a bound terminal device from
//! ever becoming this process's controlling terminal, and the non-blocking
//! flag lives on the private file description this open creates, so the
//! operator's shell and its stdin are unaffected.

use std::path::PathBuf;
use tokio::io::AsyncBufRead;

/// Opens the first terminal in `paths` that exists, as a line reader.
///
/// `None` means no terminal is available, which the caller reports per
/// request rather than silently defaulting to deny.
#[cfg(unix)]
pub(super) fn open_review_terminal(
    paths: &[PathBuf],
) -> Option<Box<dyn AsyncBufRead + Unpin + Send>> {
    paths.iter().find_map(|path| {
        ReviewTerminal::open(path)
            .ok()
            .map(|terminal| Box::new(tokio::io::BufReader::new(terminal)) as Box<_>)
    })
}

/// Terminal devices are only opened on Unix; other platforms have no
/// headless terminal channel and report the missing input per request.
#[cfg(not(unix))]
pub(super) fn open_review_terminal(
    _paths: &[PathBuf],
) -> Option<Box<dyn AsyncBufRead + Unpin + Send>> {
    None
}

#[cfg(unix)]
pub(crate) use unix::ReviewTerminal;

#[cfg(unix)]
mod unix {
    use std::{
        fs::{File, OpenOptions},
        io::{self, Read},
        os::unix::fs::OpenOptionsExt,
        path::Path,
        pin::Pin,
        task::{Context, Poll, ready},
    };
    use tokio::io::{AsyncRead, Interest, ReadBuf, unix::AsyncFd};

    /// A terminal opened read-only, non-blocking, and without becoming the
    /// controlling terminal, read through the Tokio reactor.
    pub(crate) struct ReviewTerminal {
        inner: AsyncFd<File>,
    }

    impl ReviewTerminal {
        /// Opens `path`; fails when it is missing or cannot be read.
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
                .open(path)?;
            Ok(Self {
                inner: AsyncFd::with_interest(file, Interest::READABLE)?,
            })
        }
    }

    impl AsyncRead for ReviewTerminal {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            loop {
                let mut guard = ready!(this.inner.poll_read_ready(cx))?;
                let unfilled = buf.initialize_unfilled();
                match guard.try_io(|inner| {
                    let mut file = inner.get_ref();
                    file.read(unfilled)
                }) {
                    Ok(Ok(read)) => {
                        buf.advance(read);
                        return Poll::Ready(Ok(()));
                    }
                    // A pseudo-terminal whose master end closed reports EIO
                    // on the slave: a hung-up terminal is the end of input,
                    // not a review failure.
                    Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => {
                        return Poll::Ready(Ok(()));
                    }
                    Ok(Err(error)) => return Poll::Ready(Err(error)),
                    Err(_would_block) => {}
                }
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{ReviewTerminal, open_review_terminal};
    use crate::sandbox::review_terminal::tests::open_pseudo_terminal;
    use std::{io::Write, path::PathBuf};
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn terminal_yields_typed_lines_and_ends_when_the_master_hangs_up() {
        let mut terminal = open_pseudo_terminal();
        let reader = ReviewTerminal::open(&terminal.slave_path).expect("slave should open");
        let mut reader = BufReader::new(reader);

        terminal
            .master
            .write_all(b"maybe\nyes\n")
            .expect("answers should reach the terminal");
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("first line");
        assert_eq!(line.trim_end(), "maybe");
        line.clear();
        reader.read_line(&mut line).await.expect("second line");
        assert_eq!(line.trim_end(), "yes");

        // Closing the terminal emulator's end hangs the slave up; the reader
        // sees end of input rather than an error.
        drop(terminal.master);
        line.clear();
        let read = reader
            .read_line(&mut line)
            .await
            .expect("hangup should read as end of input");
        assert_eq!(read, 0, "{line:?}");
    }

    /// An unanswered read must be nothing more than a pending await: when it
    /// is dropped, no thread is left blocked on the terminal.
    #[tokio::test]
    async fn an_unanswered_read_is_dropped_by_cancellation() {
        let terminal = open_pseudo_terminal();
        let mut reader =
            BufReader::new(ReviewTerminal::open(&terminal.slave_path).expect("slave should open"));
        let mut line = String::new();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            reader.read_line(&mut line),
        )
        .await;
        assert!(
            outcome.is_err(),
            "nothing was typed, so the read must still be pending"
        );
        // The read future is gone; the reader itself stays usable.
        drop(reader);
    }

    #[test]
    fn missing_paths_yield_no_terminal_and_the_first_existing_one_wins() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let _guard = runtime.enter();
        assert!(open_review_terminal(&[PathBuf::from("/nonexistent/merry-review-tty")]).is_none());
        let terminal = open_pseudo_terminal();
        assert!(
            open_review_terminal(&[
                PathBuf::from("/nonexistent/merry-review-tty"),
                terminal.slave_path.clone(),
            ])
            .is_some()
        );
    }
}
