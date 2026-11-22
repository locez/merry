use crate::{UserImageInput, UserMessageInput};
use std::sync::Arc;

fn persisted_image_message() -> UserMessageInput {
    UserMessageInput::new(
        "resume [Image #1] and [Image #2]",
        vec![
            UserImageInput::png(
                "[Image #1]",
                Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 11]),
                6,
                7,
            )
            .expect("valid first image"),
            UserImageInput::png(
                "[Image #2]",
                Arc::<[u8]>::from([137, 80, 78, 71, 13, 10, 26, 10, 22]),
                8,
                9,
            )
            .expect("valid second image"),
        ],
    )
    .expect("valid image message")
}

pub(super) fn rewrite_journal_commit(
    bytes: &[u8],
    mutate: impl FnOnce(&mut serde_json::Value),
) -> Vec<u8> {
    let text = String::from_utf8(bytes.to_vec()).expect("journal is utf-8");
    let mut lines = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let last = lines.last_mut().expect("journal has a commit");
    let mut commit: serde_json::Value = serde_json::from_str(last).expect("commit is json");
    mutate(&mut commit);
    *last = serde_json::to_string(&commit).expect("commit serializes");
    let mut out = lines.join("\n");
    out.push('\n');
    out.into_bytes()
}

mod round_trip;

mod validation;
