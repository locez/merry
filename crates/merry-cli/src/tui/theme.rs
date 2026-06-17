use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub(crate) enum SemanticColor {
    Status,
    Muted,
    Focus,
    Assistant,
    Selection,
    ToolKeyword,
    Command,
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
                (SemanticColor::Status, Color::LightMagenta),
                (SemanticColor::Muted, Color::DarkGray),
                (SemanticColor::Focus, Color::LightMagenta),
                (SemanticColor::Assistant, Color::White),
                (SemanticColor::Selection, Color::Magenta),
                (SemanticColor::ToolKeyword, Color::LightCyan),
                (SemanticColor::Command, Color::LightBlue),
                (SemanticColor::DiffAdd, Color::LightGreen),
                (SemanticColor::DiffDelete, Color::LightRed),
                (SemanticColor::Warning, Color::LightYellow),
                (SemanticColor::Error, Color::LightRed),
                (SemanticColor::Risk, Color::Magenta),
                (SemanticColor::Success, Color::LightGreen),
            ],
        }
    }
}

#[allow(dead_code)]
impl TuiTheme {
    pub(crate) fn from_config(
        config: &crate::config::TuiThemeToml,
    ) -> Result<Self, crate::config::ConfigError> {
        let mut theme = Self::default();
        if let Some(color) = config.status.as_deref() {
            theme.set_color(SemanticColor::Status, parse_color(color)?);
        }
        if let Some(color) = config.muted.as_deref() {
            theme.set_color(SemanticColor::Muted, parse_color(color)?);
        }
        if let Some(color) = config.focus.as_deref() {
            theme.set_color(SemanticColor::Focus, parse_color(color)?);
        }
        if let Some(color) = config.assistant.as_deref() {
            theme.set_color(SemanticColor::Assistant, parse_color(color)?);
        }
        if let Some(color) = config.selection.as_deref() {
            theme.set_color(SemanticColor::Selection, parse_color(color)?);
        }
        if let Some(color) = config.tool_keyword.as_deref() {
            theme.set_color(SemanticColor::ToolKeyword, parse_color(color)?);
        }
        if let Some(color) = config.command.as_deref() {
            theme.set_color(SemanticColor::Command, parse_color(color)?);
        }
        if let Some(color) = config.diff_add.as_deref() {
            theme.set_color(SemanticColor::DiffAdd, parse_color(color)?);
        }
        if let Some(color) = config.diff_delete.as_deref() {
            theme.set_color(SemanticColor::DiffDelete, parse_color(color)?);
        }
        if let Some(color) = config.warning.as_deref() {
            theme.set_color(SemanticColor::Warning, parse_color(color)?);
        }
        if let Some(color) = config.error.as_deref() {
            theme.set_color(SemanticColor::Error, parse_color(color)?);
        }
        if let Some(color) = config.risk.as_deref() {
            theme.set_color(SemanticColor::Risk, parse_color(color)?);
        }
        if let Some(color) = config.success.as_deref() {
            theme.set_color(SemanticColor::Success, parse_color(color)?);
        }
        Ok(theme)
    }

    pub(crate) fn color(&self, slot: SemanticColor) -> Option<Color> {
        self.colors
            .iter()
            .find_map(|(candidate, color)| (*candidate == slot).then_some(*color))
    }

    fn set_color(&mut self, slot: SemanticColor, color: Color) {
        if let Some((_, existing)) = self
            .colors
            .iter_mut()
            .find(|(candidate, _)| *candidate == slot)
        {
            *existing = color;
        } else {
            self.colors.push((slot, color));
        }
    }
}

fn parse_color(value: &str) -> Result<Color, crate::config::ConfigError> {
    match value {
        "black" => Ok(Color::Black),
        "red" => Ok(Color::Red),
        "light_red" => Ok(Color::LightRed),
        "green" => Ok(Color::Green),
        "light_green" => Ok(Color::LightGreen),
        "yellow" => Ok(Color::Yellow),
        "light_yellow" => Ok(Color::LightYellow),
        "blue" => Ok(Color::Blue),
        "light_blue" => Ok(Color::LightBlue),
        "magenta" => Ok(Color::Magenta),
        "light_magenta" => Ok(Color::LightMagenta),
        "cyan" => Ok(Color::Cyan),
        "light_cyan" => Ok(Color::LightCyan),
        "gray" => Ok(Color::Gray),
        "dark_gray" => Ok(Color::DarkGray),
        "white" => Ok(Color::White),
        other => Err(crate::config::ConfigError::Invalid(format!(
            "unsupported TUI color {other:?}"
        ))),
    }
}
