use super::{MAX_FILES, PreparationError, failure, io_failure};
use glob::{MatchOptions, Pattern};
use merry_runtime::ProcessRunnerError;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Finds static Include arguments without evaluating Host, Match, shell commands,
/// environment substitutions, or connection-dependent percent tokens.
pub(super) fn paths(text: &str, base: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let end = line
            .find(|character: char| character.is_ascii_whitespace() || character == '=')
            .unwrap_or(line.len());
        if !line[..end].eq_ignore_ascii_case("include") {
            continue;
        }
        let arguments = line[end..]
            .trim_start()
            .strip_prefix('=')
            .unwrap_or(&line[end..])
            .trim_start();
        for path in words(arguments) {
            if path.starts_with('~') || path.contains('%') || path.contains("${") {
                continue;
            }
            paths.push(base.join(path));
        }
    }
    paths
}

fn words(input: &str) -> Vec<String> {
    let mut characters = input.chars().peekable();
    let mut words = Vec::new();
    while let Some(character) = characters.peek().copied() {
        if character == ' ' || character == '\t' {
            characters.next();
            continue;
        }
        if character == '#' {
            break;
        }
        let mut word = String::new();
        let mut quote = None;
        while let Some(character) = characters.next() {
            match character {
                '\\' => match characters.peek().copied() {
                    Some(next)
                        if matches!(next, '\'' | '"' | '\\')
                            || (quote.is_none() && next == ' ') =>
                    {
                        word.push(next);
                        characters.next();
                    }
                    _ => word.push(character),
                },
                ' ' | '\t' if quote.is_none() => break,
                '\'' | '"' if quote.is_none() => quote = Some(character),
                character if quote == Some(character) => quote = None,
                character => word.push(character),
            }
        }
        if quote.is_some() {
            return Vec::new();
        }
        if !word.is_empty() {
            words.push(word);
        }
    }
    words
}

/// Expands paths through the admitted namespace instead of globbing the host root.
pub(super) fn expand(
    pattern: &Path,
    source_for: &impl Fn(&Path) -> Result<Option<PathBuf>, ProcessRunnerError>,
) -> Result<Vec<PathBuf>, PreparationError> {
    let mut candidates = vec![PathBuf::from("/")];
    for component in pattern.components() {
        let Component::Normal(component) = component else {
            if component == Component::ParentDir {
                for candidate in &mut candidates {
                    candidate.pop();
                }
            }
            continue;
        };
        let Some(component) = component.to_str() else {
            return Ok(Vec::new());
        };
        if !component.contains(['*', '?', '[', '\\']) {
            for candidate in &mut candidates {
                candidate.push(component);
            }
            continue;
        }
        let Ok(matcher) = Pattern::new(component) else {
            return Ok(Vec::new());
        };
        let mut expanded = Vec::new();
        for parent in candidates {
            let Some(source) = source_for(&parent).map_err(PreparationError::Policy)? else {
                continue;
            };
            let entries = match fs::read_dir(&source) {
                Ok(entries) => entries,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::NotADirectory
                            | std::io::ErrorKind::PermissionDenied
                    ) =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(io_failure("enumerate SSH Include directory", error).into());
                }
            };
            for (index, entry) in entries.enumerate() {
                if index >= 4096 {
                    return Err(failure("SSH Include directory exceeds 4096 entries").into());
                }
                let name = entry
                    .map_err(|error| io_failure("inspect SSH Include entry", error))?
                    .file_name();
                if matcher.matches_path_with(
                    Path::new(&name),
                    MatchOptions {
                        case_sensitive: true,
                        require_literal_separator: true,
                        require_literal_leading_dot: true,
                    },
                ) {
                    expanded.push(parent.join(name));
                    if expanded.len() > MAX_FILES {
                        return Err(failure("SSH Include pattern exceeds 256 files").into());
                    }
                }
            }
        }
        candidates = expanded;
    }
    candidates.sort();
    Ok(candidates)
}
