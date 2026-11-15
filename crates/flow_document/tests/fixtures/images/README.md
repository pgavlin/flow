# Synthetic test image

`landscape.png` is an original geometric landscape generated for flow's tests,
not a photograph or a downloaded image. Its 960 × 480 dimensions exercise local
host-supplied image decoding and aspect-preserving scaling. Both the image and generator are
covered by the repository's MIT license.

Regenerate it with Python's standard library:

```sh
python3 crates/flow_document/tests/fixtures/images/generate_landscape.py
```
