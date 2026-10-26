# Changelog

Notable changes to this crate, for people who use it, from 0.10.0 on. Earlier
releases are recorded only in the git tags and history.

## Unreleased

### Fixed

- `looks_like_cpp`, and so `language_for_header_content`, no longer reads
  three valid C shapes as C++: a pointer or variable named `new` (as in the
  Linux `list.h` helpers), a parameter or variable named `delete`, and the
  expression statement `a & b;`. The C++ grammar's error recovery turned them
  into `new`/`delete` expressions and reference declarators. A `new` or
  `delete` expression now counts only when it parses cleanly and the file
  never uses `new` or `delete` as a name; a macro named `new`
  (`#define new DEBUG_NEW`) is not such a name, and a deleted function
  (`void f() = delete;`) always counts. A reference declarator counts only
  when it parses cleanly and is not a bare `a & b;`, so an uninitialized
  reference without `extern` (`T& r;`), which C++ rejects, now reads as C.
  The fix also holds inside an `#ifdef __cplusplus extern "C" {` guard, and
  keeps K&R-style C after such a guard on the C path. Real C++ (`new T`,
  `delete[] p`, `T& r = x;`, `void f(T& x);`) still detects. One C++ shape is
  now missed: a reference beside an unexpanded macro
  (`API_EXPORT int& counter();`) leaves a parse error next to it, so a header
  whose only C++ is that shape reads as C.

## 0.13.0 — 2026-10-22

### Changed

- File extensions match case-insensitively. `language_for_file`,
  `language_for_header_content`, `language_info_for_file` (and so
  `sloc_mode_for_file` and `classify`'s extension hint),
  `is_discoverable_file`, `is_source_extension`, `is_parseable_extension` and
  `is_extension_for_language` fold the extension to ASCII lowercase before
  looking it up, so upper- and mixed-case extensions now resolve. Before,
  they matched nothing: source trees built on Windows often arrive as
  `X.C` / `X.H`, and a recursive walk found no files while a file passed by
  name was unsupported. Folding applies to every language, so `.CPP` and
  `.HPP` are C++, `.PY` is Python and `.RS` is Rust.
- `.C` is C and `.H` is a C header, for those Windows supplier trees. GCC's
  convention that `foo.C` is C++ is deliberately not followed; a C++ project
  that names its sources `.C` can parse them with `language_for_key("cpp")`.
  `language_for_header_content` still sniffs a `.H` file for C++ the way it
  does `.h`.
- `PathIgnore` globs are still case-sensitive, so `*.c` does not exclude a
  newly discovered `X.C`. Use a directory pattern, or list both cases.
- Language keys stay case-sensitive (`language_for_key("C")` is `None`), and
  `LanguageInfo::filenames` such as `CMakeLists.txt` are still matched as
  written.

## 0.12.0 — 2026-10-19

### Added

- YAML (`lang-yaml`: `.yaml`, `.yml`), JSON (`lang-json`: `.json`, `.jsonc`),
  Markdown (`lang-markdown`: `.md`, `.markdown`), CMake (`lang-cmake`:
  `CMakeLists.txt`, `.cmake`) and Bash (`lang-bash`: `.sh`, `.bash`), each
  with its grammar re-exported. Based on
  [#1](https://github.com/brandon-arrendondo/lang_parsing_substrate/pull/1)
  by @tvanfossen, which registered the same five grammars, the
  `LanguageInfo::filenames` idea and the shell shebang hints. They are
  grouped under a new `all-data-languages` feature that is **not** part of
  `all-languages` or the defaults, so a consumer that discovers files from
  `languages()` doesn't start picking up every `.json`, `.yml` and
  `README.md`. The Python wheel enables both groups. Cargo unifies features
  across a build, though: if any crate in the same dependency graph turns on
  `all-data-languages` (or one of its `lang-*` features), every consumer in
  that build gets those languages in `languages()`, and so in
  `is_source_extension` / `is_parseable_extension`. A tool that discovers
  files from them (e.g. `knots -r`) then picks up `.yml`, `.json` and `.md`
  files too. Consumers that are built separately, as all of them are today,
  are unaffected.
- `is_discoverable_file(path)` (also in Python): whether a recursive walk
  should pick up a file — a registered file name (`CMakeLists.txt`) or a
  recursive-discovery extension; explicit-only extensions such as `.h` are
  not discoverable.
- `markdown` module (`lang-markdown`):
  - `sections(root, source)` splits a document at its document-level ATX and
    setext headings. Each `Section` has `level`, `heading_text` (as written,
    without markers or a closing `#` run), `heading_range`, `body_range` and
    1-indexed inclusive `start_line` / `end_line`. Sections are flat: each
    runs to the next heading of any level. A `#` line in a code fence is never
    a heading, and a heading inside a block quote or list item doesn't start
    a section. A non-blank preamble before the first heading is level 0.
  - `front_matter(root, source)` returns the opening `---` (YAML) or `+++`
    (TOML) block, kept separate from the sections, with its body range.
  - `ContentMap::new(root, source)` records, per row, where content starts
    after container prefixes (`> `, list indentation, and list and quote
    markers). `ContentMap::content_start(row)` returns that byte, and
    `ContentMap::content_lines(node, source)` returns a node's rows without
    the prefixes, so a node's text can be re-parsed with `markdown_inline`.
  - `FrontMatterKind` (`Yaml` / `Toml`) says which front-matter syntax was
    found. `Section`, `FrontMatter` and `FrontMatterKind` are
    `#[non_exhaustive]`, so later additions to them aren't breaking.
  - Python: `markdown_sections(source)` returns `MarkdownSection` objects
    (`level`, `heading_text`, `start_line`, `end_line`, `start_byte`,
    `end_byte`, `heading_end_byte`), and `markdown_front_matter(source)`
    returns a `FrontMatter` (`kind` `"yaml"` / `"toml"`, lines, bytes, body
    bytes) or `None`. Offsets are UTF-8 byte offsets, so slice
    `source.encode()`, not the `str`.
  - Known gap: where the grammar closes a code fence early, on a
    delimiter-like line that CommonMark would not accept, a later heading can
    be missed. Across about 6,000 real-world files this affected about 0.1%
    of headings.
- `classify` takes `sh`, `bash`, `dash` and `ksh` shebangs as a `bash`
  content hint, so an extensionless `bin/foo` starting `#!/bin/sh` is
  recognised. As with other shebangs, the extension still wins: `#!/bin/sh`
  in `x.c` is a `bash` hint that disagrees, and `likely_language()` is C.
- `LanguageInfo::filenames`: exact file names that belong to a language
  whatever their extension. `language_for_file` and `language_info_for_file`
  check it ahead of the extension; `CMakeLists.txt` is the first entry, and
  other `.txt` files stay unknown. `supported_languages_report` lists the
  names, and the Python `LanguageInfo` has a `filenames` attribute.
- `language_for_key("markdown_inline")`: Markdown's inline grammar. The
  `markdown` grammar leaves paragraph and heading text as `inline` nodes;
  re-parse their ranges with it for links, emphasis and code spans. Like
  `tsx`, it is a grammar key with no `LanguageInfo` of its own.
- `SlocMode::Hash` (`#` comments: YAML, CMake, Bash) and `SlocMode::Markdown`
  (single-line `<!-- … -->`), which `regions` and `suppressions` recognise.
  JSON uses `SlocMode::Default`, matching `.jsonc`'s `//` and `/* */`.

### Changed (breaking)

- `LanguageInfo` has a new public field, so code that builds one with a struct
  literal must set `filenames`.
- `SlocMode` has two new variants, so an exhaustive `match` on it needs arms
  for them (knots matches on it).

### Changed

- The Python wheel is built with `all-data-languages`, so its behaviour
  changes on upgrade: `languages()`, `is_source_extension` and
  `is_discoverable_file` now include YAML, JSON, Markdown, CMake and Bash
  (`.yaml`, `.yml`, `.json`, `.jsonc`, `.md`, `.markdown`, `.cmake`,
  `CMakeLists.txt`, `.sh`, `.bash`), and `classify` gives a file starting
  `#!/bin/sh` (or `bash`, `dash`, `ksh`) a `bash` content hint. A Python
  consumer that discovers files from these sees the new files. Rust builds
  with the default features are unchanged.
- The SBOM and third-party licence file attached to the GitHub release now
  describe the wheel's full build, `all-data-languages` included, so they
  list the five new grammars.

## 0.11.2 — 2026-10-08

### Changed

- The copyright holder is BISSELL Homecare, Inc. The licence is still MIT.
- The Python package metadata uses an SPDX licence expression
  (`License-Expression: MIT`) and ships `LICENSE` in the wheel and sdist
  (PEP 639). Building from source needs maturin 1.9.3 or later.
- Each tagged release now publishes, as GitHub Release assets, a CycloneDX
  SBOM (`lang-parsing-substrate-X.Y.Z.cdx.json`) and a third-party licence
  file. Both describe the PyPI wheel's build: the default language set plus
  the Python bindings.

### Fixed

- Documentation: the README's analysis example now compiles, the module and
  API lists add the missing modules and main exports (including which need which
  language features), the README says a C compiler is needed, and the Python
  section names every class. Doc comments no longer cite stale tool names or
  wrong consumers, and `cargo doc` builds without warnings.

### Performance

- `flat::flatten`, and so the Python `parse_tree`, is cheaper: node kinds and
  field names are interned by the grammar's numeric id, so a name is hashed
  only the first time its id is seen rather than once per node, and the
  columns are sized from `descendant_count` up front. Output is unchanged.
  The saving grows with tree size, up to about 25% of flatten time on large
  C files.

## 0.11.1 — 2026-10-06

### Added

- Python bindings for two existing modules (no Rust API change):
  - `classify_file(path, max_size=None, prefix_len=DEFAULT_PREFIX_LEN)` and the
    I/O-free `classify(path, prefix, file_size, max_size=None)` return a
    `FileClass`. `kind` is `empty`, `oversize`, `binary` or `source_text`, and
    the fields that kind defines are set: `size`/`limit`; `binary_kind`/`mime`;
    `encoding`, `utf8_bom`, `language_by_extension`, `language_by_content`. Also
    `is_source_text` and `likely_language`. `classify_file` raises `OSError` for
    a missing or non-regular file. `DEFAULT_PREFIX_LEN` is exported.
  - `PathIgnore(patterns)` with `is_ignored(path)` (the `toolchain.toml`
    `[ignore].paths` matcher). An invalid glob raises `ValueError`.
- `query`: traversal primitives from aurora-lint's performance work.
  - `walk_preorder(root, visit)` walks a subtree pre-order with ONE `TreeCursor`;
    `visit` returns `Walk::{Continue, SkipChildren, Stop}`.
  - `ancestors(root, node)` returns a node's ancestor chain from one root-down
    descent (`child_with_descendant`). `find_ancestor_from_root`,
    `nearest_ancestor_of_kind_from_root` and `nearest_ancestor_of_kinds_from_root`
    build on it at O(depth). `find_ancestor` and friends climb with `parent()`,
    which tree-sitter answers by re-descending from the root, so they cost
    O(depth²); they are kept for callers that hold only the node, with the cost
    documented.
  - `child_nodes(node)` / `named_child_nodes(node)`: linear child lists from one
    cursor. `Node::child(i)` walks from the first child, so an index loop over a
    node's children is O(k²).

### Changed

- Every subtree walk (`find_descendants`, `find_first_descendant`, the call-graph
  walks in `calls`, the structural hash in `fingerprint`, `looks_like_cpp`,
  `detect_min_c_standard`) now runs on `walk_preorder`. The old walks created a
  cursor and a `Vec` per visited node (about 5% of CPU on sqlite's `btree.c` in
  aurora-lint). Order and results are unchanged. Measured through the Python
  bindings (best of 5, parse included): `call_edges` on `btree.c` 160 to 137 ms,
  `function_fingerprints` 110 to 98 ms, and on a 3,000-deep else-if chain 104 to
  86 ms and 66 to 57 ms.
- A source-hygiene test (`tests/source_hygiene.rs`) fails on a
  `for i in 0..n.child_count()` loop that indexes `child(i)` in `src/`.

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
