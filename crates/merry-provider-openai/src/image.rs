use base64::{Engine as _, engine::general_purpose::STANDARD};

pub(crate) fn png_data_url(png_bytes: &[u8]) -> String {
    format!("data:image/png;base64,{}", STANDARD.encode(png_bytes))
}
