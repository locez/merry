use merry_runtime::{RuntimeError, UserImageInput, UserMessageInput};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DraftImage {
    png_bytes: Arc<[u8]>,
    width: u32,
    height: u32,
}

impl DraftImage {
    pub(crate) fn new(
        png_bytes: impl Into<Arc<[u8]>>,
        width: u32,
        height: u32,
    ) -> Result<Self, RuntimeError> {
        let image = UserImageInput::png("[Image #1]", png_bytes, width, height)?;
        Ok(Self {
            png_bytes: image.shared_png_bytes(),
            width,
            height,
        })
    }

    #[must_use]
    pub(crate) fn shared_png_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.png_bytes)
    }

    #[must_use]
    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub(crate) fn height(&self) -> u32 {
        self.height
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TuiSubmission {
    pub(crate) text: String,
    pub(crate) history_text: String,
    pub(crate) images: Vec<UserImageInput>,
}

impl TuiSubmission {
    pub(crate) fn into_user_message_and_history(
        self,
    ) -> Result<(UserMessageInput, String), RuntimeError> {
        let Self {
            text,
            history_text,
            images,
        } = self;
        let message = UserMessageInput::new(&text, images)?;
        Ok((message, history_text))
    }
}
