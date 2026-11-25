//! Preserve responsive choices independently of extraction's DOM rewrites.

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;

use super::*;
use crate::model::flow::{
    FlowBuilder, ImageCandidate, ImageCandidateDescriptor, ImageCandidateSet, ImageSource,
    Resource, ResponsiveImage,
};

#[derive(Default)]
pub(super) struct ResponsiveImages {
    images: HashMap<String, PendingImage>,
    synthetic_fallbacks: HashSet<String>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Default)]
struct PendingImage {
    sources: Vec<PendingSource>,
    fallback: Option<PendingSet>,
}

struct PendingSource {
    media: Option<String>,
    media_type: Option<String>,
    set: PendingSet,
}

struct PendingSet {
    candidates: Vec<(String, ImageCandidateDescriptor)>,
    sizes: Option<String>,
}

fn attribute(node: NodeRef<'_>, key: &str) -> Option<String> {
    node.attr(key)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.to_string())
}

impl ResponsiveImages {
    pub fn capture(document: &Document, base: Option<&Url>) -> Self {
        let mut result = Self::default();
        for image in document.select("img").nodes() {
            let Some(identity) = image.attr(SOURCE_NODE_ATTRIBUTE) else {
                continue;
            };
            let fallback = result.set(*image, base, true);
            let mut sources = Vec::new();
            if let Some(picture) = image.parent().filter(|parent| parent.has_name("picture")) {
                for source in picture.children() {
                    if source.id == image.id {
                        break;
                    }
                    if !source.has_name("source") {
                        continue;
                    }
                    if let Some(set) = result.set(source, base, false) {
                        sources.push(PendingSource {
                            media: attribute(source, "media"),
                            media_type: attribute(source, "type"),
                            set,
                        });
                    }
                }
            }
            if image.attr("src").is_none_or(|src| {
                src.trim().is_empty()
                    || resolve(&src, base)
                        .is_some_and(|url| !matches!(url.scheme(), "http" | "https" | "data"))
            }) && let Some((identity, _)) = fallback
                .as_ref()
                .or_else(|| sources.first().map(|source| &source.set))
                .and_then(|set| set.candidates.first())
            {
                image.set_attr("src", identity);
                result
                    .synthetic_fallbacks
                    .insert(image.attr(SOURCE_NODE_ATTRIBUTE).unwrap().to_string());
            }
            if !sources.is_empty() || fallback.is_some() {
                result
                    .images
                    .insert(identity.to_string(), PendingImage { sources, fallback });
            }
        }
        result
    }

    /// Extraction can recover lazy-image srcsets. Prefer that recovered set while
    /// retaining the original picture sources, which extraction no longer sees.
    pub fn recover(&mut self, node: NodeRef<'_>, base: Option<&Url>) {
        let Some(identity) = node.attr(SOURCE_NODE_ATTRIBUTE) else {
            return;
        };
        let include_src = !self.synthetic_fallbacks.contains(identity.as_ref());
        if let Some(set) = self.set(node, base, include_src) {
            self.images
                .entry(identity.to_string())
                .or_default()
                .fallback = Some(set);
        }
    }

    fn set(
        &mut self,
        node: NodeRef<'_>,
        base: Option<&Url>,
        include_src: bool,
    ) -> Option<PendingSet> {
        let input = node.attr("srcset")?;
        let (parsed, mut rejected) = parse_srcset(&input);
        let mut candidates = Vec::new();
        for (identity, descriptor) in parsed {
            let identity = match resolve(identity, base) {
                Some(url) if matches!(url.scheme(), "http" | "https" | "data") => url.into(),
                None if base.is_none() => identity.to_owned(),
                _ => {
                    rejected = true;
                    continue;
                }
            };
            let same_kind = candidates.first().is_none_or(|(_, first)| {
                matches!(
                    (first, descriptor),
                    (
                        ImageCandidateDescriptor::Width(_),
                        ImageCandidateDescriptor::Width(_)
                    ) | (
                        ImageCandidateDescriptor::Density(_),
                        ImageCandidateDescriptor::Density(_)
                    )
                )
            });
            if same_kind
                && !candidates
                    .iter()
                    .any(|(_, previous)| *previous == descriptor)
            {
                candidates.push((identity, descriptor));
            } else {
                rejected = true;
            }
        }
        // In a density set, an explicit img src supplies the implicit 1x
        // candidate unless srcset already supplies one. Do not confuse our
        // synthesized deterministic fallback with an author-provided src.
        if include_src
            && node.has_name("img")
            && matches!(
                candidates.first(),
                Some((_, ImageCandidateDescriptor::Density(_)))
            )
            && !candidates
                .iter()
                .any(|(_, descriptor)| *descriptor == ImageCandidateDescriptor::Density(1.0))
            && let Some(src) = attribute(node, "src")
        {
            let identity = match resolve(&src, base) {
                Some(url) if matches!(url.scheme(), "http" | "https" | "data") => Some(url.into()),
                None if base.is_none() => Some(src),
                _ => None,
            };
            if let Some(identity) = identity {
                candidates.push((identity, ImageCandidateDescriptor::Density(1.0)));
            }
        }
        if rejected {
            self.diagnostics.push(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                kind: DiagnosticKind::DroppedNonTextualMetadata,
                origin: None,
                message: "invalid or disallowed responsive image candidates were omitted".into(),
            });
        }
        (!candidates.is_empty()).then(|| PendingSet {
            candidates,
            sizes: attribute(node, "sizes"),
        })
    }

    pub fn lower(
        &self,
        element: &Element,
        builder: &mut FlowBuilder,
    ) -> Result<Option<ResponsiveImage>, BuildError> {
        let Some(image) = element
            .attribute(None, SOURCE_NODE_ATTRIBUTE)
            .and_then(|identity| self.images.get(identity))
        else {
            return Ok(None);
        };
        fn lower_set(
            set: &PendingSet,
            media_type: Option<&str>,
            builder: &mut FlowBuilder,
        ) -> Result<ImageCandidateSet, BuildError> {
            let candidates = set
                .candidates
                .iter()
                .map(|(identity, descriptor)| {
                    Ok(ImageCandidate {
                        resource: builder.add_resource(Resource {
                            identity: identity.clone(),
                            media_type: media_type.map(str::to_owned),
                        })?,
                        descriptor: *descriptor,
                    })
                })
                .collect::<Result<_, BuildError>>()?;
            Ok(ImageCandidateSet {
                candidates,
                sizes: set.sizes.clone(),
            })
        }
        Ok(Some(ResponsiveImage {
            sources: image
                .sources
                .iter()
                .map(|source| {
                    Ok(ImageSource {
                        media: source.media.clone(),
                        media_type: source.media_type.clone(),
                        candidates: lower_set(&source.set, source.media_type.as_deref(), builder)?,
                    })
                })
                .collect::<Result<_, BuildError>>()?,
            fallback: image
                .fallback
                .as_ref()
                .map(|set| lower_set(set, None, builder))
                .transpose()?,
        }))
    }
}

/// Tokenize URLs before descriptors: commas inside data URLs are not separators.
/// Invalid entries are skipped without discarding valid neighboring candidates.
pub(super) fn parse_srcset(mut input: &str) -> (Vec<(&str, ImageCandidateDescriptor)>, bool) {
    use ImageCandidateDescriptor::{Density, Width};
    let mut candidates = Vec::new();
    let mut rejected = false;
    while !input.is_empty() {
        input = input.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if input.is_empty() {
            break;
        }
        let end = input
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(input.len());
        let mut identity = &input[..end];
        input = &input[end..];
        let descriptor = if identity.ends_with(',') {
            identity = identity.trim_end_matches(',');
            Some(Density(1.0))
        } else {
            // Parentheses belong to descriptor tokens, not candidate separators.
            let mut depth = 0usize;
            let end = input
                .char_indices()
                .find_map(|(index, c)| {
                    match c {
                        '(' => depth += 1,
                        ')' => depth = depth.saturating_sub(1),
                        ',' if depth == 0 => return Some(index),
                        _ => {}
                    }
                    None
                })
                .unwrap_or(input.len());
            let tokens: Vec<_> = input[..end].split_ascii_whitespace().collect();
            input = &input[end..];
            match tokens.as_slice() {
                [] => Some(Density(1.0)),
                [token] => token
                    .strip_suffix('w')
                    .and_then(|value| {
                        value
                            .bytes()
                            .all(|c| c.is_ascii_digit())
                            .then(|| value.parse::<u32>().ok().and_then(NonZeroU32::new))
                            .flatten()
                            .map(Width)
                    })
                    .or_else(|| {
                        token
                            .strip_suffix('x')
                            .filter(|value| {
                                !value.starts_with('+')
                                    && !value
                                        .split(['e', 'E'])
                                        .next()
                                        .unwrap_or_default()
                                        .ends_with('.')
                            })
                            .and_then(|value| value.parse::<f32>().ok())
                            .filter(|value| value.is_finite() && *value > 0.0)
                            .map(Density)
                    }),
                _ => None,
            }
        };
        if let Some(descriptor) = descriptor.filter(|_| !identity.is_empty()) {
            candidates.push((identity, descriptor));
            continue;
        }
        rejected = true;
    }
    (candidates, rejected)
}
