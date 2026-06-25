use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use std::sync::OnceLock;
use syntect::{
    easy::HighlightLines,
    highlighting::{Color as SyntectColor, FontStyle, Style as SyntectStyle, Theme, ThemeSet},
    parsing::{SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};

const MAX_HIGHLIGHT_BYTES: usize = 512 * 1024;
const MAX_HIGHLIGHT_LINES: usize = 10_000;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

pub(crate) fn highlight_code_to_lines(code: &str, lang: &str) -> Vec<Line<'static>> {
    highlight_to_line_spans(code, lang)
        .map(|lines| lines.into_iter().map(Line::from).collect())
        .unwrap_or_else(|| plain_code_lines(code))
}

fn highlight_to_line_spans(code: &str, lang: &str) -> Option<Vec<Vec<Span<'static>>>> {
    if code.is_empty()
        || code.len() > MAX_HIGHLIGHT_BYTES
        || code.lines().count() > MAX_HIGHLIGHT_LINES
    {
        return None;
    }

    let syntax = find_syntax(lang)?;
    let mut highlighter = HighlightLines::new(syntax, theme());
    let mut lines = Vec::new();
    for line in LinesWithEndings::from(code) {
        let ranges = highlighter.highlight_line(line, syntax_set()).ok()?;
        let spans = ranges
            .into_iter()
            .filter_map(|(style, text)| {
                let text = text.trim_end_matches(['\n', '\r']);
                (!text.is_empty()).then(|| Span::styled(text.to_owned(), convert_style(style)))
            })
            .collect::<Vec<_>>();
        lines.push(if spans.is_empty() {
            vec![Span::raw(String::new())]
        } else {
            spans
        });
    }
    Some(lines)
}

fn plain_code_lines(code: &str) -> Vec<Line<'static>> {
    let mut lines = code
        .lines()
        .map(|line| Line::from(line.to_owned()))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(Line::from(String::new()));
    }
    lines
}

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let themes = ThemeSet::load_defaults();
        themes
            .themes
            .get("base16-ocean.dark")
            .cloned()
            .or_else(|| themes.themes.values().next().cloned())
            .unwrap_or_default()
    })
}

fn find_syntax(lang: &str) -> Option<&'static SyntaxReference> {
    let syntax_set = syntax_set();
    let patched = match lang {
        "csharp" | "c-sharp" => "c#",
        "golang" => "go",
        "python3" => "python",
        "shell" => "bash",
        _ => lang,
    };

    syntax_set
        .find_syntax_by_token(patched)
        .or_else(|| syntax_set.find_syntax_by_name(patched))
        .or_else(|| {
            let lower = patched.to_ascii_lowercase();
            syntax_set
                .syntaxes()
                .iter()
                .find(|syntax| syntax.name.to_ascii_lowercase() == lower)
        })
        .or_else(|| syntax_set.find_syntax_by_extension(lang))
}

fn convert_style(style: SyntectStyle) -> Style {
    let mut tui_style = Style::default();
    if let Some(color) = convert_color(style.foreground) {
        tui_style = tui_style.fg(color);
    }
    if style.font_style.contains(FontStyle::BOLD) {
        tui_style.add_modifier |= Modifier::BOLD;
    }
    tui_style
}

fn convert_color(color: SyntectColor) -> Option<Color> {
    match color.a {
        0x00 => Some(ansi_palette_color(color.r)),
        0x01 => None,
        _ => Some(Color::Rgb(color.r, color.g, color.b)),
    }
}

fn ansi_palette_color(index: u8) -> Color {
    match index {
        0x00 => Color::Black,
        0x01 => Color::Red,
        0x02 => Color::Green,
        0x03 => Color::Yellow,
        0x04 => Color::Blue,
        0x05 => Color::Magenta,
        0x06 => Color::Cyan,
        0x07 => Color::Gray,
        other => Color::Indexed(other),
    }
}
