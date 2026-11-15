# flow semantic XHTML

flow semantic XHTML is a semantic document format owned and maintained by flow.
It describes readable content and its structure, not a browser DOM or application
UI. Its structural vocabulary is informed by the
[WHATWG HTML content-category model](https://html.spec.whatwg.org/multipage/dom.html#kinds-of-content),
particularly flow and phrasing content.

This document defines flow semantic XHTML's vocabulary and
authoring rules for version 1.

## 1. Serialization and document structure

A document is a UTF-8 XML fragment: an ordered sequence of block elements. A
single enclosing element, XML declaration, document type declaration, `head`,
and `body` are not required. Do not include a document type declaration or
entity declarations.

A minimal document is:

```xml
<h1 id="greeting">Hello, world</h1>
<p>This is a <em>flow semantic XHTML</em> document.</p>
```

Use these XML rules throughout:

- Element and attribute names are case-sensitive; use the lowercase names
  specified here.
- Close every element. Empty elements may use self-closing syntax, such as
  `<br/>`, `<hr/>`, and `<img src="diagram.png" alt="A diagram"/>`.
- Quote every attribute value. Boolean attributes are expressed by presence:
  `open=""` is true; omit the attribute for false. `open="false"` is still true.
- Nest elements properly. For example, `<em><strong>text</strong></em>` is valid;
  overlapping start and end tags are not.
- Write `&amp;` for a literal ampersand and `&lt;` for a literal less-than sign.
  The five predefined entities are `&amp;`, `&lt;`, `&gt;`, `&quot;`, and
  `&apos;`. Numeric character references such as `&#160;` and `&#xA0;` are also
  allowed. HTML-only named entities such as `&nbsp;` are not allowed.
- Only XML 1.0 characters are allowed. In particular, most ASCII control
  characters are forbidden.
- CDATA sections are allowed and supply literal text, not nested markup.
  Comments and processing instructions do not contribute readable content.

Unqualified elements are the normal spelling. A wrapper may instead declare the
XHTML namespace, `xmlns="http://www.w3.org/1999/xhtml"`. Ordinary attributes
remain unqualified; `xml:lang` is the exception described below. Do not introduce
foreign vocabularies or depend on namespace prefixes to add semantics.

### Content categories

**Phrasing content** is text interspersed with the inline elements specified in
section 4, inline images, and inline mathematics. It must not contain paragraphs,
headings, lists, tables, or other block elements.

**Block content** is a sequence of paragraphs, headings, thematic breaks,
containers, quotations, lists, definition lists, preformatted blocks, figures,
tables, disclosures, media objects, or display mathematics. Whitespace between
blocks is allowed and does not create content.

Use explicit paragraphs for prose in block content. Bare text or phrasing at a
block boundary is accepted as an anonymous paragraph, but explicit `<p>` elements
avoid ambiguity. In particular, a `<code>` directly in block content is treated
as preformatted code, not as an inline code span.

Reading order is source order, except for the specialized structures described
below. Heading levels do not automatically create enclosing sections, and
heading text does not automatically generate an anchor ID.

## 2. Shared attributes

The following attributes apply to semantic elements unless a specialized rule
below says otherwise.

| Attribute | Meaning |
| --- | --- |
| `id` | An explicit, document-local link target. Use a nonempty value that is unique within the document. |
| `lang` | A language tag, such as `en`, `en-US`, `ar`, or `ja`. Inherited by descendants unless overridden. |
| `xml:lang` | An alternative spelling for language, using the standard XML namespace. `lang` takes precedence when both are present. |
| `dir` | Text direction: exactly `ltr`, `rtl`, or `auto`. Inherited unless overridden. `auto` requests automatic direction. |

`lang=""` explicitly marks language as unknown rather than inheriting it.
An invalid language tag is ignored with a diagnostic; an unrecognized `dir`
value supplies no override. Duplicate IDs are an authoring error: only the first
target is retained.

```xml
<div lang="en">
  <p>English prose.</p>
  <p lang="ar" dir="rtl">مرحبا <span lang="en" dir="ltr">flow</span></p>
  <p lang="">Text whose language is unknown.</p>
</div>
```

There is no general CSS presentation contract. `style` attributes and
stylesheets do not define layout in this format. `class` has meaning only for
the code-language and mathematics conventions specified below. Other attributes
have no defined effect unless listed here. Do not rely on HTML browser defaults,
event handlers, automatic tooltips, or arbitrary `data-*` attributes.

## 3. Paragraphs, headings, and containers

| Element | Content | Meaning |
| --- | --- | --- |
| `p` | Phrasing | A paragraph. |
| `h1` through `h6` | Phrasing | A heading at the numbered level, from 1 through 6. |
| `hr` | Empty | A thematic break. |
| `div`, `section`, `article`, `main`, `header`, `footer`, `aside`, `nav`, `address` | Block | A grouping container. These names do not imply distinct navigation or landmark behavior. |
| `blockquote` | Block | A quotation, which may contain multiple blocks and nested quotations. |

Containers may nest. Do not put block elements inside a paragraph or heading.
To include a literal line break within phrasing content, use `<br/>`; a newline
in the source alone is not a hard break.

```xml
<section id="observations">
  <h2>Observations</h2>
  <p>First line.<br/>Second line of the same paragraph.</p>
  <blockquote>
    <p>A quoted paragraph.</p>
    <p>Another paragraph in the same quotation.</p>
  </blockquote>
  <hr/>
  <p>The discussion continues.</p>
</section>
```

## 4. Inline content and whitespace

All paired elements in this table contain phrasing content and may nest, subject
to the prohibition on nested links.

| Element | Meaning and specific attributes |
| --- | --- |
| `span` | A neutral inline group, useful for `lang`, `dir`, and `id`. |
| `em` | Emphasis. |
| `strong` | Strong importance. |
| `code` | Inline code when used inside phrasing content. |
| `sub` | Subscript. |
| `sup` | Superscript. |
| `abbr` | An abbreviation. An expansion tooltip is not specified. |
| `cite` | A cited work or citation. |
| `q` | A quotation. Include any required quotation marks in the text. |
| `mark` | Marked or highlighted text. |
| `time` | A time expression. Optional `datetime` retains a machine-readable value; visible text is still supplied by the children. |
| `a` | A link when `href` is present; otherwise a neutral inline group. See section 5. |
| `br` | An empty element representing a hard line break. |

Semantic roles do not prescribe exact fonts, colors, punctuation, or decoration.
For example, `time` does not generate a date label, and `q` does not require a
presenter to insert quotation marks.

In ordinary phrasing, runs of spaces, tabs, and source line breaks collapse to a
single space, including across inline element boundaries. Leading and trailing
collapsible whitespace at paragraph boundaries is discarded. `<br/>` discards
pending whitespace and starts a new line. Inline code uses these same whitespace
rules; use a preformatted block when exact spacing matters.

Nonbreaking spaces are preserved. To write one, use a literal U+00A0 or `&#160;`,
not `&nbsp;`. Escaping and character-reference decoding take place before
whitespace handling.

```xml
<p><strong>Important:</strong> keep H<sub>2</sub>O at 20&#160;°C.
Read <cite>The Field Guide</cite> before
<time datetime="2026-11-07">November 7</time>.</p>
```

## 5. Links and resource references

`a` uses `href` to identify a destination:

- `href="#results"` targets the element whose `id` is `results` in this document.
- `href="next.xhtml"` refers to another document relative to the document's
  externally supplied resource base.
- `href="next.xhtml#results"` refers to a target in that other document.
- An absolute URI with a scheme, such as `https://example.test/guide` or
  `mailto:reader@example.test`, is an external destination.

Fragments in link destinations are percent-decoded before matching IDs. Use a
literal ID value in the `id` attribute, and percent-encode it in a URI fragment
when necessary. IDs are case-sensitive. A link need not resolve during document
construction; the host handles document loading and external activation.

Links contain phrasing content, including images, but must not contain other
links or block elements. `title`, `target`, `rel`, and `download` do not add
specified link behavior. Footnote relationships are not encoded by this
vocabulary; ordinary links can connect a reference and a note explicitly.

Resource references such as image and media `src` values may also be absolute or
relative. They identify resources; they do not embed bytes or require automatic
network access. The resource base is supplied with the document, not through a
`base` element or `xml:base`. Hosts decide what URI schemes, resources, and
external actions are permitted. The format is not a URL-safety policy.

XML escaping also applies inside URIs:

```xml
<p><a href="#results">See the results</a> or
<a href="https://example.test/search?a=1&amp;b=2">search online</a>.</p>
<h2 id="results">Results</h2>
<p><a href="appendix.xhtml#methods">Methods in the appendix</a>.</p>
```

## 6. Lists and definitions

### Ordered and unordered lists

`ul` and `ol` contain one or more `li` elements. Each `li` contains block content.
Put nested lists inside an item, not directly inside another list.

`ul` is unordered. `ol` is ordered and accepts:

- `start`: a signed decimal integer; defaults to `1`.
- `reversed`: a boolean attribute indicating descending numbering. Its presence
  does not change the default `start`; specify the intended starting number.

An `li` may supply `value`, a signed decimal integer overriding that item's
ordinal. Integer values must fit in a signed 64-bit integer. List marker styles,
HTML `type`, and interactive checkbox controls are not specified.

```xml
<ol start="3" reversed="">
  <li><p>Third choice.</p></li>
  <li value="2">
    <p>Second choice, with supporting points:</p>
    <ul>
      <li><p>First point.</p></li>
      <li><p>Second point.</p></li>
    </ul>
  </li>
</ol>
```

### Definition lists

`dl` contains groups of one or more `dt` terms followed by one or more `dd`
definitions. Do not begin a group with a definition or end with an undefined
term. Each `dt` contains phrasing content; each `dd` contains block content.
A paragraph wrapper around a term is accepted and unwrapped, but is unnecessary.

```xml
<dl>
  <dt>Reflow</dt>
  <dt>Reflowable layout</dt>
  <dd><p>Layout that adapts to the available reading width.</p></dd>
  <dt>Measure</dt>
  <dd>
    <p>The width available to a line of text.</p>
    <p>It can change with the viewport.</p>
  </dd>
</dl>
```

## 7. Preformatted text and code

`pre` preserves text spacing and line breaks. Use either plain text directly
inside `pre`, or a single `code` child containing source code. Do not mix code
and surrounding prose within the same `pre`; put explanations in separate
paragraphs.

For source code, the `code` element may carry a `class` token of the form
`language-NAME`, for example `language-rust` or `language-python`. Separate
multiple class tokens with ASCII whitespace. Only the first `language-` token
is considered; use a nonempty language name. A missing language name means
unspecified source code, not plain preformatted prose.

```xml
<pre>aligned     columns
first       second</pre>
<pre><code class="language-rust">fn main() {
    println!(&quot;Hello &amp; goodbye&quot;);
}</code></pre>
```

Text is still XML-escaped or enclosed in CDATA. Nested markup inside a code
block does not define inline formatting; only its text is retained. Place the
opening and closing tags immediately next to the intended text if you do not
want extra initial or final newlines. Language metadata is a highlighting hint,
not a promise that a particular highlighter is available.

`language-mermaid` identifies Mermaid diagram source. It is retained as source
code; it does not request script execution or guarantee a rendered diagram.
Place shared attributes for the block on `pre`; the inner `code` supplies text
and the language class, not an independent anchor or language override.

## 8. Images and figures

`img` is empty and uses these attributes:

| Attribute | Rule |
| --- | --- |
| `src` | Supply the image resource reference. |
| `alt` | Supply meaningful replacement text, or the empty string for an intentionally decorative image. |
| `width`, `height` | Optional intrinsic size hint. Supply both as positive, finite numbers in CSS pixels, without a unit suffix. |

Omitting `alt` is distinct from `alt=""`: it means the alternative is missing,
not that the image is decorative, and produces a diagnostic. A `title` is not a
substitute for `alt`. An invalid or incomplete dimension pair supplies no size
hint. Dimensions are hints, not a fixed rendered size.

Images can appear in phrasing content, including links. For a standalone image,
use a paragraph or `figure`. A `figure` contains block content and may also
contain direct `img` children. It may contain at most one `figcaption`.
`figcaption` contains block content, so use a paragraph for caption prose.
The caption remains in its source position; put it before or after the content
as intended. Do not put bare text directly in a figure.

```xml
<figure id="pipeline">
  <img src="images/pipeline.png"
       alt="Source text passes through parsing, layout, and painting."
       width="640" height="360"/>
  <figcaption><p>The document processing pipeline.</p></figcaption>
</figure>
<p>A decorative separator: <img src="images/flourish.png" alt=""/>.</p>
```

## 9. Tables

A table has this structure, in order:

1. An optional single `caption`, containing block content.
2. One or more row groups: `thead`, `tbody`, or `tfoot`.
3. Each row group contains one or more `tr` rows.
4. Each row contains one or more `th` header cells or `td` data cells.
5. Each cell contains block content. Bare cell text is accepted as an anonymous
   paragraph; explicit paragraphs are recommended for longer or mixed content.

Use `thead` for heading rows, `tbody` for the body, and `tfoot` for footer rows.
Write groups in the desired reading order. Direct `tr` children of `table` are
also accepted: they form an implicit body group after any explicit groups. Do
not mix direct rows and explicit groups when their relative order matters.

Cells accept `colspan` and `rowspan`, each a positive decimal integer defaulting
to `1`. A row span must stay within its row group; `rowspan="0"` does not mean
“all remaining rows.” A column span must not exceed 4096. Invalid spans are
normalized with diagnostics; authors must supply valid spans rather than depend
on repair.

Cells are placed in source order at the next available grid position, skipping
positions occupied by earlier row spans. The column count is derived from that
grid. Supply consistent rows, accounting for all spans. `th` marks a header;
headers in `thead` receive column-header semantics. Explicit `scope` and
`headers` associations, `col`/`colgroup`, column width attributes, and CSS table
layout controls have no defined effect in this format.

```xml
<table id="measurements">
  <caption><p>Measurements by sample</p></caption>
  <thead>
    <tr><th>Sample</th><th>Reading</th><th>Unit</th></tr>
  </thead>
  <tbody>
    <tr><th rowspan="2">A</th><td>12</td><td>mm</td></tr>
    <tr><td>13</td><td>mm</td></tr>
    <tr><th>B</th><td colspan="2">Not measured</td></tr>
  </tbody>
</table>
```

Table parts must be inside their corresponding table structures. Orphan rows
and cells can be recovered as readable content with diagnostics, but are not
valid authoring structures.

## 10. Mathematics

Mathematics is encoded as TeX source in a `data-tex` attribute on a `span`:

- Without the `katex-display` class token, the expression is inline mathematics
  and belongs in phrasing content.
- With the `katex-display` class token, the expression is display mathematics
  and belongs in block content.

`data-tex` is authoritative. Children of the mathematical element are not
additional readable content; generated presentation subtrees, if present, are
ignored. Authors do not need to generate such subtrees. The `katex` class alone
does not identify mathematics; `data-tex` is required. The class names are format
conventions, not a requirement to load KaTeX or execute JavaScript.

```xml
<p>The identity <span data-tex="e^{i\pi}+1=0"/> is well known.</p>
<span class="katex-display" data-tex="\frac{1}{2} + \frac{1}{2} = 1"/>
```

Use literal TeX backslashes in XML; they need no XML escaping. Escape XML-sensitive
characters in the attribute: for example, write `&lt;` for a less-than sign and
`&amp;` for a TeX alignment ampersand. TeX source is retained semantically;
particular mathematical layout or rendering capabilities are not guaranteed.
Native MathML and dollar-delimited text are not alternative math encodings in
this vocabulary.

## 11. Disclosures

`details` contains an optional single `summary` followed by block content.
`summary` contains phrasing content. A paragraph wrapper within `summary` is
accepted and unwrapped. Put the summary first and do not include more than one.
The boolean `open` attribute records whether the disclosure is initially open;
omitting it records initially closed state. Interactive behavior is a presenter
capability, not a requirement to hide the body in every reading environment.

```xml
<details open="">
  <summary>How were the measurements taken?</summary>
  <p>Each sample was measured twice.</p>
</details>
```

## 12. Media objects and readable alternatives

`iframe`, `audio`, and `video` are block media objects, not executable embedded
content. They describe an object that may require a host presenter and should
include a readable fallback.

They accept:

- `src`: the primary resource or activation URI.
- `title`: a descriptive label, also used as fallback text when there is no
  readable block fallback.
- `width` and `height`: an optional paired size hint with the same numeric rules
  as images.

`audio` and `video` may instead contain empty `source` elements with `src`.
The direct `src` takes precedence; otherwise the first child `source` with a
`src` supplies the activation reference. Alternate source selection and MIME
type negotiation are not specified.

Write fallback content as paragraphs or other block elements inside the media
object, after any `source` elements. Include an ordinary link when readers
should be able to open the resource without native playback. Do not rely on
bare fallback text or direct inline children being retained.

```xml
<audio title="Interview">
  <source src="media/interview.mp3"/>
  <p><a href="media/interview.mp3">Listen to the interview</a>, or read
  <a href="transcript.xhtml">the transcript</a>.</p>
</audio>
```

`track` elements may occur as empty media children, but their metadata is not
retained as readable content. `poster`, `controls`, `autoplay`, `preload`, MIME
`type`, and iframe browser attributes do not specify engine behavior. Nothing
in the format requests automatic playback, frame execution, or network access.

## 13. Unsupported content and authoring constraints

Use the vocabulary above rather than arbitrary HTML. Unknown inline wrappers
may retain their text, but do not acquire additional semantics. In particular,
HTML spellings such as `b`, `i`, `s`, `del`, `ruby`, form controls, and custom
elements are not substitutes for defined semantic roles.

Do not include `script`, `style`, forms, executable handlers, SVG, or other
foreign content. The format defines no scripting or stylesheet execution, but
unsupported markup is not a sanitization mechanism: text inside an unknown
element can still become readable text. Remove unwanted content before authoring
the document.

Well-formed XML is necessary but not sufficient. Specialized structures must
also obey their child rules: lists need items, definition lists need complete
term/definition groups, tables need rows and cells, and links must not nest.
Malformed XML is rejected rather than repaired as HTML. Recovery of some
structural mistakes does not make those structures conforming documents.

## 14. Format version

The current version is **1**. The format version is supplied alongside the
document by its enclosing protocol or storage system; no root attribute or
in-document version marker is required. Unversioned input is treated as version
**0**, which currently accepts the same vocabulary and rules. Versions greater
than 1 are unsupported.

Versioning belongs to the document interchange contract, not to CSS classes,
resource paths, XML declarations, or the XML namespace URI.
