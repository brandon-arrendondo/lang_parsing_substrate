# Releasing

This page documents the maintainer release process: how a tagged version
becomes both a crates.io release and a PyPI wheel/sdist release.

## Overview

- The version is single-sourced in `Cargo.toml`'s `[package].version`.
  `invoke bump-version --new-version X.Y.Z` is the one place that edits it.
- `invoke publish` publishes the crate to **crates.io** (requires a clean
  tree and a `vX.Y.Z` tag on `HEAD` — see its own docstring in `tasks.py`).
- Pushing a `v*.*.*` tag to this repo ALSO triggers
  `.github/workflows/wheels.yml`, which builds per-platform `abi3` wheels
  (CPython 3.10+, one wheel per platform — no per-minor-version matrix
  needed) plus an sdist with maturin, and publishes them to **PyPI** via
  Trusted Publishing (OIDC — no API token). Both releases are cut from the
  same tag.

## One-time setup

On PyPI, configure a **Trusted Publisher** for the `lang-parsing-substrate`
project. Until the project's first publish, add it as a *pending* publisher
at <https://pypi.org/manage/account/publishing/> with:

| Field | Value |
|---|---|
| PyPI Project Name | `lang-parsing-substrate` |
| Owner | `brandon-arrendondo` |
| Repository name | `lang_parsing_substrate` |
| Workflow name | `wheels.yml` |
| Environment name | `release` |

The **PyPI Project Name must exactly match** the package name in
`pyproject.toml` (normalized) — a mismatch yields a `400 Non-user identities
cannot create new projects` error at publish time.

The `release` **environment** also needs to exist on the GitHub repo itself
(Settings → Environments → New environment → `release`) — `wheels.yml`'s
publish job runs under it, which is what makes the environment name in the
Trusted Publisher config resolve to anything. An environment with no
protection rules is fine; the OIDC trust binding is what actually gates the
publish, not branch protection.

## Release steps

`X.Y.Z` is the new version.

1. **Bump the version.**

   ```bash
   invoke bump-version --new-version X.Y.Z
   cargo build --all-features   # refresh Cargo.lock if it's tracked elsewhere
   git commit -am "chore: bump version to X.Y.Z"
   ```

2. **Tag and push.** This triggers both releases:

   ```bash
   git tag vX.Y.Z
   git push origin main --follow-tags
   ```

   `wheels.yml` builds manylinux + musllinux (x86_64, aarch64), macOS
   (x86_64 cross-compiled on Apple Silicon, aarch64), and Windows x64 wheels
   plus an sdist, then publishes to PyPI. Watch it under the repo's Actions
   tab.

3. **Publish to crates.io**, separately (the wheel and the crate are two
   different registries with two different publish mechanisms):

   ```bash
   invoke publish
   ```

## Verifying a release

```bash
pip install "lang-parsing-substrate==X.Y.Z"
python3 -c "import lang_parsing_substrate as lps; print(lps.supported_languages_report())"
```

## Notes & recovery

- The publish step uses `skip-existing: true`, so re-runs are idempotent:
  a platform wheel already on PyPI is skipped rather than failing on "file
  already exists" if a previous run partially succeeded.
- **PyPI versions are immutable** — a published version cannot be
  re-uploaded or overwritten. To ship a fix, cut a new version.
- `wheels.yml` can be run manually (Actions → "Wheels (PyPI)" → Run
  workflow) to validate the build matrix without publishing (publish is
  gated to tags).
