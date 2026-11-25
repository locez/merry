//! Centralized Merry brand glyphs.
//!
//! All TUI chrome references brand marks from this module instead of scattering
//! raw emoji literals across renderers. Note that `🎀` (`U+1F380`) and `🐾`
//! (`U+1F43E`) intentionally omit `U+FE0E` (VS15), because Apple platforms
//! (iOS/macOS) do not ship monochrome glyphs for those code points and render
//! `.notdef` boxes when VS15 excludes Apple Color Emoji. Terminals that want a
//! guaranteed monochrome primary mark everywhere can set
//! `[tui] brand_mark = "snowflake"`, which switches the primary mark to `❆`
//! (`U+2746`, `Emoji=No`).

use crate::config::BrandMarkMode;

/// Merry ribbon mark (`🎀`) used when `brand_mark = "emoji"`.
pub(crate) const BRAND_RIBBON: &str = "🎀";

/// Padded Merry ribbon badge for title hairlines and rounded modal headers.
pub(crate) const BRAND_RIBBON_BADGE: &str = " 🎀 ";

/// Monochrome snowflake mark (`❆`, `U+2746`, `Emoji=No`) used when `brand_mark = "snowflake"`.
pub(crate) const BRAND_SNOWFLAKE: &str = "❆";

/// Padded monochrome snowflake badge for title hairlines and rounded modal headers.
pub(crate) const BRAND_SNOWFLAKE_BADGE: &str = " ❆ ";

/// Merry paw mark (`🐾`) used for header chrome and subagent surfaces.
pub(crate) const BRAND_PAW: &str = "🐾";

/// Padded Merry paw badge for subagent modal headers.
pub(crate) const BRAND_PAW_BADGE: &str = " 🐾 ";

/// Returns the unpadded primary brand glyph (`"🎀"` or `"❆"`) for the active mode.
pub(crate) const fn primary_glyph(mode: BrandMarkMode) -> &'static str {
    match mode {
        BrandMarkMode::Emoji => BRAND_RIBBON,
        BrandMarkMode::Snowflake => BRAND_SNOWFLAKE,
    }
}

/// Returns the padded primary brand badge (`" 🎀 "` or `" ❆ "`) for the active mode.
pub(crate) const fn primary_badge(mode: BrandMarkMode) -> &'static str {
    match mode {
        BrandMarkMode::Emoji => BRAND_RIBBON_BADGE,
        BrandMarkMode::Snowflake => BRAND_SNOWFLAKE_BADGE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn brand_glyphs_omit_vs15_and_support_snowflake_mode() {
        for glyph in [BRAND_RIBBON, BRAND_SNOWFLAKE, BRAND_PAW] {
            assert!(
                !glyph.contains('\u{FE0E}'),
                "{glyph:?} must not force VS15 (breaks on iOS/macOS CoreText)"
            );
            assert!(!glyph.contains('\u{FE0F}'));
        }
        assert_eq!(primary_badge(BrandMarkMode::Emoji), BRAND_RIBBON_BADGE);
        assert_eq!(
            primary_badge(BrandMarkMode::Snowflake),
            BRAND_SNOWFLAKE_BADGE
        );
        assert_eq!(UnicodeWidthStr::width(BRAND_RIBBON), 2);
        assert_eq!(UnicodeWidthStr::width(BRAND_PAW), 2);
        assert_eq!(UnicodeWidthStr::width(BRAND_SNOWFLAKE), 1);
    }
}
