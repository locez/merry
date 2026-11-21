use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// Named built-in TUI theme presets selectable in `config.toml` or Settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ThemePreset {
    /// Matte charcoal paired with soft sakura-rose accents and ice-mint highlights.
    #[default]
    Sakura,
    /// Terminal-following ANSI 16-color accents over transparent dark surfaces.
    Ansi,
    /// Deep indigo night surfaces with cool sky-blue and violet accents.
    TokyoNight,
    /// Warm velvety dark mocha surfaces with soft pastel accents.
    CatppuccinMocha,
    /// Calm arctic slate surfaces with frost-cyan and glacier-blue accents.
    Nord,
    /// Clean daylight paper surfaces with high-contrast ink typography.
    PaperLight,
}

impl ThemePreset {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Sakura => "Sakura",
            Self::Ansi => "ANSI",
            Self::TokyoNight => "Tokyo Night",
            Self::CatppuccinMocha => "Catppuccin Mocha",
            Self::Nord => "Nord",
            Self::PaperLight => "Paper Light",
        }
    }

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::Sakura => Self::Ansi,
            Self::Ansi => Self::TokyoNight,
            Self::TokyoNight => Self::CatppuccinMocha,
            Self::CatppuccinMocha => Self::Nord,
            Self::Nord => Self::PaperLight,
            Self::PaperLight => Self::Sakura,
        }
    }

    pub(crate) const fn previous(self) -> Self {
        match self {
            Self::Sakura => Self::PaperLight,
            Self::Ansi => Self::Sakura,
            Self::TokyoNight => Self::Ansi,
            Self::CatppuccinMocha => Self::TokyoNight,
            Self::Nord => Self::CatppuccinMocha,
            Self::PaperLight => Self::Nord,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub(crate) enum SemanticColor {
    // ─── Surfaces, backgrounds, and borders ───────────────────────────────
    Background,
    Surface,
    SurfaceSubtle,
    HeaderBackground,
    StatusBackground,
    UserBackground,
    CodeBackground,
    InlineCodeBackground,
    SelectionBackground,
    ChipBackground,
    Border,
    BorderFocus,
    DiffAddBackground,
    DiffDeleteBackground,

    // ─── Typography and accents ───────────────────────────────────────────
    Status,
    Muted,
    TextSecondary,
    Focus,
    Assistant,
    Heading,
    Link,
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
        Self::from_preset(ThemePreset::default())
    }
}

#[allow(dead_code)]
impl TuiTheme {
    pub(crate) fn from_preset(preset: ThemePreset) -> Self {
        match preset {
            ThemePreset::Sakura => Self::sakura(),
            ThemePreset::Ansi => Self::ansi(),
            ThemePreset::TokyoNight => Self::tokyo_night(),
            ThemePreset::CatppuccinMocha => Self::catppuccin_mocha(),
            ThemePreset::Nord => Self::nord(),
            ThemePreset::PaperLight => Self::paper_light(),
        }
    }

    /// Clears the canvas and status bar backgrounds to inherit terminal transparency.
    pub(crate) fn with_transparent_background(mut self, transparent: bool) -> Self {
        if transparent {
            self.set_color(SemanticColor::Background, Color::Reset);
            self.set_color(SemanticColor::StatusBackground, Color::Reset);
        }
        self
    }

    fn ansi() -> Self {
        Self {
            colors: vec![
                // Surfaces & borders (neutral dark charcoal surfaces, letting terminal ANSI accents breathe)
                (SemanticColor::Background, Color::Reset),
                (SemanticColor::Surface, Color::Rgb(34, 34, 40)),
                (SemanticColor::SurfaceSubtle, Color::Rgb(30, 30, 35)),
                (
                    SemanticColor::HeaderBackground,
                    dim_color(Color::LightMagenta),
                ),
                (SemanticColor::StatusBackground, Color::Reset),
                (SemanticColor::UserBackground, Color::Rgb(42, 34, 48)),
                (SemanticColor::CodeBackground, Color::Rgb(32, 32, 38)),
                (SemanticColor::InlineCodeBackground, Color::Reset),
                (SemanticColor::SelectionBackground, Color::Rgb(54, 40, 62)),
                (SemanticColor::ChipBackground, Color::Reset),
                (SemanticColor::Border, Color::Rgb(70, 68, 78)),
                (SemanticColor::BorderFocus, Color::LightMagenta),
                (SemanticColor::DiffAddBackground, Color::Rgb(24, 43, 34)),
                (SemanticColor::DiffDeleteBackground, Color::Rgb(48, 25, 32)),
                // Typography & accents (terminal ANSI 16-color palette)
                (SemanticColor::Status, Color::LightMagenta),
                (SemanticColor::Muted, Color::DarkGray),
                (SemanticColor::TextSecondary, Color::Rgb(168, 160, 178)),
                (SemanticColor::Focus, Color::LightMagenta),
                (SemanticColor::Assistant, Color::White),
                (SemanticColor::Heading, Color::LightMagenta),
                (SemanticColor::Link, Color::LightBlue),
                (SemanticColor::Selection, Color::Magenta),
                (SemanticColor::ToolKeyword, Color::LightCyan),
                (SemanticColor::Command, Color::LightBlue),
                (SemanticColor::DiffAdd, Color::Rgb(142, 210, 166)),
                (SemanticColor::DiffDelete, Color::Rgb(228, 134, 150)),
                (SemanticColor::Warning, Color::LightYellow),
                (SemanticColor::Error, Color::LightRed),
                (SemanticColor::Risk, Color::Magenta),
                (SemanticColor::Success, Color::LightGreen),
            ],
        }
    }

    fn sakura() -> Self {
        Self {
            colors: vec![
                (SemanticColor::Background, Color::Rgb(35, 38, 39)),
                (SemanticColor::Surface, Color::Rgb(42, 44, 48)),
                (SemanticColor::SurfaceSubtle, Color::Rgb(38, 40, 43)),
                (SemanticColor::HeaderBackground, Color::Rgb(54, 30, 48)),
                (SemanticColor::StatusBackground, Color::Rgb(35, 38, 39)),
                (SemanticColor::UserBackground, Color::Rgb(46, 36, 48)),
                (SemanticColor::CodeBackground, Color::Rgb(40, 42, 46)),
                (SemanticColor::InlineCodeBackground, Color::Reset),
                (SemanticColor::SelectionBackground, Color::Rgb(92, 54, 78)),
                (SemanticColor::ChipBackground, Color::Reset),
                (SemanticColor::Border, Color::Rgb(78, 66, 78)),
                (SemanticColor::BorderFocus, Color::Rgb(255, 122, 200)),
                (SemanticColor::DiffAddBackground, Color::Rgb(28, 48, 42)),
                (SemanticColor::DiffDeleteBackground, Color::Rgb(58, 30, 38)),
                (SemanticColor::Status, Color::Rgb(255, 122, 200)),
                (SemanticColor::Muted, Color::Rgb(164, 134, 152)),
                (SemanticColor::TextSecondary, Color::Rgb(204, 180, 196)),
                (SemanticColor::Focus, Color::Rgb(255, 122, 200)),
                (SemanticColor::Assistant, Color::Rgb(255, 243, 249)),
                (SemanticColor::Heading, Color::Rgb(255, 166, 216)),
                (SemanticColor::Link, Color::Rgb(216, 180, 255)),
                (SemanticColor::Selection, Color::Rgb(255, 79, 179)),
                (SemanticColor::ToolKeyword, Color::Rgb(255, 166, 216)),
                (SemanticColor::Command, Color::Rgb(216, 180, 255)),
                (SemanticColor::DiffAdd, Color::Rgb(155, 240, 220)),
                (SemanticColor::DiffDelete, Color::Rgb(255, 138, 158)),
                (SemanticColor::Warning, Color::Rgb(255, 220, 122)),
                (SemanticColor::Error, Color::Rgb(255, 138, 158)),
                (SemanticColor::Risk, Color::Rgb(255, 122, 200)),
                (SemanticColor::Success, Color::Rgb(199, 255, 240)),
            ],
        }
    }

    fn tokyo_night() -> Self {
        Self {
            colors: vec![
                (SemanticColor::Background, Color::Rgb(26, 27, 38)),
                (SemanticColor::Surface, Color::Rgb(36, 40, 59)),
                (SemanticColor::SurfaceSubtle, Color::Rgb(31, 35, 53)),
                (SemanticColor::HeaderBackground, Color::Rgb(36, 40, 59)),
                (SemanticColor::StatusBackground, Color::Rgb(26, 27, 38)),
                (SemanticColor::UserBackground, Color::Rgb(35, 42, 64)),
                (SemanticColor::CodeBackground, Color::Rgb(31, 35, 53)),
                (SemanticColor::InlineCodeBackground, Color::Reset),
                (SemanticColor::SelectionBackground, Color::Rgb(51, 70, 124)),
                (SemanticColor::ChipBackground, Color::Reset),
                (SemanticColor::Border, Color::Rgb(65, 72, 104)),
                (SemanticColor::BorderFocus, Color::Rgb(122, 162, 247)),
                (SemanticColor::DiffAddBackground, Color::Rgb(28, 46, 42)),
                (SemanticColor::DiffDeleteBackground, Color::Rgb(56, 30, 42)),
                (SemanticColor::Status, Color::Rgb(187, 154, 247)),
                (SemanticColor::Muted, Color::Rgb(115, 122, 162)),
                (SemanticColor::TextSecondary, Color::Rgb(169, 177, 214)),
                (SemanticColor::Focus, Color::Rgb(122, 162, 247)),
                (SemanticColor::Assistant, Color::Rgb(192, 202, 245)),
                (SemanticColor::Heading, Color::Rgb(187, 154, 247)),
                (SemanticColor::Link, Color::Rgb(122, 162, 247)),
                (SemanticColor::Selection, Color::Rgb(187, 154, 247)),
                (SemanticColor::ToolKeyword, Color::Rgb(125, 207, 255)),
                (SemanticColor::Command, Color::Rgb(122, 162, 247)),
                (SemanticColor::DiffAdd, Color::Rgb(158, 206, 106)),
                (SemanticColor::DiffDelete, Color::Rgb(247, 118, 142)),
                (SemanticColor::Warning, Color::Rgb(224, 175, 104)),
                (SemanticColor::Error, Color::Rgb(247, 118, 142)),
                (SemanticColor::Risk, Color::Rgb(255, 158, 100)),
                (SemanticColor::Success, Color::Rgb(158, 206, 106)),
            ],
        }
    }

    fn catppuccin_mocha() -> Self {
        Self {
            colors: vec![
                (SemanticColor::Background, Color::Rgb(30, 30, 46)),
                (SemanticColor::Surface, Color::Rgb(49, 50, 68)),
                (SemanticColor::SurfaceSubtle, Color::Rgb(39, 40, 58)),
                (SemanticColor::HeaderBackground, Color::Rgb(46, 40, 66)),
                (SemanticColor::StatusBackground, Color::Rgb(30, 30, 46)),
                (SemanticColor::UserBackground, Color::Rgb(43, 41, 62)),
                (SemanticColor::CodeBackground, Color::Rgb(37, 37, 54)),
                (SemanticColor::InlineCodeBackground, Color::Reset),
                (SemanticColor::SelectionBackground, Color::Rgb(69, 71, 90)),
                (SemanticColor::ChipBackground, Color::Reset),
                (SemanticColor::Border, Color::Rgb(88, 91, 112)),
                (SemanticColor::BorderFocus, Color::Rgb(203, 166, 247)),
                (SemanticColor::DiffAddBackground, Color::Rgb(30, 48, 42)),
                (SemanticColor::DiffDeleteBackground, Color::Rgb(58, 34, 46)),
                (SemanticColor::Status, Color::Rgb(203, 166, 247)),
                (SemanticColor::Muted, Color::Rgb(147, 153, 178)),
                (SemanticColor::TextSecondary, Color::Rgb(186, 194, 222)),
                (SemanticColor::Focus, Color::Rgb(203, 166, 247)),
                (SemanticColor::Assistant, Color::Rgb(205, 214, 244)),
                (SemanticColor::Heading, Color::Rgb(245, 194, 231)),
                (SemanticColor::Link, Color::Rgb(137, 180, 250)),
                (SemanticColor::Selection, Color::Rgb(203, 166, 247)),
                (SemanticColor::ToolKeyword, Color::Rgb(148, 226, 213)),
                (SemanticColor::Command, Color::Rgb(137, 180, 250)),
                (SemanticColor::DiffAdd, Color::Rgb(166, 227, 161)),
                (SemanticColor::DiffDelete, Color::Rgb(243, 139, 168)),
                (SemanticColor::Warning, Color::Rgb(249, 226, 175)),
                (SemanticColor::Error, Color::Rgb(243, 139, 168)),
                (SemanticColor::Risk, Color::Rgb(250, 179, 135)),
                (SemanticColor::Success, Color::Rgb(166, 227, 161)),
            ],
        }
    }

    fn nord() -> Self {
        Self {
            colors: vec![
                (SemanticColor::Background, Color::Rgb(46, 52, 64)),
                (SemanticColor::Surface, Color::Rgb(59, 66, 82)),
                (SemanticColor::SurfaceSubtle, Color::Rgb(52, 58, 72)),
                (SemanticColor::HeaderBackground, Color::Rgb(59, 66, 82)),
                (SemanticColor::StatusBackground, Color::Rgb(46, 52, 64)),
                (SemanticColor::UserBackground, Color::Rgb(56, 65, 82)),
                (SemanticColor::CodeBackground, Color::Rgb(52, 58, 72)),
                (SemanticColor::InlineCodeBackground, Color::Reset),
                (SemanticColor::SelectionBackground, Color::Rgb(67, 76, 94)),
                (SemanticColor::ChipBackground, Color::Reset),
                (SemanticColor::Border, Color::Rgb(76, 86, 106)),
                (SemanticColor::BorderFocus, Color::Rgb(136, 192, 208)),
                (SemanticColor::DiffAddBackground, Color::Rgb(44, 62, 56)),
                (SemanticColor::DiffDeleteBackground, Color::Rgb(68, 44, 52)),
                (SemanticColor::Status, Color::Rgb(136, 192, 208)),
                (SemanticColor::Muted, Color::Rgb(132, 144, 166)),
                (SemanticColor::TextSecondary, Color::Rgb(216, 222, 233)),
                (SemanticColor::Focus, Color::Rgb(136, 192, 208)),
                (SemanticColor::Assistant, Color::Rgb(236, 239, 244)),
                (SemanticColor::Heading, Color::Rgb(143, 188, 187)),
                (SemanticColor::Link, Color::Rgb(129, 161, 193)),
                (SemanticColor::Selection, Color::Rgb(180, 142, 173)),
                (SemanticColor::ToolKeyword, Color::Rgb(136, 192, 208)),
                (SemanticColor::Command, Color::Rgb(129, 161, 193)),
                (SemanticColor::DiffAdd, Color::Rgb(163, 190, 140)),
                (SemanticColor::DiffDelete, Color::Rgb(191, 97, 106)),
                (SemanticColor::Warning, Color::Rgb(235, 203, 139)),
                (SemanticColor::Error, Color::Rgb(191, 97, 106)),
                (SemanticColor::Risk, Color::Rgb(208, 135, 112)),
                (SemanticColor::Success, Color::Rgb(163, 190, 140)),
            ],
        }
    }

    fn paper_light() -> Self {
        Self {
            colors: vec![
                (SemanticColor::Background, Color::Rgb(247, 246, 242)),
                (SemanticColor::Surface, Color::Rgb(238, 236, 230)),
                (SemanticColor::SurfaceSubtle, Color::Rgb(242, 240, 235)),
                (SemanticColor::HeaderBackground, Color::Rgb(235, 230, 245)),
                (SemanticColor::StatusBackground, Color::Rgb(247, 246, 242)),
                (SemanticColor::UserBackground, Color::Rgb(237, 233, 245)),
                (SemanticColor::CodeBackground, Color::Rgb(239, 238, 233)),
                (
                    SemanticColor::InlineCodeBackground,
                    Color::Rgb(234, 232, 226),
                ),
                (
                    SemanticColor::SelectionBackground,
                    Color::Rgb(218, 206, 240),
                ),
                (SemanticColor::ChipBackground, Color::Reset),
                (SemanticColor::Border, Color::Rgb(204, 200, 192)),
                (SemanticColor::BorderFocus, Color::Rgb(130, 80, 223)),
                (SemanticColor::DiffAddBackground, Color::Rgb(218, 251, 225)),
                (
                    SemanticColor::DiffDeleteBackground,
                    Color::Rgb(255, 235, 233),
                ),
                (SemanticColor::Status, Color::Rgb(130, 80, 223)),
                (SemanticColor::Muted, Color::Rgb(110, 118, 129)),
                (SemanticColor::TextSecondary, Color::Rgb(87, 96, 106)),
                (SemanticColor::Focus, Color::Rgb(130, 80, 223)),
                (SemanticColor::Assistant, Color::Rgb(36, 41, 47)),
                (SemanticColor::Heading, Color::Rgb(130, 80, 223)),
                (SemanticColor::Link, Color::Rgb(9, 105, 218)),
                (SemanticColor::Selection, Color::Rgb(130, 80, 223)),
                (SemanticColor::ToolKeyword, Color::Rgb(10, 127, 108)),
                (SemanticColor::Command, Color::Rgb(9, 105, 218)),
                (SemanticColor::DiffAdd, Color::Rgb(26, 127, 55)),
                (SemanticColor::DiffDelete, Color::Rgb(207, 34, 46)),
                (SemanticColor::Warning, Color::Rgb(154, 103, 0)),
                (SemanticColor::Error, Color::Rgb(207, 34, 46)),
                (SemanticColor::Risk, Color::Rgb(188, 76, 0)),
                (SemanticColor::Success, Color::Rgb(26, 127, 55)),
            ],
        }
    }

    pub(crate) fn from_config(
        config: &crate::config::TuiThemeToml,
    ) -> Result<Self, crate::config::ConfigError> {
        let base = Self::from_config_opaque(config)?;
        Ok(base.with_transparent_background(config.transparent.unwrap_or(false)))
    }

    /// Resolves `config.toml` preset and explicit slot overrides without applying
    /// the `transparent` toggle so live Settings toggles can switch cleanly in both directions.
    pub(crate) fn from_config_opaque(
        config: &crate::config::TuiThemeToml,
    ) -> Result<Self, crate::config::ConfigError> {
        let mut theme = Self::from_preset(config.preset.unwrap_or_default());

        // Apply primary accents first so derived defaults stay coherent when
        // users only customize `status`, `focus`, or `command`.
        if let Some(color) = config.status.as_deref() {
            let parsed = parse_color(color)?;
            theme.set_color(SemanticColor::Status, parsed);
            theme.set_color(SemanticColor::HeaderBackground, dim_color(parsed));
        }
        if let Some(color) = config.muted.as_deref() {
            theme.set_color(SemanticColor::Muted, parse_color(color)?);
        }
        if let Some(color) = config.text_secondary.as_deref() {
            theme.set_color(SemanticColor::TextSecondary, parse_color(color)?);
        }
        if let Some(color) = config.focus.as_deref() {
            let parsed = parse_color(color)?;
            theme.set_color(SemanticColor::Focus, parsed);
            theme.set_color(SemanticColor::UserBackground, dim_color(parsed));
            theme.set_color(SemanticColor::SelectionBackground, dim_color(parsed));
            theme.set_color(SemanticColor::BorderFocus, parsed);
            theme.set_color(SemanticColor::Heading, parsed);
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
            let parsed = parse_color(color)?;
            theme.set_color(SemanticColor::Command, parsed);
            theme.set_color(SemanticColor::Link, parsed);
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

        // Apply explicit overrides for surfaces, borders, and specialized roles.
        if let Some(color) = config.background.as_deref() {
            theme.set_color(SemanticColor::Background, parse_color(color)?);
        }
        if let Some(color) = config.surface.as_deref() {
            theme.set_color(SemanticColor::Surface, parse_color(color)?);
        }
        if let Some(color) = config.surface_subtle.as_deref() {
            theme.set_color(SemanticColor::SurfaceSubtle, parse_color(color)?);
        }
        if let Some(color) = config.header_bg.as_deref() {
            theme.set_color(SemanticColor::HeaderBackground, parse_color(color)?);
        }
        if let Some(color) = config.status_bg.as_deref() {
            theme.set_color(SemanticColor::StatusBackground, parse_color(color)?);
        }
        if let Some(color) = config.user_bg.as_deref() {
            theme.set_color(SemanticColor::UserBackground, parse_color(color)?);
        }
        if let Some(color) = config.code_bg.as_deref() {
            theme.set_color(SemanticColor::CodeBackground, parse_color(color)?);
        }
        if let Some(color) = config.inline_code_bg.as_deref() {
            theme.set_color(SemanticColor::InlineCodeBackground, parse_color(color)?);
        }
        if let Some(color) = config.selection_bg.as_deref() {
            theme.set_color(SemanticColor::SelectionBackground, parse_color(color)?);
        }
        if let Some(color) = config.chip_bg.as_deref() {
            theme.set_color(SemanticColor::ChipBackground, parse_color(color)?);
        }
        if let Some(color) = config.border.as_deref() {
            theme.set_color(SemanticColor::Border, parse_color(color)?);
        }
        if let Some(color) = config.border_focus.as_deref() {
            theme.set_color(SemanticColor::BorderFocus, parse_color(color)?);
        }
        if let Some(color) = config.heading.as_deref() {
            theme.set_color(SemanticColor::Heading, parse_color(color)?);
        }
        if let Some(color) = config.link.as_deref() {
            theme.set_color(SemanticColor::Link, parse_color(color)?);
        }
        if let Some(color) = config.diff_add_bg.as_deref() {
            theme.set_color(SemanticColor::DiffAddBackground, parse_color(color)?);
        }
        if let Some(color) = config.diff_delete_bg.as_deref() {
            theme.set_color(SemanticColor::DiffDeleteBackground, parse_color(color)?);
        }
        Ok(theme)
    }

    pub(crate) fn color(&self, slot: SemanticColor) -> Option<Color> {
        self.colors
            .iter()
            .find_map(|(candidate, color)| (*candidate == slot).then_some(*color))
    }

    /// Returns a concrete background color, omitting `Color::Reset` when unset.
    pub(crate) fn bg_color(&self, slot: SemanticColor) -> Option<Color> {
        self.color(slot).filter(|color| *color != Color::Reset)
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

/// Parses an ANSI semantic name, `"default"`/`"reset"`, `#RRGGBB`, or `#RGB` hex color.
pub(crate) fn parse_color(value: &str) -> Result<Color, crate::config::ConfigError> {
    if let Some(hex) = value.strip_prefix('#') {
        return parse_hex_color(value, hex);
    }
    match value {
        "default" | "reset" => Ok(Color::Reset),
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
            "unsupported TUI color {other:?} (expected a named color, \"default\", or #RRGGBB / #RGB hex)"
        ))),
    }
}

fn parse_hex_color(raw: &str, hex: &str) -> Result<Color, crate::config::ConfigError> {
    let invalid = || {
        crate::config::ConfigError::Invalid(format!(
            "invalid hex TUI color {raw:?} (expected #RRGGBB or #RGB)"
        ))
    };
    if !hex.is_ascii() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    match hex.len() {
        6 => {
            let red = u8::from_str_radix(&hex[0..2], 16).map_err(|_| invalid())?;
            let green = u8::from_str_radix(&hex[2..4], 16).map_err(|_| invalid())?;
            let blue = u8::from_str_radix(&hex[4..6], 16).map_err(|_| invalid())?;
            Ok(Color::Rgb(red, green, blue))
        }
        3 => {
            let expand = |digit: &str| -> Result<u8, crate::config::ConfigError> {
                let nibble = u8::from_str_radix(digit, 16).map_err(|_| invalid())?;
                Ok((nibble << 4) | nibble)
            };
            Ok(Color::Rgb(
                expand(&hex[0..1])?,
                expand(&hex[1..2])?,
                expand(&hex[2..3])?,
            ))
        }
        _ => Err(invalid()),
    }
}

pub(crate) fn dim_color(color: Color) -> Color {
    match color {
        Color::Black => Color::Black,
        Color::Red => Color::Rgb(45, 16, 24),
        Color::Green => Color::Rgb(18, 42, 28),
        Color::Yellow => Color::Rgb(48, 40, 18),
        Color::Blue => Color::Rgb(18, 30, 48),
        Color::Magenta => Color::Rgb(42, 20, 44),
        Color::Cyan => Color::Rgb(16, 42, 45),
        Color::LightRed => Color::Rgb(58, 20, 28),
        Color::LightGreen => Color::Rgb(22, 54, 34),
        Color::LightYellow => Color::Rgb(62, 52, 22),
        Color::LightBlue => Color::Rgb(22, 38, 62),
        Color::LightMagenta => Color::Rgb(54, 26, 58),
        Color::LightCyan => Color::Rgb(20, 54, 58),
        Color::Gray => Color::DarkGray,
        Color::DarkGray => Color::Black,
        Color::White => Color::DarkGray,
        Color::Rgb(red, green, blue) => Color::Rgb(red / 4, green / 4, blue / 4),
        Color::Indexed(index) => Color::Indexed(index),
        Color::Reset => Color::Reset,
    }
}

/// Resolves a modern RGB triple for named ANSI or truecolor values for smooth interpolation.
pub(crate) fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        Color::Black => Some((24, 24, 28)),
        Color::Red => Some((210, 90, 100)),
        Color::Green => Some((105, 195, 130)),
        Color::Yellow => Some((225, 195, 110)),
        Color::Blue => Some((110, 150, 235)),
        Color::Magenta => Some((215, 120, 200)),
        Color::Cyan => Some((100, 195, 220)),
        Color::Gray => Some((195, 192, 204)),
        Color::DarkGray => Some((118, 114, 128)),
        Color::LightRed => Some((248, 120, 132)),
        Color::LightGreen => Some((134, 230, 160)),
        Color::LightYellow => Some((250, 220, 132)),
        Color::LightBlue => Some((147, 180, 255)),
        Color::LightMagenta => Some((240, 148, 224)),
        Color::LightCyan => Some((125, 211, 252)),
        Color::White => Some((236, 234, 240)),
        Color::Indexed(_) | Color::Reset => None,
    }
}

/// Linearly blends two colors in RGB space (`t` clamped to `0.0..=1.0`).
pub(crate) fn blend_color(from: Color, to: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (Some(a), Some(b)) = (color_rgb(from), color_rgb(to)) else {
        return if t >= 0.5 { to } else { from };
    };
    Color::Rgb(
        blend_channel(a.0, b.0, t),
        blend_channel(a.1, b.1, t),
        blend_channel(a.2, b.2, t),
    )
}

fn blend_channel(from: u8, to: u8, t: f32) -> u8 {
    let mixed = f32::from(from).mul_add(1.0 - t, f32::from(to) * t);
    let rounded = mixed.round().clamp(0.0, 255.0);
    if rounded <= 0.0 {
        0
    } else if rounded >= 255.0 {
        255
    } else {
        u8::try_from(rounded as u16).unwrap_or(255)
    }
}

/// Quantizes an RGB color to the nearest xterm-256 palette entry for non-truecolor terminals.
#[allow(dead_code)]
pub(crate) fn quantize_for_256(color: Color) -> Color {
    let Color::Rgb(red, green, blue) = color else {
        return color;
    };
    // Compare nearest 6x6x6 color cube entry against nearest 24-step grayscale ramp entry.
    let cube_idx = |channel: u8| -> u8 {
        const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        STEPS
            .iter()
            .enumerate()
            .min_by_key(|(_, step)| channel.abs_diff(**step))
            .map_or(0, |(idx, _)| idx as u8)
    };
    const CUBE_VALS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let cr = cube_idx(red);
    let cg = cube_idx(green);
    let cb = cube_idx(blue);
    let cube_index = 16 + 36 * cr + 6 * cg + cb;
    let cube_dist = rgb_dist_sq(
        (red, green, blue),
        (
            CUBE_VALS[usize::from(cr)],
            CUBE_VALS[usize::from(cg)],
            CUBE_VALS[usize::from(cb)],
        ),
    );

    let avg = ((u16::from(red) + u16::from(green) + u16::from(blue)) / 3) as u8;
    let gray_step = avg.saturating_sub(8).min(230) / 10;
    let gray_val = 8 + gray_step * 10;
    let gray_index = 232 + gray_step;
    let gray_dist = rgb_dist_sq((red, green, blue), (gray_val, gray_val, gray_val));

    if gray_dist < cube_dist {
        Color::Indexed(gray_index)
    } else {
        Color::Indexed(cube_index)
    }
}

fn rgb_dist_sq(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let dr = i32::from(a.0) - i32::from(b.0);
    let dg = i32::from(a.1) - i32::from(b.1);
    let db = i32::from(a.2) - i32::from(b.2);
    (dr * dr + dg * dg + db * db) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_shorthand_reset_and_named_colors() {
        assert_eq!(
            parse_color("#28242a").unwrap(),
            Color::Rgb(0x28, 0x24, 0x2a)
        );
        assert_eq!(
            parse_color("#FF79C6").unwrap(),
            Color::Rgb(0xff, 0x79, 0xc6)
        );
        assert_eq!(parse_color("#f7c").unwrap(), Color::Rgb(0xff, 0x77, 0xcc));
        assert_eq!(parse_color("default").unwrap(), Color::Reset);
        assert_eq!(parse_color("reset").unwrap(), Color::Reset);
        assert_eq!(parse_color("light_magenta").unwrap(), Color::LightMagenta);
    }

    #[test]
    fn rejects_malformed_hex_colors() {
        for bad in ["#", "#12", "#1234", "#1234567", "#zzzzzz", "ff00aa"] {
            assert!(parse_color(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn explicit_surface_hex_overrides_derived_accent_defaults() {
        let theme = TuiTheme::from_config(&crate::config::TuiThemeToml {
            focus: Some("#ff79c6".to_owned()),
            user_bg: Some("#101018".to_owned()),
            surface: Some("#242029".to_owned()),
            inline_code_bg: Some("#332c38".to_owned()),
            ..crate::config::TuiThemeToml::default()
        })
        .expect("hex theme should parse");

        assert_eq!(
            theme.color(SemanticColor::Focus),
            Some(Color::Rgb(0xff, 0x79, 0xc6))
        );
        assert_eq!(
            theme.color(SemanticColor::UserBackground),
            Some(Color::Rgb(0x10, 0x10, 0x18))
        );
        assert_eq!(
            theme.color(SemanticColor::Surface),
            Some(Color::Rgb(0x24, 0x20, 0x29))
        );
        assert_eq!(
            theme.color(SemanticColor::InlineCodeBackground),
            Some(Color::Rgb(0x33, 0x2c, 0x38))
        );
        // Unset derived slot follows `focus`.
        assert_eq!(
            theme.color(SemanticColor::Heading),
            Some(Color::Rgb(0xff, 0x79, 0xc6))
        );
    }

    #[test]
    fn quantizes_rgb_for_256_color_terminals() {
        assert_eq!(
            quantize_for_256(Color::Rgb(128, 128, 128)),
            Color::Indexed(244)
        );
        assert_eq!(quantize_for_256(Color::LightMagenta), Color::LightMagenta);
    }

    #[test]
    fn sakura_is_default_and_all_presets_define_every_slot() {
        assert_eq!(
            TuiTheme::from_preset(ThemePreset::Sakura),
            TuiTheme::default()
        );

        let presets = [
            ThemePreset::Sakura,
            ThemePreset::Ansi,
            ThemePreset::TokyoNight,
            ThemePreset::CatppuccinMocha,
            ThemePreset::Nord,
            ThemePreset::PaperLight,
        ];
        let mut cursor = ThemePreset::Sakura;
        for expected in presets {
            assert_eq!(cursor, expected);
            let theme = TuiTheme::from_preset(expected);
            assert_eq!(
                theme.colors.len(),
                30,
                "preset {expected:?} should define every semantic slot"
            );
            assert_eq!(expected.next().previous(), expected);
            cursor = cursor.next();
        }
        assert_eq!(cursor, ThemePreset::Sakura);

        // ANSI preset preserves terminal-following background and named ANSI accents.
        let ansi = TuiTheme::from_preset(ThemePreset::Ansi);
        assert_eq!(ansi.bg_color(SemanticColor::Background), None);
        assert_eq!(ansi.color(SemanticColor::Status), Some(Color::LightMagenta));

        // Paper Light uses a light surface background and dark readable ink.
        let light = TuiTheme::from_preset(ThemePreset::PaperLight);
        assert_eq!(
            light.bg_color(SemanticColor::Background),
            Some(Color::Rgb(247, 246, 242))
        );
        assert_eq!(
            light.color(SemanticColor::Assistant),
            Some(Color::Rgb(36, 41, 47))
        );
    }

    #[test]
    fn config_preset_layers_transparent_and_explicit_overrides() {
        let theme = TuiTheme::from_config(&crate::config::TuiThemeToml {
            preset: Some(ThemePreset::Sakura),
            transparent: Some(true),
            header_bg: Some("#111111".to_owned()),
            user_bg: Some("#222222".to_owned()),
            ..crate::config::TuiThemeToml::default()
        })
        .expect("sakura preset with transparent and overrides should parse");

        assert_eq!(theme.bg_color(SemanticColor::Background), None);
        assert_eq!(theme.bg_color(SemanticColor::StatusBackground), None);
        assert_eq!(
            theme.bg_color(SemanticColor::HeaderBackground),
            Some(Color::Rgb(0x11, 0x11, 0x11))
        );
        assert_eq!(
            theme.bg_color(SemanticColor::UserBackground),
            Some(Color::Rgb(0x22, 0x22, 0x22))
        );
        // Unoverridden slots retain Sakura preset colors.
        assert_eq!(
            theme.color(SemanticColor::Status),
            TuiTheme::from_preset(ThemePreset::Sakura).color(SemanticColor::Status)
        );
    }
}
