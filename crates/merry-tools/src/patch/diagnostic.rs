//! Model-facing explanations for failed workspace patch matching.
//!
//! The patch tool matches byte-exactly and either writes every planned change
//! or writes nothing. A hunk that does not match therefore fails the whole
//! call, and the only thing that lets the caller recover in one retry is
//! knowing where the match failed. These helpers turn that into short,
//! single-line text that is safe to embed in a failure diagnostic.

/// Longest patch or file text preview embedded in a diagnostic message.
const PREVIEW_CHARS: usize = 96;

/// Smallest shared prefix that makes a file line a useful "closest line" hint.
const MIN_CLOSEST_PREFIX_CHARS: usize = 12;

/// Most match locations reported for an ambiguous preimage.
const MAX_REPORTED_MATCHES: usize = 5;

/// Largest number of candidate start lines compared line by line.
const MAX_MATCH_CANDIDATES: usize = 64;

/// Renders single-line diagnostic text without control characters.
///
/// Patch and file text reaches provider-visible diagnostics, so newlines, tabs,
/// and other control characters are replaced with spaces and long text is
/// truncated with an ellipsis.
pub(super) fn single_line_preview(text: &str, max_chars: usize) -> String {
    let mut preview = String::with_capacity(text.len().min(max_chars));
    for (index, character) in text.chars().enumerate() {
        if index == max_chars {
            preview.push('…');
            return preview;
        }
        preview.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    preview
}

/// Returns the one-based line number containing `byte_index`.
pub(super) fn line_number_at_byte(content: &str, byte_index: usize) -> usize {
    content[..byte_index]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1
}

/// Explains why an exact preimage match failed.
///
/// The result starts with `"; "` so callers can append it to their own message,
/// and is empty when the hunk has no usable lines.
pub(super) fn describe_preimage_miss(content: &str, old_text: &str) -> String {
    let hunk_lines = old_text.lines().collect::<Vec<_>>();
    let Some(first) = hunk_lines.first().copied() else {
        return String::new();
    };
    let content_lines = content.lines().collect::<Vec<_>>();
    if content_lines.is_empty() {
        return "; the target file is empty".to_owned();
    }

    let mut exact_starts = Vec::new();
    let mut trailing_whitespace_start = None;
    let mut whitespace_start = None;
    for (index, line) in content_lines.iter().enumerate() {
        if *line == first {
            exact_starts.push(index);
            if exact_starts.len() >= MAX_MATCH_CANDIDATES {
                break;
            }
            continue;
        }
        if trailing_whitespace_start.is_none() && line.trim_end() == first.trim_end() {
            trailing_whitespace_start = Some(index);
        }
        if whitespace_start.is_none() && line.trim() == first.trim() {
            whitespace_start = Some(index);
        }
    }

    if !exact_starts.is_empty() {
        // Compare every exact candidate so the reported line is the one that
        // matches the hunk most closely, not merely the first look-alike line.
        let mut best: Option<(usize, usize, Option<String>)> = None;
        for start in exact_starts {
            let divergence = first_divergence(&content_lines, &hunk_lines, start);
            let matched = divergence
                .as_ref()
                .map_or(hunk_lines.len(), |(offset, _)| *offset);
            if best
                .as_ref()
                .is_none_or(|(best_matched, _, _)| matched > *best_matched)
            {
                best = Some((matched, start, divergence.map(|(_, clause)| clause)));
            }
        }
        let (_, start, divergence) = best.expect("exact candidate list is not empty");
        return match divergence {
            Some(clause) => format!(
                "; the hunk's first line matches at line {}, but {clause}",
                start + 1
            ),
            None => format!(
                "; all {} hunk line(s) match at line {}, but the file uses CRLF line endings and this tool matches bytes exactly; convert the file to LF first (for example with a process command) or edit it without apply_patch",
                hunk_lines.len(),
                start + 1
            ),
        };
    }

    if let Some(index) = trailing_whitespace_start {
        return format!(
            "; a line matching the hunk's first line is at line {} but differs in trailing whitespace",
            index + 1
        );
    }
    if let Some(index) = whitespace_start {
        return format!(
            "; a line matching the hunk's first line is at line {} but differs in leading or trailing whitespace",
            index + 1
        );
    }

    match closest_line(&content_lines, first) {
        Some((index, line)) => format!(
            "; the hunk's first line \"{}\" was not found; line {} is the closest match: \"{}\"",
            single_line_preview(first, PREVIEW_CHARS),
            index + 1,
            single_line_preview(line, PREVIEW_CHARS),
        ),
        None => format!(
            "; the hunk's first line \"{}\" was not found in the file",
            single_line_preview(first, PREVIEW_CHARS),
        ),
    }
}

/// Lists the line numbers where a preimage matched more than once.
///
/// `first_match` is the byte offset of the match the caller already found, and
/// the scan continues exactly where the caller's ambiguity check continued.
pub(super) fn describe_preimage_ambiguity(
    content: &str,
    old_text: &str,
    first_match: usize,
) -> String {
    let mut lines = vec![line_number_at_byte(content, first_match)];
    let mut search_from = first_match + old_text.len();
    while lines.len() < MAX_REPORTED_MATCHES {
        let Some(offset) = content
            .get(search_from..)
            .and_then(|rest| rest.find(old_text))
        else {
            break;
        };
        let start = search_from + offset;
        lines.push(line_number_at_byte(content, start));
        search_from = start + old_text.len();
    }

    let listed = lines
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let suffix = if lines.len() >= MAX_REPORTED_MATCHES {
        "and possibly more places"
    } else {
        "so make the preimage unique"
    };
    format!("; it matches at lines {listed} {suffix}")
}

/// Describes the first line where a candidate start stops matching the hunk.
fn first_divergence(
    content_lines: &[&str],
    hunk_lines: &[&str],
    start: usize,
) -> Option<(usize, String)> {
    for (offset, hunk_line) in hunk_lines.iter().enumerate().skip(1) {
        match content_lines.get(start + offset) {
            Some(line) if line == hunk_line => {}
            Some(line) => {
                return Some((
                    offset,
                    format!(
                        "line {} differs: the patch has \"{}\" but the file has \"{}\"",
                        start + offset + 1,
                        single_line_preview(hunk_line, PREVIEW_CHARS),
                        single_line_preview(line, PREVIEW_CHARS),
                    ),
                ));
            }
            None => {
                return Some((
                    offset,
                    format!(
                        "the hunk runs past the end of the file, which has {} line(s)",
                        content_lines.len()
                    ),
                ));
            }
        }
    }
    None
}

/// Finds the file line that shares the longest prefix with the hunk's first line.
fn closest_line<'a>(content_lines: &[&'a str], first: &str) -> Option<(usize, &'a str)> {
    let target = first.trim();
    let mut best: Option<(usize, &'a str, usize)> = None;
    for (index, line) in content_lines.iter().enumerate() {
        let shared = shared_prefix_chars(target, line.trim());
        if shared < MIN_CLOSEST_PREFIX_CHARS {
            continue;
        }
        if best.is_none_or(|(_, _, best_shared)| shared > best_shared) {
            best = Some((index, line, shared));
        }
    }
    best.map(|(index, line, _)| (index, line))
}

/// Counts the leading characters two strings share.
fn shared_prefix_chars(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}
