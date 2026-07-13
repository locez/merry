//! Validated user messages admitted into runtime steps.

use crate::RuntimeError;
use merry_llm::{ModelContent, ModelImage};
use std::sync::Arc;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Maximum number of images accepted in one user message.
pub const MAX_USER_IMAGES: usize = 20;
/// Maximum encoded PNG size accepted for one image.
pub const MAX_USER_IMAGE_PNG_BYTES: usize = 10 * 1024 * 1024;
/// Maximum combined encoded PNG size accepted for one user message.
pub const MAX_USER_IMAGE_TOTAL_PNG_BYTES: usize = 20 * 1024 * 1024;
/// Maximum width or height accepted for one image.
pub const MAX_USER_IMAGE_DIMENSION: u32 = 8_000;
/// Maximum decoded pixel count accepted for one image.
pub const MAX_USER_IMAGE_PIXELS: u64 = 32_000_000;

/// Returns the canonical one-based label for a user image.
pub fn user_image_label(index: usize) -> Result<String, RuntimeError> {
    if index == 0 {
        return Err(invalid_image("user image label index must start at one"));
    }
    Ok(format!("[Image #{index}]"))
}

/// A validated normalized PNG attached to one user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserImageInput {
    label: String,
    png_bytes: Arc<[u8]>,
    width: u32,
    height: u32,
}

impl UserImageInput {
    /// Creates a normalized PNG user image.
    pub fn png(
        label: &str,
        png_bytes: impl Into<Arc<[u8]>>,
        width: u32,
        height: u32,
    ) -> Result<Self, RuntimeError> {
        if label.trim().is_empty() || label.chars().any(char::is_control) {
            return Err(invalid_image(
                "user image label must be non-blank and contain no control characters",
            ));
        }

        let png_bytes = png_bytes.into();
        if !png_bytes.starts_with(PNG_SIGNATURE) {
            return Err(invalid_image(
                "user image bytes must contain normalized PNG data",
            ));
        }
        if png_bytes.len() > MAX_USER_IMAGE_PNG_BYTES {
            return Err(invalid_image(
                "one user image encoded PNG must be at most 10 MiB",
            ));
        }
        if width == 0 || height == 0 {
            return Err(invalid_image(
                "user image dimensions must be greater than zero",
            ));
        }
        if width > MAX_USER_IMAGE_DIMENSION || height > MAX_USER_IMAGE_DIMENSION {
            return Err(invalid_image(
                "user image dimensions must not exceed 8000 pixels per dimension",
            ));
        }
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or_else(|| invalid_image("user image pixel count overflowed"))?;
        if pixels > MAX_USER_IMAGE_PIXELS {
            return Err(invalid_image(
                "one user image must not exceed 32 million pixels",
            ));
        }

        Ok(Self {
            label: label.to_owned(),
            png_bytes,
            width,
            height,
        })
    }

    /// Canonical user-visible image label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Normalized PNG bytes.
    #[must_use]
    pub fn png_bytes(&self) -> &[u8] {
        &self.png_bytes
    }

    /// Cheap shared ownership of the normalized PNG bytes.
    #[must_use]
    pub fn shared_png_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.png_bytes)
    }

    /// Decoded image width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Decoded image height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    fn model_image(&self) -> Result<ModelImage, merry_llm::ModelError> {
        ModelImage::png(
            self.label(),
            self.shared_png_bytes(),
            self.width(),
            self.height(),
        )
    }
}

/// One validated user message with zero or more normalized PNG attachments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessageInput {
    text: String,
    images: Vec<UserImageInput>,
}

impl UserMessageInput {
    /// Creates a text-only user message.
    pub fn text_only(text: &str) -> Result<Self, RuntimeError> {
        Self::new(text, Vec::new())
    }

    /// Creates a user message with ordered image attachments.
    pub fn new(text: &str, images: Vec<UserImageInput>) -> Result<Self, RuntimeError> {
        validate_user_text(text)?;
        if images.len() > MAX_USER_IMAGES {
            return Err(invalid_image(
                "one user message may contain at most 20 images",
            ));
        }

        let mut total_png_bytes = 0_usize;
        for (offset, image) in images.iter().enumerate() {
            let expected_label = user_image_label(offset + 1)?;
            if image.label() != expected_label {
                return Err(invalid_image(format!(
                    "user image {} label must be {expected_label}",
                    offset + 1
                )));
            }
            if !text.contains(image.label()) {
                return Err(invalid_image(format!(
                    "user message text must contain image label {}",
                    image.label()
                )));
            }
            total_png_bytes = total_png_bytes
                .checked_add(image.png_bytes().len())
                .ok_or_else(|| invalid_image("user image total PNG byte count overflowed"))?;
        }
        if total_png_bytes > MAX_USER_IMAGE_TOTAL_PNG_BYTES {
            return Err(invalid_image(
                "one user message encoded PNG data must total at most 20 MiB",
            ));
        }

        Ok(Self {
            text: text.to_owned(),
            images,
        })
    }

    /// Full user text containing the image labels.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Ordered normalized PNG attachments.
    #[must_use]
    pub fn images(&self) -> &[UserImageInput] {
        &self.images
    }

    pub(crate) fn model_content(&self) -> Result<ModelContent, merry_llm::ModelError> {
        if self.images.is_empty() {
            return ModelContent::text(self.text());
        }

        let images = self
            .images
            .iter()
            .map(UserImageInput::model_image)
            .collect::<Result<Vec<_>, _>>()?;
        ModelContent::user_with_images(self.text(), images)
    }
}

/// Input snapshot for a runtime step.
///
/// Runtime state such as context, artifacts, tool continuations, and ledger
/// facts is read from the owning session rather than passed as raw chat history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepInput {
    user_messages: Vec<UserMessageInput>,
    user_texts: Vec<String>,
    history: StepInputHistory,
}

impl StepInput {
    /// Creates a user-text step input.
    pub fn user_text(text: &str) -> Result<Self, RuntimeError> {
        Self::user_texts([text])
    }

    /// Creates a step input with multiple consecutive text-only user messages.
    pub fn user_texts<I, S>(texts: I) -> Result<Self, RuntimeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let messages = texts
            .into_iter()
            .map(|text| UserMessageInput::text_only(text.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        Self::from_user_messages(messages)
    }

    /// Creates a step input with consecutive validated user messages.
    pub fn from_user_messages<I>(messages: I) -> Result<Self, RuntimeError>
    where
        I: IntoIterator<Item = UserMessageInput>,
    {
        let user_messages = messages.into_iter().collect::<Vec<_>>();
        if user_messages.is_empty() {
            return Err(RuntimeError::InvalidStepInput {
                reason: "user text burst must contain at least one message",
            });
        }
        let user_texts = user_messages
            .iter()
            .map(|message| message.text().to_owned())
            .collect();

        Ok(Self {
            user_messages,
            user_texts,
            history: StepInputHistory::RecordUser,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn loop_control_text(text: &str) -> Result<Self, RuntimeError> {
        let message = UserMessageInput::text_only(text)?;
        Ok(Self {
            user_texts: vec![message.text().to_owned()],
            user_messages: vec![message],
            history: StepInputHistory::ControlOnly,
        })
    }

    pub(crate) fn no_new_user_input() -> Self {
        Self {
            user_messages: Vec::new(),
            user_texts: Vec::new(),
            history: StepInputHistory::ControlOnly,
        }
    }

    /// Borrows the first user text for this step.
    #[must_use]
    pub fn text(&self) -> &str {
        self.user_texts
            .first()
            .map(String::as_str)
            .expect("StepInput::text is available only for user text inputs")
    }

    /// Borrows the user texts for this step.
    #[must_use]
    pub fn texts(&self) -> &[String] {
        &self.user_texts
    }

    /// Borrows the validated user messages for this step.
    #[must_use]
    pub fn user_messages(&self) -> &[UserMessageInput] {
        &self.user_messages
    }

    pub(crate) fn user_messages_for_request(&self) -> &[UserMessageInput] {
        &self.user_messages
    }

    pub(crate) fn user_messages_for_history(&self) -> &[UserMessageInput] {
        if self.history == StepInputHistory::RecordUser {
            &self.user_messages
        } else {
            &[]
        }
    }

    pub(crate) fn memory_activation_query(&self) -> Option<String> {
        (!self.user_texts.is_empty()).then(|| self.user_texts.join("\n\n"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepInputHistory {
    RecordUser,
    ControlOnly,
}

pub(crate) fn validate_user_text(text: &str) -> Result<(), RuntimeError> {
    if text.trim().is_empty() {
        return Err(RuntimeError::InvalidStepInput {
            reason: "user text must not be blank",
        });
    }

    if text
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(RuntimeError::InvalidStepInput {
            reason: "user text must not contain control characters other than newline or tab",
        });
    }

    Ok(())
}

fn invalid_image(reason: impl Into<String>) -> RuntimeError {
    RuntimeError::InvalidUserImageInput {
        reason: reason.into(),
    }
}
