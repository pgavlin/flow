use super::*;

pub(super) fn effective_base(
    document: &Document,
    options: &MediaWikiOptions,
    source: Option<&Url>,
) -> Result<Option<Url>, MediaWikiImportError> {
    if let Some(explicit) = &options.resource_base {
        return absolute_url(explicit).map(Some);
    }
    if options.input == MediaWikiInput::Document {
        for node in document.select("head base[href]").nodes() {
            if !html_node(*node) {
                continue;
            }
            if let Some(value) = node.attr("href")
                && let Ok(base) = Url::parse(&value).or_else(|_| {
                    source
                        .ok_or(url::ParseError::RelativeUrlWithoutBase)?
                        .join(&value)
                })
                && matches!(base.scheme(), "http" | "https")
            {
                return Ok(Some(base));
            }
        }
    }
    Ok(source.cloned())
}

pub(super) fn extract(
    document: &Document,
    options: &MediaWikiOptions,
    base: Option<&Url>,
) -> Result<MediaWikiMetadata, MediaWikiImportError> {
    let mut declared = None;
    if options.input == MediaWikiInput::Document {
        for node in document.select("head meta").nodes() {
            if !html_node(*node) {
                continue;
            }
            if (vocabulary(*node, "property", "htmlVersion")
                || vocabulary(*node, "property", "html:version"))
                && let Some(value) = node.attr("content")
            {
                let version = value.parse()?;
                if declared.is_some_and(|previous| previous != version) {
                    return Err(MediaWikiImportError::ConflictingVersions);
                }
                declared = Some(version);
            }
        }
    }
    if let (Some(declared), Some(expected)) = (declared, options.expected_version)
        && declared != expected
    {
        return Err(MediaWikiImportError::ConflictingVersions);
    }
    let effective = declared
        .or(options.expected_version)
        .ok_or(MediaWikiImportError::MissingVersion)?;
    if effective != MediaWikiHtmlVersion::V2_8_0 {
        return Err(MediaWikiImportError::UnsupportedVersion(effective));
    }
    let mut metadata = MediaWikiMetadata {
        declared_version: declared,
        effective_version: effective,
        database_title: (options.input == MediaWikiInput::Document)
            .then(|| document.select("head title").text().to_string())
            .filter(|title| !title.is_empty()),
        categories: Vec::new(),
        language_links: Vec::new(),
        redirect: None,
    };
    for node in document.select("link[href]").nodes() {
        if !html_node(*node) {
            continue;
        }
        // Inactive and unknown-extension contents are not core page metadata.
        if node.ancestors(None).into_iter().any(|ancestor| {
            ancestor.has_name("template")
                || ancestor.attr("hidden").is_some()
                || extension(ancestor)
        }) {
            continue;
        }
        let raw = node.attr("href").unwrap().to_string();
        let target = MediaWikiReference {
            resolved: reference(&raw, base),
            raw: raw.clone(),
        };
        if vocabulary(*node, "rel", "PageProp/Category") {
            let sort_key = raw.split_once('#').map(|(_, key)| key.to_owned());
            metadata
                .categories
                .push(MediaWikiCategory { target, sort_key });
        } else if vocabulary(*node, "rel", "PageProp/Language") {
            metadata.language_links.push(target);
        } else if vocabulary(*node, "rel", "PageProp/redirect") {
            metadata.redirect = Some(target);
        }
    }
    Ok(metadata)
}
