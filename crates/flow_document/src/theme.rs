use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub page_margin: f32,
    pub minimum_content_width: f32,
    pub line_spacing: f32,
    pub block_spacing: f32,
    pub list_indent: f32,
    pub heading_scales: [f32; 6],
    pub heading_tracking: [f32; 6],
    pub inline_code_scale: f32,
    pub code_block_scale: f32,
    pub superscript_scale: f32,
    pub superscript_shift: f32,
    pub table_horizontal_padding: f32,
    pub table_vertical_padding: f32,
    pub table_min_column_width: f32,
    pub minimum_line_width: f32,
    pub space_stretch: f32,
    pub space_shrink: f32,
    pub hyphenation: bool,
    pub hyphen_penalty: f32,
    pub code_block_indent: f32,
    pub nested_code_block_indent: f32,
    pub quote_bar_offset: f32,
    pub quote_italic: bool,
    pub table_header_bold: bool,
    pub quote_bar_width: f32,
    pub rule_thickness: f32,
    pub table_strong_rule_thickness: f32,
    pub table_light_rule_thickness: f32,
    pub strikethrough_offset: f32,
    pub underline_offset: f32,
    pub decoration_thickness: f32,
    pub task_checkbox_scale: f32,
    pub task_checkbox_border: f32,
    pub task_checkbox_check_width: f32,
    pub light: Palette,
    pub dark: Palette,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            page_margin: 48.0,
            minimum_content_width: 240.0,
            line_spacing: 1.35,
            block_spacing: 16.0,
            list_indent: 28.0,
            heading_scales: [2.0, 1.6, 1.3, 1.1, 1.1, 1.1],
            heading_tracking: [0.0; 6],
            inline_code_scale: 0.92,
            code_block_scale: 0.9,
            superscript_scale: 0.72,
            superscript_shift: 0.35,
            table_horizontal_padding: 10.0,
            table_vertical_padding: 6.0,
            table_min_column_width: 2.0,
            minimum_line_width: 4.0,
            space_stretch: 0.5,
            space_shrink: 1.0 / 3.0,
            hyphenation: true,
            hyphen_penalty: 50.0,
            code_block_indent: 0.5,
            nested_code_block_indent: 1.0,
            quote_bar_offset: 16.0,
            quote_italic: true,
            table_header_bold: true,
            quote_bar_width: 3.0,
            rule_thickness: 1.0,
            table_strong_rule_thickness: 1.5,
            table_light_rule_thickness: 1.0,
            strikethrough_offset: 0.3,
            underline_offset: 0.12,
            decoration_thickness: 1.0,
            task_checkbox_scale: 0.72,
            task_checkbox_border: 1.25,
            task_checkbox_check_width: 1.6,
            light: Palette::light(),
            dark: Palette::dark(),
        }
    }
}

impl Theme {
    pub fn validate(&self) -> Result<()> {
        positive("page_margin", self.page_margin)?;
        positive("minimum_content_width", self.minimum_content_width)?;
        positive("line_spacing", self.line_spacing)?;
        nonnegative("block_spacing", self.block_spacing)?;
        positive("list_indent", self.list_indent)?;
        for (index, scale) in self.heading_scales.iter().copied().enumerate() {
            positive(&format!("heading_scales[{}]", index + 1), scale)?;
        }
        for (index, tracking) in self.heading_tracking.iter().copied().enumerate() {
            finite(&format!("heading_tracking[{}]", index + 1), tracking)?;
        }
        positive("inline_code_scale", self.inline_code_scale)?;
        positive("code_block_scale", self.code_block_scale)?;
        positive("superscript_scale", self.superscript_scale)?;
        nonnegative("superscript_shift", self.superscript_shift)?;
        nonnegative("table_horizontal_padding", self.table_horizontal_padding)?;
        nonnegative("table_vertical_padding", self.table_vertical_padding)?;
        positive("table_min_column_width", self.table_min_column_width)?;
        positive("minimum_line_width", self.minimum_line_width)?;
        nonnegative("space_stretch", self.space_stretch)?;
        nonnegative("space_shrink", self.space_shrink)?;
        nonnegative("hyphen_penalty", self.hyphen_penalty)?;
        nonnegative("code_block_indent", self.code_block_indent)?;
        nonnegative("nested_code_block_indent", self.nested_code_block_indent)?;
        nonnegative("quote_bar_offset", self.quote_bar_offset)?;
        nonnegative("quote_bar_width", self.quote_bar_width)?;
        positive("rule_thickness", self.rule_thickness)?;
        positive(
            "table_strong_rule_thickness",
            self.table_strong_rule_thickness,
        )?;
        positive(
            "table_light_rule_thickness",
            self.table_light_rule_thickness,
        )?;
        nonnegative("strikethrough_offset", self.strikethrough_offset)?;
        nonnegative("underline_offset", self.underline_offset)?;
        positive("decoration_thickness", self.decoration_thickness)?;
        positive("task_checkbox_scale", self.task_checkbox_scale)?;
        positive("task_checkbox_border", self.task_checkbox_border)?;
        positive("task_checkbox_check_width", self.task_checkbox_check_width)
    }
}

fn finite(name: &str, value: f32) -> Result<()> {
    if value.is_finite() {
        Ok(())
    } else {
        anyhow::bail!("{name} must be finite")
    }
}

fn positive(name: &str, value: f32) -> Result<()> {
    if value.is_finite() && value > 0.0 {
        Ok(())
    } else {
        anyhow::bail!("{name} must be greater than zero")
    }
}

fn nonnegative(name: &str, value: f32) -> Result<()> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        anyhow::bail!("{name} must be nonnegative")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    pub link: [u8; 3],
    pub border: [u8; 3],
    pub key_background: [u8; 3],
    pub muted: [u8; 3],
    pub quote_bar: [u8; 3],
    pub horizontal_rule: [u8; 3],
    pub table_rule_strong: [u8; 3],
    pub table_rule_light: [u8; 3],
    pub error: [u8; 3],
}

impl Default for Palette {
    fn default() -> Self {
        Self::light()
    }
}

impl Palette {
    pub const fn light() -> Self {
        Self {
            background: [255, 255, 255],
            foreground: [0, 0, 0],
            link: [31, 89, 173],
            border: [140, 140, 133],
            key_background: [232, 231, 225],
            muted: [91, 89, 84],
            quote_bar: [140, 140, 133],
            horizontal_rule: [179, 176, 166],
            table_rule_strong: [91, 89, 84],
            table_rule_light: [179, 176, 166],
            error: [191, 38, 38],
        }
    }

    pub const fn dark() -> Self {
        Self {
            background: [29, 29, 27],
            foreground: [226, 224, 217],
            link: [112, 164, 238],
            border: [113, 112, 106],
            key_background: [58, 58, 54],
            muted: [174, 172, 164],
            quote_bar: [113, 112, 106],
            horizontal_rule: [91, 90, 85],
            table_rule_strong: [174, 172, 164],
            table_rule_light: [91, 90, 85],
            error: [239, 112, 112],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_pages_are_plain_white() {
        assert_eq!(Palette::light().background, [255, 255, 255]);
    }

    #[test]
    fn partial_theme_inherits_non_palette_defaults() {
        let theme = Theme {
            line_spacing: 1.65,
            heading_scales: [1.6, 1.25, 1.0, 1.0, 1.0, 1.0],
            ..Theme::default()
        };
        assert_eq!(theme.line_spacing, 1.65);
        assert_eq!(theme.heading_scales[0], 1.6);
        assert_eq!(theme.dark, Palette::dark());
        theme.validate().unwrap();
    }
}
