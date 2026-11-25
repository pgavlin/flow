//! Explicit source visibility, not a stylesheet or computed-layout engine.

use cssparser::{Delimiter, Parser, parse_important};
use dom_smoothie::dom_query::NodeRef;

pub(super) fn is_explicitly_hidden(node: NodeRef<'_>) -> bool {
    node.attr("hidden").is_some()
        || node
            .attr("aria-hidden")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || node
            .attr("style")
            .is_some_and(|style| hidden_inline_style(&style))
}

fn hidden_inline_style(style: &str) -> bool {
    let mut input = Parser::new(style);
    let mut display = None;
    let mut visibility = None;
    while !input.is_exhausted() {
        let declaration = input.parse_until_before(
            Delimiter::Semicolon,
            |input| -> Result<_, cssparser::ParseError<()>> {
                let name = input.expect_ident_cloned()?;
                input.expect_colon()?;
                let display_property = name.eq_ignore_ascii_case("display");
                if !display_property && !name.eq_ignore_ascii_case("visibility") {
                    return Err(cssparser::ParseError::custom(()));
                }
                let value = input.expect_ident_cloned()?.to_ascii_lowercase();
                let second = input.try_parse(|input| input.expect_ident_cloned()).ok();
                let important = input.try_parse(parse_important).is_ok();
                input.expect_exhausted()?;
                let hidden = if display_property {
                    if let Some(second) = second {
                        if !matches!(value.as_str(), "block" | "inline" | "run-in")
                            || !matches!(
                                second.to_ascii_lowercase().as_str(),
                                "flow" | "flow-root" | "table" | "flex" | "grid" | "ruby"
                            )
                        {
                            return Err(cssparser::ParseError::custom(()));
                        }
                        false
                    } else {
                        match value.as_str() {
                            "none" => true,
                            "block"
                            | "inline"
                            | "inline-block"
                            | "flex"
                            | "inline-flex"
                            | "grid"
                            | "inline-grid"
                            | "table"
                            | "inline-table"
                            | "table-row"
                            | "table-cell"
                            | "table-row-group"
                            | "table-header-group"
                            | "table-footer-group"
                            | "table-column"
                            | "table-column-group"
                            | "table-caption"
                            | "contents"
                            | "list-item"
                            | "flow-root"
                            | "ruby"
                            | "ruby-base"
                            | "ruby-text"
                            | "ruby-base-container"
                            | "ruby-text-container"
                            | "inherit"
                            | "initial"
                            | "unset"
                            | "revert"
                            | "revert-layer" => false,
                            _ => return Err(cssparser::ParseError::custom(())),
                        }
                    }
                } else {
                    if second.is_some() {
                        return Err(cssparser::ParseError::custom(()));
                    }
                    match value.as_str() {
                        "hidden" | "collapse" => true,
                        "visible" | "inherit" | "initial" | "unset" | "revert" | "revert-layer" => {
                            false
                        }
                        _ => return Err(cssparser::ParseError::custom(())),
                    }
                };
                Ok((display_property, hidden, important))
            },
        );
        if let Ok((display_property, hidden, important)) = declaration {
            let current: &mut Option<(bool, bool)> = if display_property {
                &mut display
            } else {
                &mut visibility
            };
            if current.is_none_or(|(_, previous_important)| !previous_important || important) {
                *current = Some((hidden, important));
            }
        }
        if input.expect_semicolon().is_err() {
            break;
        }
    }
    display.is_some_and(|(hidden, _)| hidden) || visibility.is_some_and(|(hidden, _)| hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comments_escapes_and_declaration_priority() {
        for style in [
            "display:/**/none",
            "DISPLAY: n\\6f ne !IMPORTANT",
            "display:block; display:none",
            "display:none!important; display:block",
            "visibility: hidden ! important",
            "visibility:collapse",
            "display:none;display:bogus",
        ] {
            assert!(hidden_inline_style(style), "{style}");
        }
        for style in [
            "display:none; display:block",
            "display:none;display:inline flow-root",
            "display:none;display:block!important",
            "display:none!important;display:block!important",
            "font-family:'x;display:none'",
            "--x:display:none",
            "visibility:hidden;visibility:visible",
        ] {
            assert!(!hidden_inline_style(style), "{style}");
        }
    }
}
