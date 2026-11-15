//! Typed engine options. Application configuration formats and override policy
//! belong to hosts; these defaults are overridable starting points, not bounds.
use crate::theme::Theme;
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ReadingMode {
    #[default]
    Continuous,
    Paginated,
}

pub const DEFAULT_FONT_SIZE_POINTS: f32 = 14.0;
pub const DEFAULT_MAX_WIDTH_POINTS: f32 = 468.0;
pub const DEFAULT_REGULAR_GAMMA: f32 = 1.0;
pub const DEFAULT_EMPHASIZED_GAMMA: f32 = 0.65;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RenderSettings {
    pub font_size: f32,
    pub max_width: f32,
    pub regular_gamma: f32,
    pub emphasized_gamma: f32,
    pub font_hinting: bool,
    pub fractional_positioning: bool,
    pub dark_mode: bool,
    pub theme: Theme,
    /// Ordered host-supplied family preferences. Empty uses registered/generic families.
    pub body_fonts: Vec<String>,
    pub code_fonts: Vec<String>,
    pub author_fonts: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Settings {
    pub rendering: RenderSettings,
    pub reading_mode: ReadingMode,
}

impl Default for RenderSettings {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE_POINTS,
            max_width: DEFAULT_MAX_WIDTH_POINTS,
            regular_gamma: DEFAULT_REGULAR_GAMMA,
            emphasized_gamma: DEFAULT_EMPHASIZED_GAMMA,
            font_hinting: false,
            fractional_positioning: true,
            dark_mode: false,
            theme: Theme::default(),
            body_fonts: Vec::new(),
            code_fonts: Vec::new(),
            author_fonts: true,
        }
    }
}

impl std::ops::Deref for Settings {
    type Target = RenderSettings;
    fn deref(&self) -> &Self::Target {
        &self.rendering
    }
}

impl std::ops::DerefMut for Settings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.rendering
    }
}

impl RenderSettings {
    /// Whether the logical layout inputs agree, ignoring paint-only choices.
    pub fn layout_equivalent(&self, other: &Self) -> bool {
        fn normalized(value: &RenderSettings) -> RenderSettings {
            let mut value = value.clone();
            value.regular_gamma = DEFAULT_REGULAR_GAMMA;
            value.emphasized_gamma = DEFAULT_EMPHASIZED_GAMMA;
            value.dark_mode = false;
            value.theme.light = crate::theme::Palette::light();
            value.theme.dark = crate::theme::Palette::dark();
            value
        }
        normalized(self) == normalized(other)
    }

    /// Checks numerical validity without imposing reader UI preference bounds.
    pub fn validate(&self) -> Result<()> {
        validate_font_size(self.font_size)?;
        validate_max_width(self.max_width)?;
        validate_glyph_gamma(self.regular_gamma)?;
        validate_glyph_gamma(self.emphasized_gamma)?;
        self.theme.validate()?;
        for family in self.body_fonts.iter().chain(&self.code_fonts) {
            if family.trim().is_empty() {
                anyhow::bail!("font family names must not be empty");
            }
        }
        Ok(())
    }

    pub fn glyph_positioning(&self) -> crate::text::GlyphPositioning {
        if self.fractional_positioning {
            crate::text::GlyphPositioning::Fractional
        } else {
            crate::text::GlyphPositioning::PixelSnapped
        }
    }

    pub fn rasterization(&self) -> crate::text::RasterizationOptions {
        crate::text::RasterizationOptions {
            regular_gamma: self.regular_gamma,
            emphasized_gamma: self.emphasized_gamma,
            hinting: self.font_hinting,
            positioning: self.glyph_positioning(),
        }
    }
}

impl From<Settings> for RenderSettings {
    fn from(settings: Settings) -> Self {
        settings.rendering
    }
}

fn positive(name: &str, value: f32) -> Result<()> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        anyhow::bail!("{name} must be finite and positive")
    }
}

pub fn validate_font_size(value: f32) -> Result<()> {
    positive("font size", value)
}
pub fn validate_max_width(value: f32) -> Result<()> {
    positive("max width", value)
}
pub fn validate_glyph_gamma(value: f32) -> Result<()> {
    positive("glyph gamma", value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_defaults_do_not_select_platform_fonts_or_application_config() {
        let settings = Settings::default();
        assert!(settings.rendering.validate().is_ok());
        assert!(settings.body_fonts.is_empty());
        assert!(settings.code_fonts.is_empty());
        assert_eq!(settings.reading_mode, ReadingMode::Continuous);
    }

    #[test]
    fn numerical_validity_does_not_impose_reader_preference_limits() {
        for value in [0.01, 4.0, 100.0, 2000.0] {
            assert!(validate_font_size(value).is_ok());
            assert!(validate_max_width(value).is_ok());
            assert!(validate_glyph_gamma(value).is_ok());
        }
        for value in [0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert!(validate_font_size(value).is_err());
            assert!(validate_max_width(value).is_err());
            assert!(validate_glyph_gamma(value).is_err());
        }
        assert!(
            RenderSettings {
                body_fonts: vec![" ".into()],
                ..RenderSettings::default()
            }
            .validate()
            .is_err()
        );
    }
}
