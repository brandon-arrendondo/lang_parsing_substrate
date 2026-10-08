# Unified Toolchain Config — Design Spec

**Status:** Partly implemented; the substrate's `suppressions`, `regions` and `path_ignore`
modules cite this page as their spec.

- **Implemented:** `toolchain.toml` `[ignore].paths` (read by aurora-lint and moldy); `knots.toml`
  thresholds and `[[filter.exclude]]` (read by knots); `suppress.toml` (read by aurora-lint, which
  falls back to its older suppress-file names); inline `tools:suppress TOOL:RULE` (parsed by the
  substrate, honoured by aurora-lint and knots); and `tools:off` / `tools:on` regions (parsed by the
  substrate, honoured by knots).
- **Still proposed:** `toolchain.toml` language defaults, per-language sections in `moldy.toml`,
  `aurora-lint.toml`, loading each tool's config from beside `toolchain.toml`, and a `funky:off`
  deprecation window in moldy.

It was written when the formatter was funky (succeeded by moldy) and aurora-lint was called sqc;
the names below are the current ones.

Covers `toolchain.toml`, per-tool config files, the shared suppress file, and inline
suppression comment syntax.

---

## File layout

```
project/
  toolchain.toml      # shared: ignores + language defaults (substrate-level)
  knots.toml          # knots thresholds + filter rules
  moldy.toml          # moldy formatting per language (already exists; gains lang sections)
  aurora-lint.toml    # aurora-lint manifest ref + rule overrides
  suppress.toml       # valgrind-style suppress entries for all tools
```

Each tool loads its own config file plus `toolchain.toml` for the substrate-level shared
settings.  A project that only uses knots never needs `aurora-lint.toml`.  Tools pass the
relevant config slices down to `lang_parsing_substrate`.

---

## `toolchain.toml` — substrate-level shared config

```toml
[ignore]
paths = [
    "vendor/**",
    "third_party/**",
    "generated/**",
]

# Per-language defaults any tool can read from the substrate.
# Each tool's own config can override these for its own purposes.
# Omit a language to accept each tool's built-in defaults.

[language.c]
indent = { style = "spaces", width = 4 }

[language.python]
line_length = 88    # project-wide override of PEP8's 79
```

---

## `knots.toml` — knots-specific config

Modelled after `.yamllint`: all fields optional, built-in defaults apply when absent,
CLI flags override config values.

```toml
[thresholds]          # global defaults; per-language sections override
mccabe    = 10
cognitive = 15
nesting   = 5

[c.thresholds]
mccabe = 15           # C idioms inflate cyclomatic complexity vs higher-level languages

[[filter.exclude]]
file_patterns     = ["tests/**"]    # glob
function_patterns = ["^test_"]      # regex
```

knots enables no threshold by default: a threshold applies only when the config or a CLI flag
sets one. Suggested starting values:

| Metric    | Suggested | Rationale |
|-----------|---------|-----------|
| mccabe    | 10      | PEP8/pylint recommendation; widely adopted |
| cognitive | 15      | Sonar default |
| nesting   | 5       | Common industry guideline |

---

## `moldy.toml` — per-language formatting config

Language sections use the canonical names returned by `substrate::language_for_file()`.
Built-in safe defaults are baked in per language; only write what diverges.

| Language | Built-in default basis |
|----------|----------------------|
| `c`, `cpp` | LLVM style |
| `python`   | PEP8 (line_length=79, indent=4 spaces) |
| `rust`     | rustfmt defaults |
| `go`       | gofmt defaults |
| others     | language community standard where one exists |

```toml
[ignore]
paths = []

[c.indent]
style = "spaces"
width = 4
[c.braces]
style = "allman"

[python]
line_length = 88    # project override; PEP8 default 79 is the built-in

# [rust], [go], [cpp] — omit to accept built-in defaults
```

Config dispatch in moldy:

```
file → substrate::language_for_file() → "python"
     → load moldy.toml [python.*]
     → merge over built-in Python defaults
     → format
```

---

## `aurora-lint.toml` — aurora-lint-specific config

```toml
[manifest]
path = "rules_templates/rules-all.toml"   # default; overridable

# Per-rule overrides without editing the manifest
[rules.INT30-C]
enabled = false
```

---

## `suppress.toml` — valgrind-style suppress file

One file, all tools.  Each entry is a named suppression; the `tool` field scopes it.

```toml
[[suppress]]
name          = "legacy-int-arithmetic"
tool          = "aurora-lint"
rule          = "INT30-C"
file          = "src/legacy.c"
hash          = "abc123def456789a"       # aurora-lint only: SHA-256(rule+":"+normalised_code)[..16]
justification = "Validated by security team — JIRA-456"

[[suppress]]
name          = "legacy-complexity"
tool          = "knots"
rule          = "cognitive"
file_glob     = "src/legacy/**"
justification = "Legacy module — JIRA-789"

[[suppress]]
name          = "third-party"
tool          = "*"                      # wildcard: all tools skip this subtree
file_glob     = "third_party/**"
justification = "Third-party code"
```

### Suppression entry fields

| Field         | Required | Description |
|---------------|----------|-------------|
| `name`        | yes      | Human-readable label (unique within file) |
| `tool`        | yes      | `"knots"`, `"aurora-lint"`, `"moldy"`, or `"*"` |
| `rule`        | no       | Exact rule/metric ID; omit to suppress all rules for the tool |
| `file`        | no*      | Exact relative path |
| `file_glob`   | no*      | Glob pattern |
| `hash`        | aurora-lint only | Truncated SHA-256 of normalised code; required for aurora-lint inline suppressions |
| `justification` | no     | Free text; strongly encouraged |

\* At least one of `file` / `file_glob` must be present.  When both `rule` and file
fields are specified, both must match (AND semantics).

---

## Inline suppression comment syntax

The `tools:suppress TOOL:RULE` shape is identical across all languages.  Only the
comment character varies.  Block regions use `tools:off` / `tools:on`.

### Single-line (suppresses next non-blank statement; or enclosing function for knots metrics)

```c
// tools:suppress aurora-lint:INT30-C HASH:abc123def456789a JUSTIFICATION:"validated"
uint32_t x = y + z;

// tools:suppress knots:cognitive JUSTIFICATION:"legacy, JIRA-123"
void big_function() { ... }
```

```python
# tools:suppress knots:cognitive JUSTIFICATION:"legacy"
def big_function():
    ...
```

```rust
// tools:suppress knots:cognitive JUSTIFICATION:"legacy"
fn big_function() { ... }
```

### Block region (format pass-through for moldy; can scope other tools too)

```c
/* tools:off moldy */
int m[] = {1,0,
           0,1};
/* tools:on */

/* tools:off */          /* no tool qualifier = all tools ignore this region */
...
/* tools:on */
```

### Syntax rules

- `TOOL:RULE` — tool name matches config file key (`knots`, `aurora-lint`, `moldy`); rule is a
  metric name or rule ID within that tool.
- `HASH:` field is required for aurora-lint (tamper detection preserved); omit for other tools.
- `JUSTIFICATION:` is optional but strongly encouraged.
- Block form: `tools:off [TOOL[,TOOL,...]]`; no qualifier suppresses all tools.
- Legacy `// AURORA-SUPPRESS:` (and its older `SQC-SUPPRESS` spelling) still parses in
  aurora-lint. *Proposed:* moldy would likewise accept funky's `/* funky:off */` during a
  deprecation window; it does not today.

---

## Config resolution order

For a given tool invocation:

1. Locate `toolchain.toml` — walk up from the target path until found. (aurora-lint and moldy do
   this today, up to the filesystem root.)
2. *Proposed:* load the tool's own config file from the same directory as `toolchain.toml`.
   (knots today finds `knots.toml` on its own, walking up from the current working directory;
   this step would change that.)
3. *Proposed:* load `suppress.toml` from the same directory. (aurora-lint today reads it from the
   scan root.)
4. CLI flags override config values (knots thresholds, aurora-lint `--rules`, etc.).
5. Per-language sections in the tool config override global sections in the tool config,
   which override `toolchain.toml` language defaults, which override built-in defaults.

---

## Migration path

| Surface | Target | State |
|---------|--------|-------|
| knots `--include` / `--exclude` JSON filter files | `[[filter.exclude]]` in `knots.toml` | Done; the JSON files still work |
| knots inline suppress | `tools:suppress knots:METRIC`, `tools:off` | Done |
| aurora-lint `// AURORA-SUPPRESS:` | `// tools:suppress aurora-lint:RULE HASH:...` | Done; the old form still parses |
| aurora-lint `.aurora-lint-suppress.toml` / `.sqc-suppress.toml` | `suppress.toml` | Done; the old names are fallbacks |
| moldy `[ignore].patterns` in `moldy.toml` | `[ignore]` in `toolchain.toml` + `[moldy.ignore]` | `toolchain.toml` `[ignore].paths` done; `[moldy.ignore]` proposed |
| aurora-lint `--exclude-all` / `--report-exclude` globs | `[aurora-lint.ignore]` in `aurora-lint.toml` | Proposed |
| funky's `/* funky:off */` | `/* tools:off moldy */` | Proposed |
