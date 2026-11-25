//! MediaWiki-owned DOM adaptation; no dependency on web extraction policy.

use super::*;

/// Text fallback excludes inactive descendants even in foreign namespaces.
fn readable_text(root: NodeRef<'_>) -> String {
    let mut text = String::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        if node.has_name("script")
            || node.has_name("style")
            || node.has_name("template")
            || node.has_attr("hidden")
        {
            continue;
        }
        if let Some(value) = node
            .query(|inner| match &inner.data {
                NodeData::Text { contents } => Some(contents.to_string()),
                _ => None,
            })
            .flatten()
        {
            text.push_str(&value);
        } else {
            pending.extend(node.children_it(true));
        }
    }
    text
}

impl Adapter<'_> {
    fn warn(&mut self, kind: DiagnosticKind, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            severity: DiagnosticSeverity::Warning,
            kind,
            origin: None,
            message: message.into(),
        });
    }

    fn append(
        &mut self,
        parent: SourceNodeId,
        kind: SourceNodeKind,
        node: NodeRef<'_>,
    ) -> Result<SourceNodeId, MediaWikiImportError> {
        self.tree
            .append(
                parent,
                kind,
                None,
                NodeProvenance::Original {
                    identity: Some(SourceNodeIdentity(identity(node))),
                },
            )
            .map_err(|_| MediaWikiImportError::TooComplex)
    }

    fn fallback(
        &mut self,
        parent: SourceNodeId,
        node: NodeRef<'_>,
        attrs: Vec<Attribute>,
        text: String,
    ) -> Result<(), MediaWikiImportError> {
        let wrapper = self.append(
            parent,
            SourceNodeKind::Element(Element::new(element_name("span"), attrs)),
            node,
        )?;
        self.append(wrapper, SourceNodeKind::Text(text), node)?;
        Ok(())
    }

    pub(super) fn adapt(
        &mut self,
        root: NodeRef<'_>,
        parent: SourceNodeId,
    ) -> Result<(), MediaWikiImportError> {
        // Track extension trust per subtree, independently of ordinary HTML semantics.
        let mut pending = vec![(root, parent, true)];
        while let Some((node, parent, core)) = pending.pop() {
            let Some(data) = node.query(|inner| inner.data.clone()) else {
                continue;
            };
            let mut children = true;
            let mut child_core = core;
            let mut label = None;
            let kind = match data {
                NodeData::Text { contents } => SourceNodeKind::Text(contents.to_string()),
                NodeData::Element(element) => {
                    let html = &*element.name.ns == HTML_NS;
                    let tag = element.name.local.as_ref();
                    if html
                        && (matches!(
                            tag,
                            "script" | "style" | "head" | "link" | "meta" | "template"
                        ) || node.has_attr("hidden"))
                    {
                        continue;
                    }
                    if html
                        && tag == "tr"
                        && !node
                            .children()
                            .into_iter()
                            .any(|child| child.has_name("td") || child.has_name("th"))
                    {
                        self.warn(
                            DiagnosticKind::InvalidSpan,
                            "empty table row omitted; spans are normalized to retained rows",
                        );
                        continue;
                    }
                    let extension = extension(node);
                    let math = core && vocabulary(node, "typeof", "Extension/math");
                    let cite_ref = core && vocabulary(node, "typeof", "Extension/ref");
                    let cite_body = core && vocabulary(node, "typeof", "Extension/references");
                    if extension && !(math || cite_ref || cite_body) {
                        child_core = false;
                        self.warn(DiagnosticKind::DroppedNonTextualMetadata, "unknown extension retained as ordinary HTML without extension semantics");
                    }
                    let mut attrs = Vec::new();
                    for attr in element.attrs {
                        let local = attr.name.local.to_string();
                        // Private adapter keys can only be assigned here, never by source markup.
                        if local.starts_with("data-flow-import-") {
                            continue;
                        }
                        let ns = (!attr.name.ns.is_empty()).then(|| attr.name.ns.to_string());
                        let prefix = attr.name.prefix.map(|prefix| prefix.to_string());
                        let qualified = prefix
                            .as_ref()
                            .map_or_else(|| local.clone(), |prefix| format!("{prefix}:{local}"));
                        let mut value = attr.value.to_string();
                        if ns.is_none() && local == "src" && value.trim().is_empty() {
                            continue;
                        }
                        if ns.is_none() && matches!(local.as_str(), "href" | "src") {
                            value = value
                                .trim_matches(|c: char| c.is_ascii_whitespace())
                                .to_owned();
                            if local == "href" && value.starts_with('#') {
                                // Parsoid fragment-only links remain local regardless of <base>.
                            } else {
                                value = reference(&value, self.base);
                                if let Ok(mut url) = Url::parse(&value) {
                                    let allowed = matches!(url.scheme(), "http" | "https")
                                        || (local == "href" && url.scheme() == "mailto")
                                        || (local == "src" && url.scheme() == "data");
                                    if !allowed {
                                        self.warn(
                                            DiagnosticKind::InvalidDestination,
                                            "unsupported URL scheme omitted",
                                        );
                                        continue;
                                    }
                                    if local == "href"
                                        && let Some(fragment) = url.fragment().map(str::to_owned)
                                    {
                                        url.set_fragment(None);
                                        if self.source_url.is_some_and(|source| {
                                            let mut source = source.clone();
                                            source.set_fragment(None);
                                            source == url
                                        }) {
                                            value = format!("#{fragment}");
                                        }
                                    }
                                }
                            }
                        }
                        attrs.push(Attribute::new(
                            ExpandedName::new(qualified, local, prefix, ns),
                            value,
                            None,
                        ));
                    }
                    if node.id == root.id {
                        for inherited in ["lang", "dir"] {
                            if node.attr(inherited).is_none()
                                && let Some(value) = node
                                    .ancestors(None)
                                    .into_iter()
                                    .find_map(|ancestor| ancestor.attr(inherited))
                            {
                                attrs.push(attribute(inherited, value.to_string()));
                            }
                        }
                    }
                    let key = identity(node);
                    attrs.push(attribute("data-flow-import-key", key.clone()));
                    if html
                        && tag == "a"
                        && node.attr("id").is_none()
                        && let Some(name) = node.attr("name")
                    {
                        attrs.push(attribute("id", name.to_string()));
                    }
                    if cite_ref
                        && !node
                            .descendants_it()
                            .any(|child| child.has_name("a") && child.has_attr("href"))
                    {
                        self.warn(
                            DiagnosticKind::InvalidDestination,
                            "reference has no usable note link; visible fallback retained",
                        );
                    }
                    if core
                        && html
                        && tag == "a"
                        && let Some(call_site) = node
                            .ancestors(None)
                            .into_iter()
                            .find(|ancestor| vocabulary(*ancestor, "typeof", "Extension/ref"))
                    {
                        let return_anchor =
                            call_site.attr("id").map(|id| AnchorId::new(id.to_string()));
                        if return_anchor.is_none() {
                            self.warn(
                                DiagnosticKind::InvalidDestination,
                                "reference call site has no return anchor",
                            );
                        }
                        self.policy
                            .relations
                            .insert(key.clone(), LinkRelation::NoteReference { return_anchor });
                    }
                    if core
                        && html
                        && tag == "li"
                        && node
                            .ancestors(None)
                            .into_iter()
                            .any(|ancestor| vocabulary(ancestor, "typeof", "Extension/references"))
                    {
                        self.policy.notes.insert(key.clone());
                    }
                    if core
                        && html
                        && tag == "a"
                        && node
                            .ancestors(None)
                            .into_iter()
                            .any(|ancestor| class(ancestor, "mw-cite-backlink"))
                        && node
                            .ancestors(None)
                            .into_iter()
                            .any(|ancestor| vocabulary(ancestor, "typeof", "Extension/references"))
                    {
                        self.policy
                            .relations
                            .insert(key.clone(), LinkRelation::NoteReturn);
                    }
                    if core
                        && html
                        && tag == "a"
                        && vocabulary(node, "rel", "ExtLink")
                        && class(node, "autonumber")
                        && node.text().trim().is_empty()
                        && node.children().is_empty()
                    {
                        self.numbering += 1;
                        label = Some(format!("[{}]", self.numbering));
                    }
                    if html && matches!(tag, "object" | "embed") {
                        self.warn(
                            DiagnosticKind::UnsupportedMedia,
                            "unsupported embedded object retained as ordinary fallback content",
                        );
                    }
                    if html && tag == "img" {
                        let (responsive, fallback, diagnostics) =
                            responsive::capture(node, self.base);
                        self.diagnostics.extend(diagnostics);
                        if let Some(responsive) = responsive {
                            self.policy.images.insert(key.clone(), responsive);
                        }
                        if !attrs
                            .iter()
                            .any(|attr| attr.name().local() == "src" && !attr.value().is_empty())
                        {
                            if let Some(fallback) = fallback {
                                attrs.retain(|attr| attr.name().local() != "src");
                                attrs.push(attribute("src", fallback));
                            } else {
                                self.warn(
                                    DiagnosticKind::UnsupportedMedia,
                                    "image has no usable resource; alternative text retained",
                                );
                                self.fallback(
                                    parent,
                                    node,
                                    attrs,
                                    node.attr("alt").unwrap_or_default().to_string(),
                                )?;
                                continue;
                            }
                        }
                    }
                    let tex = if math { self.math(node) } else { None };
                    let name = if let Some((tex, display)) = tex {
                        self.policy.math.insert(key, (tex, display));
                        children = false;
                        element_name(if display == MathDisplay::Block {
                            "div"
                        } else {
                            "span"
                        })
                    } else if math {
                        // A malformed Math contract must still yield just one fallback.
                        let fallback = node
                            .attr("alttext")
                            .map(|value| value.to_string())
                            .or_else(|| {
                                node.descendants_it()
                                    .find(|child| child.has_name("img"))
                                    .and_then(|image| image.attr("alt"))
                                    .map(|value| value.to_string())
                            })
                            .unwrap_or_else(|| readable_text(node));
                        self.fallback(parent, node, attrs, fallback)?;
                        continue;
                    } else if !html {
                        // Keep one useful fallback, but never interpret foreign local names as HTML.
                        self.warn(
                            DiagnosticKind::UnsupportedMedia,
                            "foreign content retained as text fallback",
                        );
                        self.fallback(parent, node, attrs, readable_text(node))?;
                        continue;
                    } else if tag == "table"
                        && !node
                            .descendants_it()
                            .any(|child| child.has_name("td") || child.has_name("th"))
                    {
                        self.warn(
                            DiagnosticKind::InvalidSpan,
                            "table without cells retained as ordinary content",
                        );
                        element_name("div")
                    } else {
                        element_name(match tag {
                            "b" => "strong",
                            "i" => "em",
                            other => other,
                        })
                    };
                    SourceNodeKind::Element(Element::new(name, attrs))
                }
                _ => continue,
            };
            let id = self.append(parent, kind, node)?;
            if node.has_name("a")
                && let (Some(name), Some(authored_id)) = (node.attr("name"), node.attr("id"))
                && name != authored_id
            {
                self.append(
                    id,
                    SourceNodeKind::Element(Element::new(
                        element_name("span"),
                        vec![attribute("id", name.to_string())],
                    )),
                    node,
                )?;
            }
            if let Some(label) = label {
                self.tree
                    .append(
                        id,
                        SourceNodeKind::Text(label),
                        None,
                        NodeProvenance::Generated {
                            reason: GeneratedReason::LinkLabel,
                            related: id,
                        },
                    )
                    .map_err(|_| MediaWikiImportError::TooComplex)?;
            }
            if children {
                pending.extend(node.children_it(true).map(|child| (child, id, child_core)));
            }
        }
        Ok(())
    }

    fn math(&mut self, node: NodeRef<'_>) -> Option<(String, MathDisplay)> {
        let result = node.attr("data-mw").and_then(|data| {
            // serde_json's default recursion limit bounds nested metadata; the
            // parser's attribute/input budgets bound its total bytes.
            let value: serde_json::Value = serde_json::from_str(&data).ok()?;
            if value.get("name")?.as_str()? != "math" {
                return None;
            }
            let tex = value.get("body")?.get("extsrc")?.as_str()?.to_owned();
            if tex.is_empty() {
                return None;
            }
            let block = value
                .get("attrs")
                .and_then(|attrs| attrs.get("display"))
                .and_then(|display| display.as_str())
                == Some("block")
                || node
                    .attr("display")
                    .is_some_and(|display| display.as_ref() == "block")
                || class(node, "mwe-math-element-block");
            Some((
                tex,
                if block {
                    MathDisplay::Block
                } else {
                    MathDisplay::Inline
                },
            ))
        });
        if result.is_none() {
            self.warn(
                DiagnosticKind::UnsupportedMedia,
                "Math extension has no reliable TeX metadata; retained ordinary fallback",
            );
        }
        result
    }
}
