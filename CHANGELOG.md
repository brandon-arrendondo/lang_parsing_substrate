# Changelog

All notable changes to this crate are documented here.

## 0.11.0 — 2026-10-06

### Added

- `flat` module: `flatten(&Tree) -> FlatTree` copies every node of a tree into
  parallel `u32` columns: kind and field (indexing `kinds` / `fields` string
  tables, field 0 = none), flags (named, error, missing, extra, has_error),
  parent / first_child / next_sibling / prev_sibling links (`NONE` when
  absent), byte offsets, (row, column) points, and a `child_offset` /
  `child_list` index giving each node's children as one slice. Rows are
  pre-order with the root at 0. The walk is iterative.
- Python binding `parse_tree(language_key, source: bytes) -> FlatTree`, with
  each column returned as `bytes` for `memoryview(...).cast("I")`, and
  `lang_parsing_substrate.NONE`. This lets a Python consumer walk the full tree
  without its own tree-sitter grammar packages. clew uses it to parse Python
  and Rust.
- `FlatTree.root_node` returns a native `Node` that answers the walking subset of
  py-tree-sitter's `Node` API: `type`, `children`, `named_children`,
  `child_by_field_name`, `children_by_field_name`, `parent`, the (named) sibling
  links, `start_byte` / `end_byte`, `start_point` / `end_point`, `text`, `id`,
  `child_count`, `named_child_count`, and the `is_named` / `is_error` /
  `is_missing` / `is_extra` / `has_error` flags. Existing py-tree-sitter walkers
  run on it unchanged. A Python wrapper over the columns measured 1.7-2.3x
  slower than py-tree-sitter on clew's harvest. The native node brings a whole
  clew build to within about 3% of py-tree-sitter, with byte-identical output.
  `FlatTree.source` returns the parsed bytes.
- `tsquery` module: `run_query(&Language, Node, &[u8], &str)` returns owned
  `Capture`s (pattern index, capture name, kind, byte range, start/end
  points), applying the standard predicates. `tags_query(key)` returns the
  grammar's bundled tags query for c, cpp, python, rust and javascript, and for
  `typescript` / `tsx` the JavaScript query followed by TypeScript's, since
  TypeScript's alone has no functions or classes. tree-sitter-tags directives
  (`#strip!`, `#select-adjacent!`) are not applied.
- Python bindings `query(language_key, source, query) -> list[Capture]` (raises
  `ValueError` when the query does not compile) and `tags_query(language_key)`.

### Fixed

- `"tsx"`: `import_sources` returned nothing for it (only `javascript` and
  `typescript` were matched), and `suppressions` / `ignored_regions` raised
  `ValueError`, because their comment style is looked up by `LanguageInfo`
  key and `.tsx` is filed under `typescript`. Both now treat `tsx` as
  TypeScript.

## 0.10.0 — 2026-10-06

### Changed

- `Cargo.toml` description and the `Suppression::tool` / `hash` doc comments
  now name the current consumers: moldy (replaced funky), aurora-lint
  (formerly sqc / tools_sqc), and clew.

### Added

- `classify` module: cheap, **heuristic** pre-parse file classification so
  consumers can exit early before parsing (e.g. a 2 GB zip named `.c`).
  - `classify(path, prefix, file_size, &ClassifyLimits) -> FileClass` is
    I/O-free. `classify_file(path, &ClassifyLimits)` reads metadata and at most
    `prefix_len` bytes (default `DEFAULT_PREFIX_LEN` = 8 KiB). It rejects
    non-regular files.
  - `FileClass::{Empty, Binary { kind, mime }, Oversize { size, limit },
    SourceText(SourceText)}`. `BinaryKind` covers archive, compressed, ELF, PE,
    Mach-O, image, PDF, document, media, font and unknown. `TextEncoding`
    covers UTF-8, UTF-8 with BOM, UTF-16 LE/BE (BOM only) and Latin-1 (legacy
    8-bit).
  - Signals: the magic-number table of the `infer` crate (new dependency,
    `default-features = false`, no transitive deps), plus the ratios of NUL
    bytes, control bytes and invalid UTF-8 bytes. Text formats `infer` knows
    (HTML, XML, `#!`, PEM, RTF, PostScript) are ignored. Weak ASCII-like
    signatures only count when the bytes also look binary.
  - Language hints: `SourceText::by_extension` (registry) and `by_content`
    (shebang / `<?php`), with `likely_language()` and `extension_agrees()`.
    These are hints only and never gate the result.
  - Thresholds need an absolute floor as well as a ratio (at least 8
    NUL/control bytes), so stray bytes in a small source file never make it
    `Binary`. They were calibrated on about 201K files (aurora-lint benchmark
    corpora, Juliet, fixtures) with no C-family file coming back `Binary`.
    See `docs/classify-calibration.md` and `examples/classify_calibrate.rs`.
  - `SourceText::utf8_bom` records a UTF-8 BOM even when the rest of the file
    is not UTF-8.
  - `classify_file` checks the file type before opening, so a FIFO returns
    `InvalidInput` instead of blocking.
  - `infer` is pinned to `~0.22`, since the code matches on its extension
    strings.
  - Known limitations are listed in the `classify` module docs. Among them:
    the decision uses the prefix only, source with many raw NUL bytes comes
    back `Binary`, BOM-less UTF-16 and all UTF-32 come back as `Binary`, and
    without `infer`'s `std` feature every OLE2 file reports as `Document`.
  - No policy is built in. The caller sets the size limit (default: none) and
    decides what to do with `Binary` and `Oversize`.
