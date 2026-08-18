# SPDX-License-Identifier: MIT
"""PyO3 binding tests — parse-and-analyze round trip through Python.

Requires the wheel built with the `pyo3` feature to be installed
(`maturin develop --release` or `pip install target/wheels/*.whl`). Not run
by `cargo test`; wired into `invoke test` as a conditional step.
"""

import lang_parsing_substrate as lps

RUST_SRC = """
fn helper(x: i32) -> i32 { x + 1 }

fn main() {
    let y = helper(41);
    println!("{}", y);
}
"""

C_SRC = """
int helper(int x) { return x + 1; }

int main(void) {
    int y = helper(41);
    return y;
}
"""


def test_call_edges_rust():
    edges = lps.call_edges("rust", RUST_SRC)
    pairs = {(e.caller, e.callee) for e in edges}
    assert ("main", "helper") in pairs


def test_call_edges_c():
    edges = lps.call_edges("c", C_SRC)
    pairs = {(e.caller, e.callee) for e in edges}
    assert ("main", "helper") in pairs


def test_import_sources_rust():
    src = "use std::collections::HashMap;\nfn main() {}\n"
    assert "std::collections::HashMap" in lps.import_sources("rust", src.encode())


def test_function_cfg_rust():
    cfg = lps.function_cfg("rust", RUST_SRC, "main")
    assert cfg is not None
    assert cfg.entry in [b.id for b in cfg.blocks]
    assert cfg.exits


def test_function_cfg_unknown_function_returns_none():
    assert lps.function_cfg("rust", RUST_SRC, "does_not_exist") is None


def test_function_fingerprints_rust():
    fps = lps.function_fingerprints("rust", RUST_SRC, min_nodes=1)
    names = {f.name for f in fps}
    assert {"helper", "main"} <= names


def test_languages_reflects_compiled_features():
    keys = {info.key for info in lps.languages()}
    assert "rust" in keys
    assert "c" in keys


def test_supported_languages_report_is_nonempty():
    assert "Rust" in lps.supported_languages_report()


def test_unknown_language_key_raises():
    try:
        lps.call_edges("not-a-real-language", "whatever")
    except ValueError:
        return
    raise AssertionError("expected ValueError for unknown language key")
