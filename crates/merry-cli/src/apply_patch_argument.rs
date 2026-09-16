//! Shared reader for the file sections an `apply_patch` argument declares.
//!
//! The call detail and the TUI projector both need to know which files a patch
//! names, so section headers are recognized in one place and both readers agree
//! about the set of operations. `merry-tools` owns the grammar that validates
//! and applies a patch; this module only presents the same headers, and it must
//! never be used to decide what a patch does.

/// File operation an `apply_patch` section header declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatchArgumentSectionKind {
    /// `*** Add File:` creates the file.
    Add,
    /// `*** Update File:` edits the file.
    Update,
    /// `*** Delete File:` removes the file.
    Delete,
}

/// Splits a section header into its operation and the path that follows it.
///
/// The path is trimmed, because a header may carry trailing model whitespace.
/// Every other line returns `None`, including another `*** ...` directive such
/// as `*** Begin Patch`, so a caller cannot mistake a directive for a file.
pub(crate) fn section_header(line: &str) -> Option<(PatchArgumentSectionKind, &str)> {
    [
        ("*** Add File: ", PatchArgumentSectionKind::Add),
        ("*** Update File: ", PatchArgumentSectionKind::Update),
        ("*** Delete File: ", PatchArgumentSectionKind::Delete),
    ]
    .into_iter()
    .find_map(|(marker, kind)| line.strip_prefix(marker).map(|path| (kind, path.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_headers_name_every_supported_operation() {
        assert_eq!(
            section_header("*** Add File: notes/new.txt"),
            Some((PatchArgumentSectionKind::Add, "notes/new.txt"))
        );
        assert_eq!(
            section_header("*** Update File: dir/note.txt  "),
            Some((PatchArgumentSectionKind::Update, "dir/note.txt"))
        );
        assert_eq!(
            section_header("*** Delete File: obsolete.txt"),
            Some((PatchArgumentSectionKind::Delete, "obsolete.txt"))
        );
    }

    #[test]
    fn directives_and_hunk_lines_are_not_section_headers() {
        for line in [
            "*** Begin Workspace Patch",
            "*** End Workspace Patch",
            "*** Begin Patch",
            "*** End Patch",
            "*** Add File:",
            "@@ -1,2 +1,3 @@",
            "+added",
            " context",
        ] {
            assert_eq!(
                section_header(line),
                None,
                "`{line}` must not name a file section"
            );
        }
    }
}
