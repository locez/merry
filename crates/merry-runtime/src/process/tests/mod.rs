use crate::process::{ProcessActionIntent, ProcessEnvPolicy};

mod classification;
mod contracts;

fn intent() -> ProcessActionIntent {
    ProcessActionIntent::new(
        vec!["cargo".to_owned(), "test".to_owned()],
        Some("crates/merry-runtime".to_owned()),
        ProcessEnvPolicy::empty(),
        Some("stdin text".to_owned()),
        1024,
        2048,
    )
    .expect("valid process intent")
}
