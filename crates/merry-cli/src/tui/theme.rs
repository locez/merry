use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub(crate) enum SemanticColor {
    Status,
    Muted,
    Focus,
    Selection,
    DiffAdd,
    DiffDelete,
    Warning,
    Error,
    Risk,
    Success,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct TuiTheme {
    colors: Vec<(SemanticColor, Color)>,
}

impl Default for TuiTheme {
    fn default() -> Self {
        Self {
            colors: vec![
                (SemanticColor::Status, Color::Cyan),
                (SemanticColor::Muted, Color::DarkGray),
                (SemanticColor::Focus, Color::Yellow),
                (SemanticColor::Selection, Color::Blue),
                (SemanticColor::DiffAdd, Color::Green),
                (SemanticColor::DiffDelete, Color::Red),
                (SemanticColor::Warning, Color::Yellow),
                (SemanticColor::Error, Color::Red),
                (SemanticColor::Risk, Color::Magenta),
                (SemanticColor::Success, Color::Green),
            ],
        }
    }
}

#[allow(dead_code)]
impl TuiTheme {
    pub(crate) fn color(&self, slot: SemanticColor) -> Option<Color> {
        self.colors
            .iter()
            .find_map(|(candidate, color)| (*candidate == slot).then_some(*color))
    }
}
