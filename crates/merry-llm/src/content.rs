//! Provider-neutral ordered model content.

use crate::ModelError;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{borrow::Cow, sync::Arc};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// A normalized PNG image visible to a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelImage {
    label: String,
    #[schemars(with = "Vec<u8>")]
    png_bytes: Arc<[u8]>,
    width: u32,
    height: u32,
}

impl ModelImage {
    /// Creates a validated normalized PNG image.
    pub fn png(
        label: &str,
        png_bytes: impl Into<Arc<[u8]>>,
        width: u32,
        height: u32,
    ) -> Result<Self, ModelError> {
        validate_text("ModelImage label", label)?;
        if label.chars().any(char::is_control) {
            return Err(ModelError::invalid_request(
                "ModelImage label must not contain control characters",
            ));
        }

        let png_bytes = png_bytes.into();
        if !png_bytes.starts_with(PNG_SIGNATURE) {
            return Err(ModelError::invalid_request(
                "ModelImage png_bytes must contain normalized PNG data",
            ));
        }
        if width == 0 || height == 0 {
            return Err(ModelError::invalid_request(
                "ModelImage dimensions must be greater than zero",
            ));
        }

        Ok(Self {
            label: label.to_owned(),
            png_bytes,
            width,
            height,
        })
    }

    /// User-visible label associated with this image.
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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelImageWire {
    label: String,
    png_bytes: Vec<u8>,
    width: u32,
    height: u32,
}

impl<'de> Deserialize<'de> for ModelImage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelImageWire::deserialize(deserializer)?;
        Self::png(&wire.label, wire.png_bytes, wire.width, wire.height).map_err(de::Error::custom)
    }
}

/// One ordered provider-neutral model content part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelContentPart {
    /// A text segment.
    Text { text: String },
    /// A normalized PNG image.
    Image { image: ModelImage },
}

/// Provider-neutral model input content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelContent {
    parts: Vec<ModelContentPart>,
    text_projection: String,
}

impl ModelContent {
    /// Creates validated text content.
    pub fn text(text: &str) -> Result<Self, ModelError> {
        validate_text("ModelContent text", text)?;
        Ok(Self {
            parts: vec![ModelContentPart::Text {
                text: text.to_owned(),
            }],
            text_projection: text.to_owned(),
        })
    }

    /// Creates ordered user content with labeled image frames before the full text.
    pub fn user_with_images(text: &str, images: Vec<ModelImage>) -> Result<Self, ModelError> {
        validate_text("ModelContent text", text)?;
        if images.is_empty() {
            return Self::text(text);
        }

        let mut parts = Vec::with_capacity(images.len().saturating_mul(3).saturating_add(1));
        for image in images {
            parts.push(ModelContentPart::Text {
                text: format!("<image name={}>", image.label()),
            });
            parts.push(ModelContentPart::Image { image });
            parts.push(ModelContentPart::Text {
                text: "</image>".to_owned(),
            });
        }
        parts.push(ModelContentPart::Text {
            text: text.to_owned(),
        });

        Self::from_parts(parts)
    }

    fn from_parts(parts: Vec<ModelContentPart>) -> Result<Self, ModelError> {
        if parts.is_empty() {
            return Err(ModelError::invalid_request(
                "ModelContent parts must not be empty",
            ));
        }

        let mut text_projection = String::new();
        let mut has_images = false;
        for part in &parts {
            match part {
                ModelContentPart::Text { text } => {
                    validate_text("ModelContent text part", text)?;
                    text_projection.push_str(text);
                }
                ModelContentPart::Image { .. } => has_images = true,
            }
        }
        if !has_images {
            return Err(ModelError::invalid_request(
                "ModelContent parts must contain at least one image",
            ));
        }
        validate_text("ModelContent text projection", &text_projection)?;

        Ok(Self {
            parts,
            text_projection,
        })
    }

    /// Ordered text and image parts.
    #[must_use]
    pub fn parts(&self) -> &[ModelContentPart] {
        &self.parts
    }

    /// Iterates over images in model-visible order.
    pub fn images(&self) -> impl Iterator<Item = &ModelImage> {
        self.parts.iter().filter_map(|part| match part {
            ModelContentPart::Image { image } => Some(image),
            ModelContentPart::Text { .. } => None,
        })
    }

    /// Whether this content contains any image parts.
    #[must_use]
    pub fn has_images(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, ModelContentPart::Image { .. }))
    }

    /// Returns the text-only projection used by existing context consumers.
    #[must_use]
    pub fn as_text(&self) -> &str {
        &self.text_projection
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ModelContentRef<'a> {
    Text { text: &'a str },
    Parts { parts: &'a [ModelContentPart] },
}

impl Serialize for ModelContent {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.has_images() {
            ModelContentRef::Parts {
                parts: self.parts(),
            }
            .serialize(serializer)
        } else {
            ModelContentRef::Text {
                text: self.as_text(),
            }
            .serialize(serializer)
        }
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ModelContentWire {
    Text { text: String },
    Parts { parts: Vec<ModelContentPart> },
}

impl<'de> Deserialize<'de> for ModelContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match ModelContentWire::deserialize(deserializer)? {
            ModelContentWire::Text { text } => Self::text(&text).map_err(de::Error::custom),
            ModelContentWire::Parts { parts } => Self::from_parts(parts).map_err(de::Error::custom),
        }
    }
}

impl JsonSchema for ModelContent {
    fn schema_name() -> Cow<'static, str> {
        "ModelContent".into()
    }

    fn schema_id() -> Cow<'static, str> {
        concat!(module_path!(), "::ModelContent").into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        ModelContentWire::json_schema(generator)
    }
}

pub(crate) fn validate_text(kind: &'static str, value: &str) -> Result<(), ModelError> {
    if value.trim().is_empty() {
        return Err(ModelError::invalid_request(format!(
            "{kind} must not be blank"
        )));
    }

    Ok(())
}
