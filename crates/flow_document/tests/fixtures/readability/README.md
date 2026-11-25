# Pinned Readability fixtures

Upstream: <https://github.com/mozilla/readability>
Revision: `ab4027a8b37669745016869a37a504727992b2ba`
Paths: `test/test-pages/<case>/{source.html,expected.html,expected-metadata.json}`

The eight checked-in cases are synthetic Lorem-ipsum tests, copied unchanged.
The upstream Apache-2.0 license and NOTICE are included. Captured third-party
articles are **not** vendored: the library license alone does not establish
redistribution rights for every captured publisher page. Review those separately
before adding real article fixtures.

`web_html_corpus.rs` compares semantic text (including image alternatives),
heading counts, link destinations, image resources, title, and byline, not HTML
serialization. Thematic breaks and unavailable replaced media contribute canonical
object markers, not empty strings. Fragment-only expected URLs are resolved using
the source HTML base and percent-decoded for exact anchor matching, intentionally
correcting the upstream base-fragment behavior. The default
suite needs no network, browser, or resource acquisition.

## Complete external corpus

Use a separate checkout at the exact revision above:

```sh
git clone https://github.com/mozilla/readability.git /tmp/readability
git -C /tmp/readability checkout ab4027a8b37669745016869a37a504727992b2ba
FLOW_READABILITY_CORPUS=/tmp/readability/test/test-pages \
  cargo test -p flow_document --features web-html --test web_html_corpus \
  external_readability_corpus_report -- --ignored --nocapture
```

Set `FLOW_READABILITY_CORPUS_VERBOSE=1` to include missing/additional resource
and destination sets in the report.

The report compares each dimension against an unmodified native extractor too:
`pipeline`, `native extraction`, or `native extraction + pipeline`. These labels
identify where to investigate, not whether the difference is necessarily a bug.
For example, canonical math source vs rendered text, semantic math vs fallback
image resources, unsupported-media labels, image alternatives, title/site-suffix
normalization, and removal of duplicate title headings can legitimately differ.
Both the comparison projection and the particular transformation need review
before accepting a discrepancy. The report fails on IR-construction errors;
semantic differences remain visible investigation results, not silently blessed
snapshots or a claim of importer completeness.

The full corpus has 130 cases. It exercises real article extraction locally
without redistributing the original captured pages. Importer results do not
depend on whether a particular desktop/browser host can fetch linked resources.

## Current evaluation

With `dom_smoothie` 0.20.0 and the pinned revision above, all 130 inputs construct
valid IR. 104 match every compared dimension; 26 have reported differences:

| Difference | Cases | Interpretation |
| --- | ---: | --- |
| Native extraction | 15 | Fourteen byline differences and the `mercurial` content/outline difference also occur without our transformations. |
| Title cleanup | 7 | The importer removes a trailing site-name suffix. |
| Responsive fallback | 2 | `data-url-image` and `lazy-image-2` gain resources from srcset-only images. |
| URI policy | 1 | `hukumusume` contains rejected local-file spacer images. |
| Math representation | 2 | `mathjax` and `wikipedia-3` retain semantic math rather than the reference's rendered text/images; repaired math also affects extraction scoring in the latter. |

`iab-1` appears in both the native-byline and title groups. These classifications
explain the current report; they are not exception lists in the tests or proof
of complete importer fidelity. Literal preformatted whitespace, links, anchors,
and other semantics not covered by the coarse corpus projection have separate
regression tests.

Known follow-ups are responsive-alternative IR support, better fallback image
selection, foreign-content fallbacks (notably inline SVG), and additional
site-specific note conventions. They remain separate from resource acquisition
and the hardening boundary below.

## Hardening boundary

Input is capped at 2 MiB. The HTML5 tree sink rejects operations before exceeding
50,000 arena nodes, depth 128 (including detached template fragments), 4,096
attributes per element, 16 MiB of accounted DOM storage, or 8 million sink
operations. Accounted bytes include node/attribute structural charges and payload;
they are not exact allocator-capacity measurements. Reparenting checks the whole
moved subtree, and failure stops DOM mutation without panic-based cancellation.
Tokenizer feeds are at most 4 KiB and cumulative validation visits are bounded.

Math carriers and retained-note snapshots have independent 2 MiB budgets. Notes
are copied from bounded parsed-node snapshots, never reparsed by an unbudgeted
fragment parser; capacity/depth checks precede restoration. Explicitly hidden note
children remain hidden. Visibility checks handle inline declarations, not a full
stylesheet cascade or computed browser layout.

Global allocator accounting (including tokenizer scratch and vector capacity)
and interruption of native extraction mid-call remain hardening work. These are
importer boundaries, independent of desktop/browser resource acquisition.
