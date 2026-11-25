//! MediaWiki-owned capture of static HTML responsive alternatives. No selection
//! engine, lazy-image recovery, DOM rewrites, or resource acquisition.

use super::*;
use std::num::NonZeroU32;

pub(super) struct PendingImage {
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

impl PendingSet {
    fn lower(&self, builder: &mut FlowBuilder) -> Result<ImageCandidateSet, BuildError> {
        let candidates = self
            .candidates
            .iter()
            .map(|(identity, descriptor)| {
                let resource = builder.add_resource(Resource {
                    identity: identity.clone(),
                    media_type: None,
                })?;
                Ok(ImageCandidate {
                    resource,
                    descriptor: *descriptor,
                })
            })
            .collect::<Result<Vec<_>, BuildError>>()?;
        Ok(ImageCandidateSet {
            candidates,
            sizes: self.sizes.clone(),
        })
    }
}
impl PendingImage {
    pub fn lower(&self, builder: &mut FlowBuilder) -> Result<ResponsiveImage, BuildError> {
        let sources = self
            .sources
            .iter()
            .map(|source| {
                Ok(ImageSource {
                    media: source.media.clone(),
                    media_type: source.media_type.clone(),
                    candidates: source.set.lower(builder)?,
                })
            })
            .collect::<Result<Vec<_>, BuildError>>()?;
        Ok(ResponsiveImage {
            sources,
            fallback: self
                .fallback
                .as_ref()
                .map(|set| set.lower(builder))
                .transpose()?,
        })
    }
}

fn value(node: NodeRef<'_>, key: &str) -> Option<String> {
    node.attr(key)
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.to_string())
}

fn image_url(raw: &str, base: Option<&Url>) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let resolved = reference(raw, base);
    if let Ok(url) = Url::parse(&resolved)
        && !matches!(url.scheme(), "http" | "https" | "data")
    {
        return None;
    }
    Some(resolved)
}

fn candidate_set(
    node: NodeRef<'_>,
    base: Option<&Url>,
    implicit_src: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<PendingSet> {
    let input = value(node, "srcset")?;
    let mut rest = input.as_str();
    let mut candidates = Vec::new();
    let mut rejected = false;
    while !rest.is_empty() {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
        if rest.is_empty() {
            break;
        }
        // URLs can contain commas (notably data URLs). A trailing comma ends a
        // descriptorless candidate; otherwise descriptors run to the next comma.
        let end = rest
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let token = &rest[..end];
        rest = &rest[end..];
        let (url, descriptors) = if token.ends_with(',') {
            (token.trim_end_matches(','), "")
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            let descriptors = rest[..end].trim();
            rest = rest.get(end + 1..).unwrap_or("");
            (token, descriptors)
        };
        let descriptor = if descriptors.is_empty() {
            Some(ImageCandidateDescriptor::Density(1.0))
        } else if descriptors.split_ascii_whitespace().count() != 1 {
            None
        } else if let Some(width) = descriptors.strip_suffix('w') {
            if width.bytes().all(|c| c.is_ascii_digit()) {
                width
                    .parse::<u32>()
                    .ok()
                    .and_then(NonZeroU32::new)
                    .map(ImageCandidateDescriptor::Width)
            } else {
                None
            }
        } else if let Some(density) = descriptors.strip_suffix('x') {
            density
                .parse::<f32>()
                .ok()
                .filter(|value| value.is_finite() && *value > 0.0)
                .map(ImageCandidateDescriptor::Density)
        } else {
            None
        };
        let Some((url, descriptor)) = image_url(url, base).zip(descriptor) else {
            rejected = true;
            continue;
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
            candidates.push((url, descriptor));
        } else {
            rejected = true;
        }
    }
    if implicit_src
        && matches!(
            candidates.first(),
            Some((_, ImageCandidateDescriptor::Density(_)))
        )
        && !candidates
            .iter()
            .any(|(_, descriptor)| *descriptor == ImageCandidateDescriptor::Density(1.0))
        && let Some(src) = value(node, "src").and_then(|src| image_url(&src, base))
    {
        candidates.push((src, ImageCandidateDescriptor::Density(1.0)));
    }
    if rejected {
        diagnostics.push(Diagnostic {
            severity: DiagnosticSeverity::Warning,
            kind: DiagnosticKind::DroppedNonTextualMetadata,
            origin: None,
            message: "invalid responsive image candidates omitted".into(),
        });
    }
    (!candidates.is_empty()).then(|| PendingSet {
        candidates,
        sizes: value(node, "sizes"),
    })
}

pub(super) fn capture(
    node: NodeRef<'_>,
    base: Option<&Url>,
) -> (Option<PendingImage>, Option<String>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let fallback = candidate_set(node, base, true, &mut diagnostics);
    let mut sources = Vec::new();
    if let Some(picture) = node.parent().filter(|parent| parent.has_name("picture")) {
        for child in picture.children() {
            if child.id == node.id {
                break;
            }
            if child.has_name("source")
                && let Some(set) = candidate_set(child, base, false, &mut diagnostics)
            {
                sources.push(PendingSource {
                    media: value(child, "media"),
                    media_type: value(child, "type"),
                    set,
                });
            }
        }
    }
    let resource = value(node, "src")
        .and_then(|src| image_url(&src, base))
        .or_else(|| {
            fallback
                .as_ref()
                .or_else(|| sources.first().map(|source| &source.set))
                .and_then(|set| set.candidates.first())
                .map(|(url, _)| url.clone())
        });
    let image =
        (!sources.is_empty() || fallback.is_some()).then_some(PendingImage { sources, fallback });
    (image, resource, diagnostics)
}
