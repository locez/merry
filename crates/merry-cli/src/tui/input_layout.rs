//! Visual rows for the composer. Logical lines wrap on display width, and
//! atomic placeholders stay on one row.

use std::ops::Range;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VisualRow {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) fn visual_rows(text: &str, max_width: usize, atoms: &[Range<usize>]) -> Vec<VisualRow> {
    let max_width = max_width.max(1);
    let mut rows = Vec::new();
    let mut row_start = 0;
    let mut column = 0;
    let mut index = 0;
    while index < text.len() {
        if text.as_bytes()[index] == b'\n' {
            rows.push(VisualRow {
                start: row_start,
                end: index,
            });
            if column >= max_width {
                rows.push(VisualRow {
                    start: index,
                    end: index,
                });
            }
            index += 1;
            row_start = index;
            column = 0;
            continue;
        }
        let (next, width) = step(text, index, atoms);
        if column > 0 && column.saturating_add(width) > max_width {
            rows.push(VisualRow {
                start: row_start,
                end: index,
            });
            row_start = index;
            column = 0;
            continue;
        }
        column = column.saturating_add(width);
        index = next;
    }
    rows.push(VisualRow {
        start: row_start,
        end: text.len(),
    });
    if let Some(last) = rows.last() {
        let width = UnicodeWidthStr::width(&text[last.start..last.end]);
        if width >= max_width {
            let end = last.end;
            rows.push(VisualRow { start: end, end });
        }
    }
    rows
}

/// Row and display column of `cursor`. A cursor sitting on a soft-wrap boundary
/// belongs to the following row.
pub(crate) fn locate_cursor(text: &str, cursor: usize, rows: &[VisualRow]) -> (usize, usize) {
    let cursor = cursor.min(text.len());
    for (index, row) in rows.iter().enumerate() {
        let soft_next = rows
            .get(index + 1)
            .is_some_and(|next| next.start == row.end);
        let on_row = if soft_next {
            cursor >= row.start && cursor < row.end
        } else {
            cursor >= row.start && cursor <= row.end
        };
        if on_row || index + 1 == rows.len() {
            let end = cursor.min(row.end);
            let column = UnicodeWidthStr::width(&text[row.start..end.max(row.start)]);
            return (index, column);
        }
    }
    (0, 0)
}

pub(crate) fn byte_at_column(
    text: &str,
    row: &VisualRow,
    column: usize,
    atoms: &[Range<usize>],
) -> usize {
    let mut current = 0_usize;
    let mut index = row.start;
    while index < row.end {
        let (next, width) = step(text, index, atoms);
        let next = next.min(row.end);
        if width > 0 && current.saturating_add(width) > column {
            if let Some(atom) = atom_covering(atoms, index) {
                let midpoint = current.saturating_add(width / 2);
                if column <= midpoint {
                    return atom.start.max(row.start);
                }
                return atom.end.min(row.end);
            }
            return index;
        }
        current = current.saturating_add(width);
        index = next;
        if current >= column {
            return index;
        }
    }
    row.end
}

/// Keeps a cursor past the right edge on the last visible cell of an oversized row.
pub(crate) fn display_column(column: usize, max_width: usize) -> usize {
    if max_width == 0 {
        0
    } else {
        column.min(max_width.saturating_sub(1))
    }
}

fn step(text: &str, index: usize, atoms: &[Range<usize>]) -> (usize, usize) {
    if let Some(atom) = atom_covering(atoms, index) {
        let end = atom.end.min(text.len());
        return (end, UnicodeWidthStr::width(&text[index..end]));
    }
    let Some(value) = text[index..].chars().next() else {
        return (text.len(), 0);
    };
    let width = UnicodeWidthChar::width(value).unwrap_or(0);
    (index + value.len_utf8(), width)
}

fn atom_covering(atoms: &[Range<usize>], index: usize) -> Option<&Range<usize>> {
    atoms
        .iter()
        .find(|atom| index >= atom.start && index < atom.end && atom.start < atom.end)
}

#[cfg(test)]
mod tests {
    use super::{byte_at_column, locate_cursor, visual_rows};

    #[test]
    fn wraps_on_display_width_and_keeps_the_cursor_row() {
        let text = "a你好b";
        let rows = visual_rows(text, 4, &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(&text[rows[0].start..rows[0].end], "a你");
        assert_eq!(&text[rows[1].start..rows[1].end], "好b");
        assert_eq!(locate_cursor(text, text.len(), &rows), (1, 3));
        assert_eq!(locate_cursor(text, "a你".len(), &rows), (1, 0));
    }

    #[test]
    fn exact_width_line_gains_a_cursor_row() {
        let rows = visual_rows("abcd", 4, &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].start, rows[1].end);
        assert_eq!(locate_cursor("abcd", 4, &rows), (1, 0));
    }

    #[test]
    fn vertical_target_keeps_column_across_a_short_row() {
        let text = "abcdef\nab\nabcdef";
        let rows = visual_rows(text, 20, &[]);
        let short = &rows[1];
        assert_eq!(byte_at_column(text, short, 5, &[]), short.end);
        let long = &rows[2];
        let at = byte_at_column(text, long, 5, &[]);
        assert_eq!(&text[long.start..at], "abcde");
    }
}
