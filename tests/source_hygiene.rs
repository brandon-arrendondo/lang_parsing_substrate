//! Source-level guards against traversal idioms that are quadratic on real trees.
//!
//! `Node::child(i)` walks from the first child, so an index loop over a node's
//! children is O(k²) in the child count; aurora-lint found about 1,090 of them.
//! Use `child_nodes` / `named_child_nodes` or
//! `node.children(&mut cursor)` instead. This test keeps the substrate's own source
//! free of the idiom.

use std::fs;
use std::path::Path;

fn sources(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push((
                path.display().to_string(),
                fs::read_to_string(&path).unwrap(),
            ));
        }
    }
}

#[test]
fn no_indexed_child_loops_in_src() {
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut offenders = Vec::new();
    for (path, text) in &files {
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") || !t.contains("child_count()") || !t.starts_with("for ") {
                continue;
            }
            let window = lines[i..lines.len().min(i + 6)].join("\n");
            if window.contains(".child(") || window.contains(".named_child(") {
                offenders.push(format!("{path}:{}", i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "indexed child loops (O(k^2); use child_nodes / node.children(&mut cursor)): {offenders:?}"
    );
}
