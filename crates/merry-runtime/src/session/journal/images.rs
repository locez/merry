//! Content-addressed sidecar PNG storage for session journals.
//!
//! Image bytes are written once to `<session_id>/images/<sha256>.png`; journal
//! lines store only the image id and dimensions.

use super::wire::JournalImageRef;
use crate::{
    FileSessionStore, SessionStoreError, UserImageInput,
    session::persistence::{invalid_document, runtime_error_to_invalid_document},
};
use merry_core::SessionId;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
pub(crate) struct JournalImageFile {
    pub(crate) id: String,
    pub(crate) bytes: Arc<[u8]>,
}

pub(super) fn image_refs(
    images: &[UserImageInput],
    files: &mut Vec<JournalImageFile>,
) -> Vec<JournalImageRef> {
    images
        .iter()
        .map(|image| {
            let bytes = image.shared_png_bytes();
            let id = image_id(&bytes);
            if !files.iter().any(|file| file.id == id) {
                files.push(JournalImageFile {
                    id: id.clone(),
                    bytes,
                });
            }
            JournalImageRef {
                id,
                label: image.label().to_owned(),
                width: image.width(),
                height: image.height(),
            }
        })
        .collect()
}

pub(super) async fn load_images(
    images: &[JournalImageRef],
    images_dir: &Path,
) -> Result<Vec<UserImageInput>, SessionStoreError> {
    let mut loaded = Vec::with_capacity(images.len());
    for image in images {
        if !is_image_id(&image.id) {
            return Err(invalid_document("session journal image id is invalid"));
        }
        let path = images_dir.join(format!("{}.png", image.id));
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|source| SessionStoreError::Io {
                path: path.clone(),
                source,
            })?;
        if image_id(&bytes) != image.id {
            return Err(invalid_document(
                "session image does not match its journal id",
            ));
        }
        loaded.push(
            UserImageInput::png(
                &image.label,
                Arc::<[u8]>::from(bytes),
                image.width,
                image.height,
            )
            .map_err(runtime_error_to_invalid_document)?,
        );
    }
    Ok(loaded)
}

pub(crate) fn is_image_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn images_dir(store: &FileSessionStore, session_id: &SessionId) -> PathBuf {
    store.session_dir(session_id).join("images")
}

fn image_id(bytes: &[u8]) -> String {
    hex_encode(&Sha256::digest(bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}
