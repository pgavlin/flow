# Document test fixtures

## MediaWiki DOM capture

`mediawiki/` contains a pinned real Parsoid 2.8.0 article revision. See its
[README](mediawiki/README.md) for the source, revision, checksum, attribution,
license, and supported Cite/Math markup contracts. No setup or network access is
required; the development-only capture is excluded from the published crate.

```sh
cargo test -p flow_document --no-default-features --features mediawiki-html
```

This runs adapter contract tests, XML importer fingerprints, and the pinned
article test. `--all-features` also exercises rendering and semantic-cache
round trips.

## Readability corpus

`readability/` is an unmodified Git submodule of
<https://github.com/mozilla/readability>, pinned to
[`ab4027a8b37669745016869a37a504727992b2ba`](https://github.com/mozilla/readability/commit/ab4027a8b37669745016869a37a504727992b2ba).
The submodule URL in the repository's `.gitmodules` and its recorded gitlink
commit are the provenance and version pin. Upstream documentation, LICENSE, and
NOTICE remain in the submodule. There are no locally copied corpus files or
separate licensing/provenance manifests.

The 130 cases are under `readability/test/test-pages/`. Each case supplies
`source.html`, `expected.html`, and `expected-metadata.json`.

### Setup and execution

From the repository root:

```sh
git submodule update --init --recursive
cargo test -p flow_document --features web-html --test web_html_corpus
```

Or clone the repository with `git clone --recurse-submodules`. Ordinary
`--all-features` test runs include this suite, and CI initializes the submodule
before running it. Tests fail with a setup instruction if the corpus is absent;
they never fetch it automatically. The corpus is excluded from the published
`flow_document` crate archive.

The suite checks:

- All 130 pinned inputs must construct valid IR.
- Eight selected synthetic cases must match the reference semantic projection.
- The full corpus produces a separate semantic difference report. A difference
  from Mozilla's output is not automatically an import failure.

To see the report:

```sh
cargo test -p flow_document --features web-html --test web_html_corpus \
  readability_corpus_imports -- --nocapture
```

Set `FLOW_READABILITY_CORPUS_VERBOSE=1` to include missing/additional resource and
link-destination sets. No external checkout environment variable is required.

### Comparison and current results

The projection compares semantic text (including image alternatives and object
markers), headings, link destinations, resources, title, and byline—not serialized
HTML. Fragment references use the source HTML base and percent-decoded anchor
names. Literal preformatted whitespace, table associations, ruby, and other
semantics have additional targeted importer regressions.

With `dom_smoothie` 0.20.0 and the pinned corpus, all 130 cases import. 104 match
all compared dimensions and 26 have reported differences:

| Difference | Cases | Interpretation |
| --- | ---: | --- |
| Native extraction | 15 | Fourteen byline differences and the `mercurial` content/outline difference also occur without our transformations. |
| Title cleanup | 7 | The importer removes trailing site-name suffixes. |
| Responsive fallback | 2 | `data-url-image` and `lazy-image-2` gain resources from srcset-only images. |
| URI policy | 1 | `hukumusume` contains rejected local-file spacer images. |
| Math representation | 2 | `mathjax` and `wikipedia-3` retain semantic math rather than the reference's rendered text/images; math repairs also affect extraction scoring in the latter. |

`iab-1` appears in both the native-byline and title groups. These classifications
are not test exception lists or proof of complete importer fidelity. Importer
tests are independent of whether a desktop/browser host can fetch linked resources.

### Updating the pin

Update the submodule to a deliberate upstream commit, rerun the corpus suite,
review semantic changes, and commit the new gitlink together with any test and
documentation updates. Do not use `git submodule update --remote` in test setup:
normal setup must check out the commit recorded by the parent repository.
