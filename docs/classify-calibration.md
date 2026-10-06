# `classify` calibration

Calibration record for the text-vs-binary thresholds in `src/classify.rs`
(substrate task 2168). These are aggregates only. Re-run with:

```text
cargo run --release --example classify_calibrate -- [binary:]ROOT...
```

The run uses the default 8 KiB prefix and no size limit. Git checkouts are
scanned with `git ls-files`, so a pinned tree contributes exactly its pinned
source.

## Gates

1. No C-family file (`.c .h .inc .cpp .cc .cxx .hpp .hxx`) in a text corpus
   is classified `Binary`.
2. Every file in the planted-binary set is classified `Binary`.

## Results (2026-10-06, r720)

| Corpus set | Files | C-family `SourceText` | C-family `Binary` | Other `Binary` |
|---|---:|---:|---:|---:|
| 12 aurora-lint benchmark corpora at their pins (curl, hostap, libcrc, lua, mbedtls, mosquitto, pureftpd, raylib, sel4, sqlite, valkey, ventoy) | 18,327 | 5,953 | **0** | 1,844 |
| Juliet C/C++ test suite | 105,198 | 105,188 | **0** | 2 |
| aurora-lint `tests/fixtures` (incl. deliberately malformed) | 183 | 155 | **0** | 0 |
| 30 other checkouts under `~/toolchain` (reactos, lapack, cp2k, …) | 77,548 | 25,093 | **0** | 3,425 |
| **Total** | **201,256** | **136,389** | **0** | 5,271 |

The 71-codebase shadow set was not available on r720. It is to be run with
the same example where it lives.

Planted binaries, all renamed to `.c` (two to `.rs` / `.py`): ELF executable,
PE executable, `.o`, `.a`, PNG, JPEG, PDF, zip, tar, gzip, xz, zstd, and 64 KiB
of random noise (×2). Result: **14 / 14 `Binary`**.

### Byte statistics of C-family files classified `SourceText`

Maxima per file, over all 136,389 files (the maxima may come from different
files):

| Signal | Files with any | Max count | Max ratio | Threshold |
|---|---:|---:|---:|---|
| NUL | 0 | 0 | 0% | ≥ 8 **and** > 0.1% |
| other C0 control | 1 | 1 | 0.012% | ≥ 8 **and** > 5% |
| invalid UTF-8 | 234 | 3,631 | 44.3% | > 30% **and** ≥ 8 NUL+control |

What this shows:

- Real C sources contain essentially no NUL or control bytes, so the NUL and
  control floors sit far above anything observed.
- The invalid-UTF-8 ratio alone **is not safe**. A pureftpd source in a legacy
  8-bit encoding is 44% invalid UTF-8. It stays `SourceText` only because the
  invalid-UTF-8 rule also requires at least 8 NUL/control bytes, and no real
  C source came close to that. Keep the co-signal requirement.

### `Binary` outside C-family files

Of the 5,271 non-C `Binary` results, the top extensions are `bmp`, `mod`
(ventoy GRUB modules), `ico`, `png`, `.a`, `.o`, `ttf`, `pdf`, `jpg`, `db`,
`exe`, `efi`, `ko` and `xz`. All are binary assets.

Exactly **one** file with a registered source extension came back `Binary`:
an OSS-Fuzz regression test in commons-lang (Java) whose string literals
embed raw NUL bytes, 1,524 of them in the first 8 KiB (about 19%). No
threshold can keep that file as text without also passing real binaries. It
is listed under "Known limitations" in the module docs.
