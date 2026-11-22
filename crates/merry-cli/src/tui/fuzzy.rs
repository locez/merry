//! Scored fuzzy matching for workspace paths, skills, and slash commands.
//!
//! Uses a bounded optimal-alignment dynamic program with boundary and
//! consecutive-run bonuses so queries like `render` align to `render.rs`
//! instead of scattering across earlier directory segments, while still
//! supporting cross-segment queries like `cmrender`.

/// One fuzzy match result with ranking score and matched character indices in
/// the original candidate string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FuzzyMatch {
    pub(crate) score: i32,
    pub(crate) positions: Vec<usize>,
}

const MATCH_SCORE: i32 = 16;
const EXACT_CASE_BONUS: i32 = 2;
const CONSECUTIVE_BONUS: i32 = 14;
const PATH_BOUNDARY_BONUS: i32 = 20;
const START_BOUNDARY_BONUS: i32 = 18;
const WORD_BOUNDARY_BONUS: i32 = 14;
const CAMEL_BOUNDARY_BONUS: i32 = 12;
const GAP_START_PENALTY: i32 = -5;
const GAP_EXTEND_PENALTY: i32 = -1;
const BASENAME_BONUS: i32 = 40;
const BASENAME_PREFIX_BONUS: i32 = 25;
const EXACT_BASENAME_BONUS: i32 = 60;
const FILE_OVER_DIR_BONUS: i32 = 4;
const MAX_DP_CELLS: usize = 16_384;
const NEG_INF: i32 = i32::MIN / 4;

/// Computes an optimal case-insensitive fuzzy match score and character
/// positions for `query` inside `candidate`.
pub(crate) fn fuzzy_match(candidate: &str, query: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return Some(FuzzyMatch {
            score: 0,
            positions: Vec::new(),
        });
    }

    let cand_chars: Vec<char> = candidate.chars().collect();
    let cand_lower: Vec<char> = cand_chars
        .iter()
        .map(|&ch| ch.to_lowercase().next().unwrap_or(ch))
        .collect();
    let query_chars: Vec<char> = query.chars().collect();
    let query_lower: Vec<char> = query_chars
        .iter()
        .map(|&ch| ch.to_lowercase().next().unwrap_or(ch))
        .collect();

    if query_lower.len() > cand_lower.len() {
        return None;
    }
    if !is_subsequence(&cand_lower, &query_lower) {
        return None;
    }

    let bonuses = boundary_bonuses(&cand_chars);
    let q_len = query_lower.len();
    let c_len = cand_lower.len();

    let (mut score, positions) = if q_len.saturating_mul(c_len) <= MAX_DP_CELLS {
        optimal_alignment(
            &cand_chars,
            &cand_lower,
            &query_chars,
            &query_lower,
            &bonuses,
        )
    } else {
        greedy_alignment(
            &cand_chars,
            &cand_lower,
            &query_chars,
            &query_lower,
            &bonuses,
        )
    };

    // Gently prefer tighter candidates when match quality is otherwise identical,
    // without disturbing alphabetical order between near-equal sibling names.
    score -= (c_len.saturating_sub(q_len) as i32) / 8;

    Some(FuzzyMatch { score, positions })
}

/// Computes a path-aware fuzzy match that rewards filename/basename matches,
/// shallow depth, and files over directories, while falling back to full-path
/// cross-segment matching (e.g. `cmrender` -> `crates/merry-cli/src/tui/render.rs`).
pub(crate) fn fuzzy_match_path(path: &str, query: &str) -> Option<FuzzyMatch> {
    let is_dir = path.ends_with('/');
    let trimmed = path.trim_end_matches('/');
    let depth = trimmed.matches('/').count() as i32;

    if query.is_empty() {
        let mut score = -depth * 2 - (path.chars().count() as i32) / 4;
        if !is_dir {
            score += FILE_OVER_DIR_BONUS;
        }
        return Some(FuzzyMatch {
            score,
            positions: Vec::new(),
        });
    }

    let full = fuzzy_match(path, query)?;
    let basename_start_byte = trimmed.rfind('/').map_or(0, |index| index + 1);
    let basename = &trimmed[basename_start_byte..];
    let basename_char_offset = path[..basename_start_byte].chars().count();

    let mut best = full;
    if !basename.is_empty()
        && let Some(mut base_match) = fuzzy_match(basename, query)
    {
        base_match.score += BASENAME_BONUS;
        let base_lower = basename.to_ascii_lowercase();
        let query_lower = query.to_ascii_lowercase();
        let stem_lower = base_lower
            .rsplit_once('.')
            .map_or(base_lower.as_str(), |(stem, _)| stem);
        if base_lower == query_lower || stem_lower == query_lower {
            base_match.score += EXACT_BASENAME_BONUS;
        } else if base_lower.starts_with(&query_lower) {
            base_match.score += BASENAME_PREFIX_BONUS;
        }
        for pos in &mut base_match.positions {
            *pos += basename_char_offset;
        }
        if base_match.score >= best.score {
            best = base_match;
        }
    }

    best.score -= depth * 2;
    if !is_dir {
        best.score += FILE_OVER_DIR_BONUS;
    }
    Some(best)
}

fn is_subsequence(candidate: &[char], query: &[char]) -> bool {
    let mut qi = 0;
    for &ch in candidate {
        if ch == query[qi] {
            qi += 1;
            if qi == query.len() {
                return true;
            }
        }
    }
    false
}

fn boundary_bonuses(chars: &[char]) -> Vec<i32> {
    let mut bonuses = Vec::with_capacity(chars.len());
    for (index, &ch) in chars.iter().enumerate() {
        let bonus = if index == 0 {
            START_BOUNDARY_BONUS
        } else {
            let prev = chars[index - 1];
            if matches!(prev, '/' | '\\') {
                PATH_BOUNDARY_BONUS
            } else if matches!(prev, '-' | '_' | '.' | ' ' | ':') {
                WORD_BOUNDARY_BONUS
            } else if prev.is_lowercase() && ch.is_uppercase() {
                CAMEL_BOUNDARY_BONUS
            } else {
                0
            }
        };
        bonuses.push(bonus);
    }
    bonuses
}

fn optimal_alignment(
    cand_chars: &[char],
    cand_lower: &[char],
    query_chars: &[char],
    query_lower: &[char],
    bonuses: &[i32],
) -> (i32, Vec<usize>) {
    let q_len = query_lower.len();
    let c_len = cand_lower.len();
    // m_score[qi][ci]: best score matching query[0..=qi] ending at cand[ci].
    // prev_pos[qi][ci]: exact match index of query[qi - 1] achieving m_score[qi][ci].
    let mut m_score = vec![vec![NEG_INF; c_len]; q_len];
    let mut prev_pos = vec![vec![usize::MAX; c_len]; q_len];

    for qi in 0..q_len {
        // Best score/index for matching query[0..qi] strictly before ci - 1
        // (i.e. with a non-empty gap before ci).
        let mut best_gap = NEG_INF;
        let mut best_gap_idx = usize::MAX;
        for ci in qi..c_len {
            if qi > 0 {
                let open_gap = if ci >= 2 && m_score[qi - 1][ci - 2] > NEG_INF {
                    m_score[qi - 1][ci - 2] + GAP_START_PENALTY
                } else {
                    NEG_INF
                };
                let extend_gap = if best_gap > NEG_INF {
                    best_gap + GAP_EXTEND_PENALTY
                } else {
                    NEG_INF
                };
                if open_gap >= extend_gap && open_gap > NEG_INF {
                    best_gap = open_gap;
                    best_gap_idx = ci - 2;
                } else if extend_gap > NEG_INF {
                    best_gap = extend_gap;
                }
            }

            if cand_lower[ci] == query_lower[qi] {
                let char_score = MATCH_SCORE
                    + bonuses[ci]
                    + if cand_chars[ci] == query_chars[qi] {
                        EXACT_CASE_BONUS
                    } else {
                        0
                    };
                if qi == 0 {
                    // Prefer earlier offsets for mid-word starts while keeping
                    // equal word-boundary starts tied for alphabetical stability.
                    let start_penalty = if bonuses[ci] > 0 {
                        0
                    } else {
                        (ci as i32).min(8)
                    };
                    m_score[0][ci] = char_score - start_penalty;
                } else {
                    let consec = if m_score[qi - 1][ci - 1] > NEG_INF {
                        m_score[qi - 1][ci - 1] + char_score + CONSECUTIVE_BONUS
                    } else {
                        NEG_INF
                    };
                    let gapped = if best_gap > NEG_INF {
                        best_gap + char_score
                    } else {
                        NEG_INF
                    };
                    if consec >= gapped && consec > NEG_INF {
                        m_score[qi][ci] = consec;
                        prev_pos[qi][ci] = ci - 1;
                    } else if gapped > NEG_INF {
                        m_score[qi][ci] = gapped;
                        prev_pos[qi][ci] = best_gap_idx;
                    }
                }
            }
        }
    }

    let mut end_ci = 0;
    let mut best_score = NEG_INF;
    for (ci, &score) in m_score[q_len - 1].iter().enumerate() {
        if score > best_score {
            best_score = score;
            end_ci = ci;
        }
    }

    let mut positions = vec![0; q_len];
    let mut ci = end_ci;
    for qi in (0..q_len).rev() {
        positions[qi] = ci;
        if qi > 0 {
            ci = prev_pos[qi][ci];
        }
    }

    (best_score, positions)
}

fn greedy_alignment(
    cand_chars: &[char],
    cand_lower: &[char],
    query_chars: &[char],
    query_lower: &[char],
    bonuses: &[i32],
) -> (i32, Vec<usize>) {
    let mut positions = Vec::with_capacity(query_lower.len());
    let mut score = 0;
    let mut qi = 0;
    let mut last_ci: Option<usize> = None;
    for (ci, &ch) in cand_lower.iter().enumerate() {
        if ch == query_lower[qi] {
            score += MATCH_SCORE + bonuses[ci];
            if cand_chars[ci] == query_chars[qi] {
                score += EXACT_CASE_BONUS;
            }
            if let Some(prev) = last_ci {
                if ci == prev + 1 {
                    score += CONSECUTIVE_BONUS;
                } else {
                    score += GAP_START_PENALTY + GAP_EXTEND_PENALTY * ((ci - prev - 1) as i32);
                }
            }
            positions.push(ci);
            last_ci = Some(ci);
            qi += 1;
            if qi == query_lower.len() {
                break;
            }
        }
    }
    (score, positions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_match_prefers_basename_alignment_over_scattered_prefix_letters() {
        let render = fuzzy_match_path("crates/merry-cli/src/tui/render.rs", "render")
            .expect("render.rs matches");
        let scattered = fuzzy_match_path("crates/a_r/b_e/c_n/d_d/e_e/f_r.txt", "render")
            .expect("scattered path matches");
        assert!(
            render.score > scattered.score,
            "basename match ({}) should beat scattered match ({})",
            render.score,
            scattered.score
        );

        // Positions should align to "render" in "render.rs", not earlier "r"/"e" in "crates/merry".
        let chars: Vec<char> = "crates/merry-cli/src/tui/render.rs".chars().collect();
        let matched: String = render.positions.iter().map(|&i| chars[i]).collect();
        assert_eq!(matched, "render");
        let basename_start = "crates/merry-cli/src/tui/".chars().count();
        assert_eq!(
            render.positions,
            (basename_start..basename_start + 6).collect::<Vec<_>>()
        );
    }

    #[test]
    fn path_match_supports_cross_segment_queries_and_ranks_exact_files_first() {
        let cross = fuzzy_match_path("crates/merry-cli/src/tui/render.rs", "cmrender")
            .expect("cross-segment query matches");
        assert_eq!(cross.positions.len(), "cmrender".len());

        let exact = fuzzy_match_path("Cargo.toml", "Cargo").expect("Cargo.toml matches");
        let nested = fuzzy_match_path("crates/merry-cli/Cargo.toml", "Cargo")
            .expect("nested Cargo.toml matches");
        assert!(exact.score > nested.score);

        let file = fuzzy_match_path("crates/merry-cli/src/tui/render.rs", "render")
            .expect("render.rs matches");
        let dir = fuzzy_match_path("crates/merry-cli/src/tui/render/", "render")
            .expect("render/ matches");
        assert!(file.score > dir.score);
    }

    #[test]
    fn acronym_fuzzy_match_aligns_on_word_boundaries() {
        let cr = fuzzy_match("code-review", "cr").expect("cr matches code-review");
        assert_eq!(cr.positions, vec![0, 5]);
        let sr = fuzzy_match("security-review", "sr").expect("sr matches security-review");
        assert_eq!(sr.positions, vec![0, 9]);
    }

    #[test]
    fn consecutive_backtracking_does_not_jump_to_earlier_higher_start_match() {
        // 'a' at index 0 has START_BOUNDARY_BONUS, while "ab" at indices 4..6 is
        // contiguous; backtracking must preserve [4, 5], not jump back to [0, 5].
        let matched = fuzzy_match("axxxab", "ab").expect("ab matches axxxab");
        assert_eq!(matched.positions, vec![4, 5]);
    }
}
