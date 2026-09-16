//! Small text-normalization helpers shared by runtime records.

/// Collapses every whitespace run into one space and trims both ends.
///
/// Skill descriptions, memory labels, and memory reasoning text are stored and
/// compared as single-line values, so they share one normalization rule instead
/// of each module reimplementing the same `split_whitespace` conversion.
pub(crate) fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_line_breaks_and_padding() {
        assert_eq!(
            collapse_whitespace("  Use when\n\t the task  needs the tool. \n"),
            "Use when the task needs the tool."
        );
        assert_eq!(collapse_whitespace("\n \t\n"), "");
    }
}
