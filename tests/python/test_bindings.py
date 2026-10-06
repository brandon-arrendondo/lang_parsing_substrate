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


# ─── parse_tree ──────────────────────────────────────────────────────────────


def _columns(tree):
    names = (
        "kind field flags parent first_child next_sibling prev_sibling start_byte end_byte "
        "start_row start_col end_row end_col child_offset child_list"
    ).split()
    return {n: memoryview(getattr(tree, n)).cast("I") for n in names}


def test_parse_tree_columns_describe_the_tree():
    src = b"def f(a):\n    return g(a)\n"
    tree = lps.parse_tree("python", src)
    c = _columns(tree)
    assert len(tree) == len(c["kind"]) == len(c["child_offset"]) - 1
    assert tree.kinds[c["kind"][0]] == "module"
    assert c["parent"][0] == lps.NONE
    assert tree.fields[0] == ""

    # The function_definition's `name` field child spans "f".
    fn = c["child_list"][c["child_offset"][0]]
    assert tree.kinds[c["kind"][fn]] == "function_definition"
    kids = c["child_list"][c["child_offset"][fn] : c["child_offset"][fn + 1]]
    named = {tree.fields[c["field"][k]]: k for k in kids if c["field"][k]}
    name = named["name"]
    assert src[c["start_byte"][name] : c["end_byte"][name]] == b"f"
    assert (c["start_row"][name], c["start_col"][name]) == (0, 4)
    assert c["parent"][name] == fn


def test_parse_tree_links_agree():
    tree = lps.parse_tree("rust", RUST_SRC.encode())
    c = _columns(tree)
    for i in range(len(tree)):
        kids = list(c["child_list"][c["child_offset"][i] : c["child_offset"][i + 1]])
        linked = []
        k = c["first_child"][i]
        while k != lps.NONE:
            linked.append(k)
            k = c["next_sibling"][k]
        assert kids == linked
        for a, b in zip(kids, kids[1:]):
            assert c["prev_sibling"][b] == a


def test_parse_tree_flags_errors():
    tree = lps.parse_tree("c", b"int f( { return 1 }")
    flags = memoryview(tree.flags).cast("I")
    assert flags[0] & 16, "has_error on the root"
    assert any(f & (2 | 4) for f in flags), "an ERROR or MISSING node"


def test_parse_tree_tsx_parses_jsx():
    flags = memoryview(lps.parse_tree("tsx", b"const a = <div>{x}</div>;\n").flags).cast("I")
    assert not flags[0] & 16


# ─── query / tags_query ──────────────────────────────────────────────────────


def test_query_returns_named_captures_with_positions():
    src = "def f():\n    pass\n\ndef g():\n    f()\n"
    caps = lps.query("python", src, "(function_definition name: (identifier) @name)")
    assert [src[c.start_byte : c.end_byte] for c in caps] == ["f", "g"]
    assert caps[1].start_point == (3, 4)
    assert caps[1].name == "name" and caps[1].kind == "identifier"


def test_query_compile_error_raises():
    try:
        lps.query("python", "x = 1", "(no_such_kind) @x")
    except ValueError:
        return
    raise AssertionError("expected ValueError for a query that does not compile")


def test_tags_query_for_typescript_includes_javascript_definitions():
    q = lps.tags_query("typescript")
    caps = lps.query("typescript", "function f() {}\ninterface I {}\n", q)
    names = {c.name for c in caps}
    assert {"definition.function", "definition.interface"} <= names
    assert lps.tags_query("not-a-language") is None


# ─── tsx consistency ─────────────────────────────────────────────────────────


def test_tsx_key_is_accepted_everywhere():
    src = "import a from 'a';\n// tools:suppress knots:x\nconst b = <i/>;\n"
    assert lps.import_sources("tsx", src.encode()) == ["a"]
    assert lps.suppressions("tsx", src) is not None
    assert lps.ignored_regions("tsx", src) == []


# ─── the native Node (py-tree-sitter-compatible) ─────────────────────────────


def test_root_node_walks_like_py_tree_sitter():
    src = b"def f(a):\n    return g(a)\n"
    tree = lps.parse_tree("python", src)
    root = tree.root_node
    assert root.type == "module" and root.parent is None and tree.source == src
    fn = root.named_children[0]
    assert fn.type == "function_definition"
    name = fn.child_by_field_name("name")
    assert name.text == b"f"
    assert name.start_point == (0, 4) and name.end_point == (0, 5)
    assert (name.start_byte, name.end_byte) == (4, 5)
    assert name.parent == fn and name.is_named and not name.has_error
    assert fn.child_by_field_name("no_such_field") is None
    assert [c.type for c in fn.children_by_field_name("body")] == ["block"]
    body = fn.child_by_field_name("body")
    assert body.prev_named_sibling == fn.child_by_field_name("parameters")
    assert fn.child_count == len(fn.children) > fn.named_child_count == len(fn.named_children)


def test_node_identity_and_hashing():
    tree = lps.parse_tree("rust", RUST_SRC.encode())
    a, b = tree.root_node.children[0], tree.root_node.children[0]
    assert a == b and hash(a) == hash(b) and a.id == b.id
    assert a != tree.root_node.children[1]
    other = lps.parse_tree("rust", RUST_SRC.encode()).root_node.children[0]
    assert other != a and other.id != a.id, "ids never collide across trees"
    assert "function_item" in repr(a)


def test_node_error_flags():
    root = lps.parse_tree("c", b"int f( { return 1 }").root_node
    assert root.has_error
    stack, flagged = [root], False
    while stack:
        n = stack.pop()
        flagged |= n.is_error or n.is_missing
        stack.extend(n.children)
    assert flagged
