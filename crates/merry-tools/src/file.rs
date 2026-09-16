//! Bounded whole-file reads shared by the workspace tools.
//!
//! `read_text` streams a line range and never needs the complete file, while
//! `apply_patch` needs every byte twice: once while planning a preimage and
//! once immediately before it mutates the file. Both patch reads must enforce
//! the same configured byte budget and report the same failures, so the
//! complete-file read has one owner here.

use std::{
    fs,
    io::{Read, Seek, SeekFrom},
};

use crate::errors::{
    BlockingToolError, DomainError, ERROR_FILE_TOO_LARGE, ERROR_NOT_FILE, ERROR_NOT_UTF8,
    ERROR_READ_FAILED,
};

/// Reads every byte of an open workspace file, bounded by `max_bytes`.
///
/// The read starts at the beginning of the file, so a caller that already
/// wrote through the same handle still observes the file's current bytes.
/// Cancellation is checked before the read starts and again before the bytes
/// are returned to the caller.
///
/// Fails when the handle does not refer to a regular file, when the file
/// exceeds the configured read limit, or when the read itself fails.
pub(crate) fn read_bounded(
    file: &mut fs::File,
    max_bytes: usize,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, BlockingToolError> {
    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    let metadata = file.metadata().map_err(|_| {
        DomainError::new(
            ERROR_READ_FAILED,
            "could not inspect workspace file metadata",
        )
    })?;
    if !metadata.is_file() {
        return Err(
            DomainError::new(ERROR_NOT_FILE, "workspace path is not a regular file").into(),
        );
    }
    let size = usize::try_from(metadata.len()).map_err(|_| {
        DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
    })?;
    if size > max_bytes {
        return Err(DomainError::new(
            ERROR_FILE_TOO_LARGE,
            "workspace file exceeds the configured read limit",
        )
        .into());
    }

    if file.seek(SeekFrom::Start(0)).is_err() {
        return Err(DomainError::new(ERROR_READ_FAILED, "could not seek workspace file").into());
    }

    if is_cancelled() {
        return Err(BlockingToolError::Cancelled);
    }

    // The measured length caps the read, so the returned bytes can never
    // exceed the configured limit even if the file grows meanwhile.
    let mut bytes = Vec::with_capacity(size);
    Read::by_ref(file)
        .take(metadata.len())
        .read_to_end(&mut bytes)
        .map_err(|_| DomainError::new(ERROR_READ_FAILED, "could not read workspace file"))?;

    Ok(bytes)
}

/// Decodes a bounded whole-file read as UTF-8 text.
pub(crate) fn decode_utf8(bytes: Vec<u8>) -> Result<String, DomainError> {
    String::from_utf8(bytes)
        .map_err(|_| DomainError::new(ERROR_NOT_UTF8, "workspace file is not valid UTF-8"))
}
