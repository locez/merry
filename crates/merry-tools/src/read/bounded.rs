use std::io::BufRead;

use crate::errors::{
    BlockingToolError, DomainError, ERROR_FILE_TOO_LARGE, ERROR_NOT_UTF8, ERROR_READ_FAILED,
};

/// Reads one UTF-8 line without allocating beyond the remaining scan budget.
pub(super) fn read_line(
    reader: &mut impl BufRead,
    remaining: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<String, BlockingToolError> {
    let mut bytes = Vec::new();
    loop {
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        let available = reader
            .fill_buf()
            .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace file"))?;
        if is_cancelled() {
            return Err(BlockingToolError::Cancelled);
        }
        if available.is_empty() {
            break;
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(available.len(), |position| position + 1);
        if count > remaining.saturating_sub(bytes.len()) {
            return Err(DomainError::new(
                ERROR_FILE_TOO_LARGE,
                "workspace read range exceeds the configured read limit",
            )
            .into());
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            break;
        }
    }
    String::from_utf8(bytes)
        .map_err(|_| DomainError::new(ERROR_NOT_UTF8, "workspace file is not valid UTF-8").into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        io::{self, BufReader, Read},
    };

    struct EndlessLine<'a> {
        bytes_read: &'a Cell<usize>,
        cancelled: &'a Cell<bool>,
        cancel_on_read: bool,
    }

    impl Read for EndlessLine<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            buffer.fill(b'a');
            self.bytes_read.set(self.bytes_read.get() + buffer.len());
            if self.cancel_on_read {
                self.cancelled.set(true);
            }
            Ok(buffer.len())
        }
    }

    #[test]
    fn oversized_line_stops_at_the_byte_budget() {
        let bytes_read = Cell::new(0);
        let cancelled = Cell::new(false);
        let source = EndlessLine {
            bytes_read: &bytes_read,
            cancelled: &cancelled,
            cancel_on_read: false,
        };
        let mut reader = BufReader::new(source.take(33));
        let error = read_line(&mut reader, 32, &|| false).expect_err("oversized line must fail");
        assert!(
            matches!(error, BlockingToolError::Domain(error) if error.code == ERROR_FILE_TOO_LARGE)
        );
        assert_eq!(bytes_read.get(), 33);
    }

    #[test]
    fn cancellation_during_read_stops_before_another_chunk() {
        let bytes_read = Cell::new(0);
        let cancelled = Cell::new(false);
        let source = EndlessLine {
            bytes_read: &bytes_read,
            cancelled: &cancelled,
            cancel_on_read: true,
        };
        let mut reader = BufReader::with_capacity(4, source.take(33));
        let error =
            read_line(&mut reader, 32, &|| cancelled.get()).expect_err("cancelled read must stop");
        assert!(matches!(error, BlockingToolError::Cancelled));
        assert_eq!(bytes_read.get(), 4);
    }

    #[test]
    fn utf8_split_across_chunks_and_final_partial_lines_are_preserved() {
        let mut reader = BufReader::with_capacity(2, "你好\nlast".as_bytes());
        assert_eq!(
            read_line(&mut reader, 16, &|| false).expect("valid UTF-8"),
            "你好\n"
        );
        assert_eq!(
            read_line(&mut reader, 9, &|| false).expect("last line"),
            "last"
        );
        assert_eq!(read_line(&mut reader, 5, &|| false).expect("EOF"), "");
    }
}
