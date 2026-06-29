"""
Invoke tasks for lang-parsing-substrate development.

Usage:
    invoke check        # Run clippy and format check
    invoke build        # Build (debug by default; --release for release)
    invoke test         # Run all tests
    invoke clean        # Remove build artifacts
    invoke bump-version # Bump version in Cargo.toml

Install invoke: pip install invoke
"""

import re
from pathlib import Path

from invoke import task


def _read_cargo_version():
    cargo = Path("Cargo.toml").read_text()
    match = re.search(r'^version = "([^"]+)"', cargo, re.MULTILINE)
    if not match:
        raise RuntimeError("Could not find version in Cargo.toml")
    return match.group(1)


@task
def bump_version(c, new_version=None):
    """Bump version across all files that reference it.

    Reads the current version from Cargo.toml, then updates it there and in
    any other files that embed a version pin. If --new-version is omitted,
    prints the current version and the list of files that would be changed.

    Args:
        new_version: Target version string, e.g. 0.2.0 (no leading 'v').
    """
    current = _read_cargo_version()

    bare_version_files = [
        ("Cargo.toml", rf'^(version = "){re.escape(current)}(")', r"\g<1>{new}\g<2>"),
    ]

    tagged_version_files: list[str] = []

    if not new_version:
        print(f"Current version: {current}")
        print("\nFiles that would be updated:")
        for path, *_ in bare_version_files:
            print(f"  {path}  (bare version)")
        print("\nRun: invoke bump-version --new-version X.Y.Z")
        return

    tag_old = f"v{current}"
    tag_new = f"v{new_version}"
    changed = []

    for path, pattern, tmpl in bare_version_files:
        p = Path(path)
        text = p.read_text()
        updated = re.sub(pattern, tmpl.format(new=new_version), text, flags=re.MULTILINE)
        if updated != text:
            p.write_text(updated)
            changed.append(path)

    for path in tagged_version_files:
        p = Path(path)
        if not p.exists():
            continue
        text = p.read_text()
        updated = text.replace(tag_old, tag_new)
        if updated != text:
            p.write_text(updated)
            changed.append(path)

    if changed:
        print(f"Bumped {current} → {new_version} in:")
        for f in changed:
            print(f"  {f}")
    else:
        print(f"No occurrences of {current} found — nothing changed.")


@task
def check(c):
    """Run clippy and format check."""
    c.run("cargo clippy --all-features -- -D warnings", pty=True)
    c.run("cargo fmt --check", pty=True)


@task
def build(c, release=False):
    """Build the library.

    Args:
        release: Build in release mode (default: debug).
    """
    cmd = "cargo build --all-features"
    if release:
        cmd += " --release"
    c.run(cmd, pty=True)


@task
def test(c):
    """Run all tests across all feature combinations."""
    c.run("cargo test --all-features", pty=True)
    c.run(
        "cargo test --no-default-features --features lang-c,lang-cpp",
        pty=True,
    )


@task
def publish(c, dry_run=False):
    """Publish to crates.io.

    Verifies the working tree is clean and the current git tag matches
    Cargo.toml before publishing.

    Args:
        dry_run: Pass --dry-run to cargo publish (verify without uploading).
    """
    import subprocess

    result = subprocess.run(
        ["git", "status", "--porcelain"], capture_output=True, text=True
    )
    if result.stdout.strip():
        raise SystemExit("Working tree is dirty — commit or stash changes before publishing.")

    version = _read_cargo_version()
    tag = f"v{version}"

    result = subprocess.run(
        ["git", "tag", "--points-at", "HEAD"], capture_output=True, text=True
    )
    tags = result.stdout.split()
    if tag not in tags:
        raise SystemExit(
            f"HEAD is not tagged {tag} — run: git tag {tag} && git push origin {tag}"
        )

    flag = " --dry-run" if dry_run else ""
    print(f"Publishing lang-parsing-substrate {version} to crates.io{' (dry run)' if dry_run else ''}...")
    c.run(f"cargo publish{flag}", pty=True)


@task
def clean(c):
    """Remove build artifacts."""
    c.run("cargo clean", pty=True)
