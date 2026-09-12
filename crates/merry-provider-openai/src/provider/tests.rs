mod errors;
mod request;
mod retry;
mod stream;

use merry_llm::{
    GenerationConfig, ModelContent, ModelMessage, ModelMessageRole, ModelName, ModelRequest,
};

fn model_request() -> ModelRequest {
    ModelRequest::new(
        ModelName::new("debug-model").expect("valid model name"),
        vec![
            ModelMessage::new(
                ModelMessageRole::User,
                ModelContent::text("Hello").expect("valid content"),
            )
            .expect("valid message"),
        ],
        Vec::new(),
        GenerationConfig::default(),
    )
    .expect("valid request")
}
