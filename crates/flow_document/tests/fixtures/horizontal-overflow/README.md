# Horizontal overflow fixtures

Open these files in `examples/reader` at a narrow width (e.g. 360 logical pixels).
Both contain enough rows/lines to span multiple pages.

- `wide-code.md`: authored long lines, far-right content, and ordinary inline code.
- `wide-table.md`: unbreakable identifiers force GRIDMIN beyond the reading width;
  far-right links exercise transformed hit testing and repeated headers.
- `nested-code.xhtml`: a preformatted block and nested table use only the outer
  table's viewport.

Check continuous and paginated modes, Tab/Shift+Tab and Escape, horizontal wheel
and touch pans, edge limits (never page turns), shared offsets across pages,
selection/search clipping, and resize clamping. These fixtures are also consumed
by the viewer regression tests.
