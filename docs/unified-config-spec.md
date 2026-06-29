# Unified Toolchain Config — Design Spec

Covers `toolchain.toml`, per-tool config files, the shared suppress file, and inline
suppression comment syntax.  This is the design basis for tasks 8, 10, 11, 12.

---

## File layout

```
project/
  toolchain.toml      # shared: ignores + language defaults (substrate-level)
  knots.toml          # knots thresholds + filter rules
  funky.toml          # funky formatting per language (already exists; gains lang sections)
  sqc.toml            # sqc manifest ref + rule overrides
  suppress.toml       # valgrind-style suppress entries for all tools
```

Each tool loads its own config file plus `toolchain.toml` for the substrate-level shared
settings.  A project that only uses knots never needs `sqc.toml`.  Tools pass the
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

Built-in threshold defaults (used when neither config nor CLI flag is present):

| Metric    | Default | Rationale |
|-----------|---------|-----------|
| mccabe    | 10      | PEP8/pylint recommendation; widely adopted |
| cognitive | 15      | Sonar default |
| nesting   | 5       | Common industry guideline |

---

## `funky.toml` — per-language formatting config

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

Config dispatch in funky:

```
file → substrate::language_for_file() → "python"
     → load funky.toml [python.*]
     → merge over built-in Python defaults
     → format
```

---

## `sqc.toml` — sqc-specific config

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
tool          = "sqc"
rule          = "INT30-C"
file          = "src/legacy.c"
hash          = "abc123def456789a"       # sqc only: SHA-256(rule+":"+normalised_code)[..16]
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
| `tool`        | yes      | `"knots"`, `"sqc"`, `"funky"`, or `"*"` |
| `rule`        | no       | Exact rule/metric ID; omit to suppress all rules for the tool |
| `file`        | no*      | Exact relative path |
| `file_glob`   | no*      | Glob pattern |
| `hash`        | sqc only | Truncated SHA-256 of normalised code; required for sqc inline suppressions |
| `justification` | no     | Free text; strongly encouraged |

\* At least one of `file` / `file_glob` must be present.  When both `rule` and file
fields are specified, both must match (AND semantics).

---

## Inline suppression comment syntax

The `tools:suppress TOOL:RULE` shape is identical across all languages.  Only the
comment character varies.  Block regions use `tools:off` / `tools:on`.

### Single-line (suppresses next non-blank statement; or enclosing function for knots metrics)

```c
// tools:suppress sqc:INT30-C HASH:abc123def456789a JUSTIFICATION:"validated"
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

### Block region (format pass-through for funky; can scope other tools too)

```c
/* tools:off funky */
int m[] = {1,0,
           0,1};
/* tools:on */

/* tools:off */          /* no tool qualifier = all tools ignore this region */
...
/* tools:on */
```

### Syntax rules

- `TOOL:RULE` — tool name matches config file key (`knots`, `sqc`, `funky`); rule is a
  metric name or rule ID within that tool.
- `HASH:` field is required for sqc (tamper detection preserved); omit for other tools.
- `JUSTIFICATION:` is optional but strongly encouraged.
- Block form: `tools:off [TOOL[,TOOL,...]]`; no qualifier suppresses all tools.
- Legacy `// SQC-SUPPRESS:` and `/* funky:off */` continue to parse during a
  deprecation window (implementation detail for tasks 8, 11, 12).

---

## Config resolution order

For a given tool invocation:

1. Locate `toolchain.toml` — walk up from the target path until found (or repo root).
2. Load the tool's own config file from the same directory as `toolchain.toml`.
3. Load `suppress.toml` from the same directory.
4. CLI flags override config values (knots thresholds, sqc `--rules`, etc.).
5. Per-language sections in the tool config override global sections in the tool config,
   which override `toolchain.toml` language defaults, which override built-in defaults.

---

## Migration path

| Current surface | Target | Task |
|----------------|--------|------|
| knots JSON filter files | `[[knots.filter.*]]` in `knots.toml` | 10 |
| funky `[ignore].patterns` in `funky.toml` | `[ignore]` in `toolchain.toml` + `[funky.ignore]` | 11 |
| sqc `--exclude` CLI glob | `[sqc.ignore]` in `sqc.toml` | 12 |
| knots inline suppress (none) | `tools:suppress knots:METRIC` | 8 |
| `/* funky:off */` | `/* tools:off funky */` (legacy form kept during deprecation) | 8, 11 |
| `// SQC-SUPPRESS:` | `// tools:suppress sqc:RULE HASH:...` (legacy form kept) | 8, 12 |
| `.sqc-suppress.toml` | `suppress.toml` | 12 |
