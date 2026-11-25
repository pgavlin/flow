# MediaWiki DOM fixtures

Tests are offline: they never refresh this capture or fetch linked resources.
This development-only fixture is excluded from the published Rust crate.

## Apple

`apple-1386470530.html` is unmodified Parsoid HTML captured from
<https://en.wikipedia.org/api/rest_v1/page/html/Apple>, declaring DOM 2.8.0 and
revision **1386470530**. Attribution and article history:
<https://en.wikipedia.org/w/index.php?title=Apple&oldid=1386470530>.
SHA-256: `6264029d6f368f868ee6dc681da8eb02ad51f05d70df6fd75e7b39bd9545b88f`.

Copyright the Wikipedia contributors, distributed under
[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/). As below,
referenced media is not downloaded or redistributed. This development-only
capture reproduces a pagination assertion caused by interleaved table-cell text
ranges, and exercises rendering at multiple viewport sizes. All 42 image nodes
have dimensions, so their metadata need not be eagerly fetched before reading.

## Pythagorean theorem

`pythagorean-theorem-1386629982.html` is unmodified Parsoid output captured from
<https://en.wikipedia.org/api/rest_v1/page/html/Pythagorean_theorem>.
The document pins its identity to revision **1386629982** and declares DOM
version **2.8.0** in both version properties. Capture SHA-256:
`43133c4917405e6d0f95378c85a1672a91d67ef4abece6650e9eb8af51f16be6`. Its immutable article attribution
and revision history are available at
<https://en.wikipedia.org/w/index.php?title=Pythagorean_theorem&oldid=1386629982>.

Copyright the Wikipedia contributors. Article text is distributed under
[CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/); see the article's
history for contributors. Embedded media may have separate licenses; only HTML
references are stored here, and tests do not download media. This fixture's
license is separate from the Rust implementation's license.

This is a real-output contract fixture, not an assertion of support for all wiki
extensions. The serving deployment's Cite/Math software versions are not exposed
in the capture; support is restricted to these observed shapes:

- Cite: `typeof="mw:Extension/ref"` (possibly alongside `mw:Transclusion`), a
  call-site ID on the wrapper, and an enclosed link to a note; note bodies in an
  `mw:Extension/references` list; `mw-cite-backlink` containers with one or more
  links back to individual call-site IDs.
- Math: `typeof="mw:Extension/math"`, with JSON `data-mw.name == "math"` and
  `data-mw.body.extsrc` containing TeX. Block mode comes from `attrs.display`,
  the element's `display`, or `mwe-math-element-block`. Replacing a formula
  consumes its entire rendered/accessibility fallback subtree once.

The core contract is [MediaWiki DOM Spec 2.8.0](https://www.mediawiki.org/wiki/Specs/HTML/2.8.0),
particularly its extension-tag encapsulation contract. The importer interprets
only the documented Math metadata fields; it does not expand `data-mw` HTML,
wikitext, or template parts. Small synthetic regression cases in the Rust tests
exercise incomplete notes, older HTML wrapper Math output, and malformed data.
