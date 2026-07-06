//! Call-graph edge extraction.
//!
//! Ported from knots' `external_calls` metric (function/macro-name collection
//! plus the call-node-kind vocabulary in `handle_call_node` /
//! `collect_external_calls_recursive`), generalized to record every call
//! target rather than only the ones that fall outside the file's own
//! function/macro names. Unlike [`crate::imports`], this dispatch is *not*
//! keyed by language: the node kinds involved (`call_expression`, `call`,
//! `method_invocation`, ...) are shared across enough grammars that a single
//! generic walk covers all 16 languages without a per-language table.
//!
//! No cross-file resolution happens here — `is_external` only means "not one
//! of the names this file itself defines." Turning that into an actual
//! cross-file call graph (deciding which external callee belongs to which
//! other file) is a per-tool concern, same as `import_sources`.

use std::collections::HashSet;
use tree_sitter::Node;

/// One call site: `caller` (a function/macro name defined in this file)
/// invoked `callee`. `is_external` is true when `callee` is not among the
/// function/macro names this file defines — i.e. it's presumably resolved
/// elsewhere (another file, a library, a language builtin).
///
/// Only named functions become callers; anonymous closures/lambdas are
/// skipped as call *sites* (matching knots' default, non-anonymous-counting
/// behavior), though calls made from inside them are still attributed to
/// their nearest enclosing named function, since the underlying traversal
/// does not stop at nested-function boundaries — the same behavior as the
/// existing `external_calls` metric it extends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEdge {
    pub caller: String,
    pub callee: String,
    pub is_external: bool,
}

/// Builds call-graph edges for every named function/macro in `tree`.
///
/// Building a corpus-wide graph is left to the caller: concatenate
/// `call_edges` results across every file's tree. This function only sees
/// one file, so `is_external` can't distinguish "external to this file, but
/// defined in another file of the same project" from "genuinely external" —
/// that classification needs the whole corpus's local-name sets, which is
/// the caller's job.
pub fn call_edges(root: Node, source: &str) -> Vec<CallEdge> {
    let local_names = collect_local_names(root, source);
    let mut functions = Vec::new();
    collect_functions(root, source, &mut functions);

    let mut edges = Vec::new();
    for (node, caller) in functions {
        let mut callees = Vec::new();
        // A syntax error anywhere in this function's subtree means its span
        // can't be trusted (see collect_call_names doc comment) — a brace
        // that opens and closes in different branches of the same repeated
        // #ifdef guard, for example, makes tree-sitter-c's grammar swallow
        // every subsequent sibling function as a nested descendant. In that
        // case, stop at nested function boundaries instead of attributing
        // a swallowed sibling's calls to this caller.
        collect_call_names(node, source, node.has_error(), &mut callees);
        for callee in callees {
            let is_external = !local_names.contains(&callee);
            edges.push(CallEdge {
                caller: caller.clone(),
                callee,
                is_external,
            });
        }
    }
    edges
}

fn collect_functions<'a>(node: Node<'a>, source: &str, out: &mut Vec<(Node<'a>, String)>) {
    if is_function_kind(node.kind()) && !is_macro_function_definition(node) {
        if let Some(name) = get_function_name(node, source) {
            out.push((node, name));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_functions(child, source, out);
    }
}

/// Returns `true` if `kind` is a function-like node this module treats as a
/// potential call-graph caller.
pub fn is_function_kind(kind: &str) -> bool {
    matches!(
        kind,
        "function_definition"
            | "function_item"
            | "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "generator_function_declaration"
            | "generator_function"
            | "subprogram_body"
            | "expression_function_declaration"
            | "task_body"
            | "method_declaration"
            | "func_literal"
            | "constructor_declaration"
            | "local_function_statement"
            | "init_declaration"
            // Fortran: function subprogram, subroutine subprogram, module procedure, main program
            | "function"
            | "subroutine"
            | "module_procedure"
            | "program"
    )
}

fn is_macro_function_definition(node: Node) -> bool {
    node.kind() == "function_definition"
        && node
            .child_by_field_name("declarator")
            .map(|d| d.kind() == "parenthesized_declarator")
            .unwrap_or(false)
}

/// Resolves the name of a function-like node, or `None` for genuinely
/// anonymous usage (callbacks, IIFEs, return values, etc.) that can't be
/// traced back to a binding.
pub fn get_function_name(node: Node, source: &str) -> Option<String> {
    match node.kind() {
        "function_item"
        | "method_definition"
        | "generator_function_declaration"
        | "generator_function"
        | "method_declaration"
        | "constructor_declaration"
        | "local_function_statement" => name_field(node, source),

        "function_declaration" => name_field(node, source).or_else(|| {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find(|c| c.kind() == "simple_identifier");
            found
                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                .map(|s| s.to_string())
        }),

        "function_definition" => name_field(node, source)
            .or_else(|| get_c_name(node, source))
            .or_else(|| get_lua_assignment_name(node, source)),

        "function_expression" => {
            name_field(node, source).or_else(|| get_name_from_assignment_context(node, source))
        }

        "arrow_function" => get_name_from_assignment_context(node, source),

        "init_declaration" => Some("init".to_string()),

        "func_literal" => None,

        "subprogram_body" | "expression_function_declaration" => {
            let mut cursor = node.walk();
            let spec = node.children(&mut cursor).find(|c| {
                matches!(
                    c.kind(),
                    "function_specification" | "procedure_specification"
                )
            })?;
            name_field(spec, source)
        }

        "task_body" => {
            let mut cursor = node.walk();
            let found = node
                .children(&mut cursor)
                .find(|c| c.kind() == "identifier");
            found
                .and_then(|c| c.utf8_text(source.as_bytes()).ok())
                .map(|s| s.to_string())
        }

        "function" => name_in_child(node, "function_statement", source),
        "subroutine" => name_in_child(node, "subroutine_statement", source),
        "module_procedure" => name_in_child(node, "module_procedure_statement", source),

        "program" => {
            let mut cursor = node.walk();
            let stmt = node
                .children(&mut cursor)
                .find(|c| c.kind() == "program_statement")?;
            let mut inner = stmt.walk();
            let first_named = stmt.named_children(&mut inner).next();
            first_named
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(|s| s.to_string())
                .or_else(|| Some("program".to_string()))
        }

        _ => get_c_name(node, source),
    }
}

fn name_field(node: Node, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source.as_bytes()).ok())
        .map(|s| s.to_string())
}

fn name_in_child(node: Node, child_kind: &str, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    let child = node
        .children(&mut cursor)
        .find(|c| c.kind() == child_kind)?;
    name_field(child, source)
}

fn get_c_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" {
            return get_declarator_name(child, source);
        }
        if child.kind() == "pointer_declarator" {
            if let Some(name) = get_function_name_from_declarator(child, source) {
                return Some(name);
            }
        }
    }
    None
}

fn get_function_name_from_declarator(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" {
            return get_declarator_name(child, source);
        } else if child.kind() == "pointer_declarator" {
            if let Some(name) = get_function_name_from_declarator(child, source) {
                return Some(name);
            }
        }
    }
    None
}

fn get_declarator_name(node: Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "identifier"
            | "qualified_identifier"
            | "destructor_name"
            | "operator_name"
            | "field_identifier" => {
                return Some(child.utf8_text(source.as_bytes()).ok()?.to_string());
            }
            "pointer_declarator" | "function_declarator" => {
                if let Some(name) = get_declarator_name(child, source) {
                    return Some(name);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract the name of a Lua anonymous function_definition from its
/// assignment context.
fn get_lua_assignment_name(node: Node, source: &str) -> Option<String> {
    let parent = node.parent()?;
    match parent.kind() {
        "field" => {
            let mut cur = parent.walk();
            let found = parent
                .named_children(&mut cur)
                .find(|c| c.kind() == "identifier");
            found
                .and_then(|n| n.utf8_text(source.as_bytes()).ok())
                .map(|s| s.to_string())
        }
        "expression_list" => {
            let idx = {
                let mut cur = parent.walk();
                let pos = parent
                    .named_children(&mut cur)
                    .position(|c| c.id() == node.id())
                    .unwrap_or(0);
                pos
            };
            let assign = parent.parent()?;
            if assign.kind() != "assignment_statement" {
                return None;
            }
            let mut cur = assign.walk();
            let found = assign
                .children(&mut cur)
                .find(|c| c.kind() == "variable_list");
            let var_list = found?;
            let mut cur2 = var_list.walk();
            let var = var_list.named_children(&mut cur2).nth(idx)?;
            if var.kind() != "identifier" {
                return None;
            }
            var.utf8_text(source.as_bytes()).ok().map(|s| s.to_string())
        }
        _ => None,
    }
}

/// Extract the name of an arrow_function or anonymous function_expression
/// from the surrounding assignment context. Returns None for truly
/// anonymous usage (callbacks, IIFEs, return values, etc.).
fn get_name_from_assignment_context(node: Node, source: &str) -> Option<String> {
    let parent = node.parent()?;
    match parent.kind() {
        // const foo = () => {}  or  const foo = function() {}
        "variable_declarator" => parent
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(|s| s.to_string()),
        // { foo: () => {} }
        "pair" => parent
            .child_by_field_name("key")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(|s| s.to_string()),
        // class { foo = () => {} }
        "public_field_definition" => parent
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source.as_bytes()).ok())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Collects all function and macro names defined in this translation unit.
/// Used to classify call sites as local vs. external.
pub fn collect_local_names(root: Node, source: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    collect_local_names_recursive(root, source, &mut names);
    names
}

fn collect_local_names_recursive(node: Node, source: &str, names: &mut HashSet<String>) {
    if is_function_kind(node.kind()) {
        if let Some(name) = get_function_name(node, source) {
            names.insert(name);
        }
    } else if matches!(node.kind(), "preproc_def" | "preproc_function_def") {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                names.insert(name.to_string());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_local_names_recursive(child, source, names);
    }
}

fn handle_call_node(node: Node, source: &str, out: &mut Vec<String>) {
    let Some(func_node) = node.child_by_field_name("function") else {
        return;
    };
    match func_node.kind() {
        "identifier" => {
            if let Ok(name) = func_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
        "scoped_identifier"
        | "attribute"
        | "member_expression"
        | "selector_expression"
        | "member_access_expression" => {
            if let Ok(name) = func_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
        "field_expression" => {
            if let Some(field) = func_node.child_by_field_name("field") {
                if let Ok(method_name) = field.utf8_text(source.as_bytes()) {
                    out.push(method_name.to_string());
                }
            }
        }
        _ => {}
    }
}

/// Collects every call target name reachable from `node`, recursing through
/// nested function bodies (a nested closure's calls are attributed to every
/// enclosing named function, not just its own) — matching the traversal the
/// `external_calls` metric already uses.
///
/// `stop_at_nested`: when true, does not descend into a nested function-like
/// node (see [`is_function_kind`]). Callers set this when `node.has_error()`
/// — a syntax error anywhere in the subtree means the span can't be trusted,
/// and for grammars where a function-like node can never legitimately nest
/// inside another of the same call-graph role (e.g. C's `function_definition`
/// — C has no nested functions), an apparent nesting is a sign the grammar
/// swallowed unrelated sibling code, not a real closure. Attributing that
/// sibling's calls to the outer, corrupted caller would be wrong, so the walk
/// stops at the boundary instead and lets the nested node be counted (in a
/// separate, correctly-scoped pass) under its own name.
fn collect_call_names(node: Node, source: &str, stop_at_nested: bool, out: &mut Vec<String>) {
    if node.kind() == "call_expression"
        || node.kind() == "call"
        || node.kind() == "invocation_expression"
    {
        handle_call_node(node, source, out);
    }
    if node.kind() == "procedure_call_statement" || node.kind() == "function_call" {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
    }
    if node.kind() == "method_invocation" {
        if let Some(name_node) = node.child_by_field_name("name") {
            if let Ok(name) = name_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
    }
    if node.kind() == "call_expression" && node.child_by_field_name("function").is_none() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !child.is_named() {
                continue;
            }
            if matches!(
                child.kind(),
                "value_arguments" | "type_arguments" | "annotated_lambda" | "call_suffix"
            ) {
                continue;
            }
            let text = child.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            out.push(text);
            break;
        }
    }
    if node.kind() == "object_creation_expression" {
        if let Some(type_node) = node.child_by_field_name("type") {
            if let Ok(name) = type_node.utf8_text(source.as_bytes()) {
                out.push(name.to_string());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if stop_at_nested && is_function_kind(child.kind()) && !is_macro_function_definition(child)
        {
            continue;
        }
        collect_call_names(child, source, stop_at_nested, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::language_for_key;
    use tree_sitter::Parser;

    fn edges(key: &str, code: &str) -> Vec<CallEdge> {
        let language = language_for_key(key).unwrap_or_else(|| panic!("no grammar for {key}"));
        let mut parser = Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(code, None).unwrap();
        call_edges(tree.root_node(), code)
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn rust_local_and_external_calls() {
        let code =
            "fn helper() {}\nfn main() { helper(); println!(\"hi\"); std::fs::read(\"x\"); }\n";
        let e = edges("rust", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "std::fs::read" && c.is_external));
    }

    #[cfg(feature = "lang-python")]
    #[test]
    fn python_calls() {
        let code = "def helper():\n    pass\n\ndef main():\n    helper()\n    os.path.join('a')\n";
        let e = edges("python", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "os.path.join" && c.is_external));
    }

    #[cfg(feature = "lang-go")]
    #[test]
    fn go_calls() {
        let code =
            "package main\nfunc helper() {}\nfunc main() {\n\thelper()\n\tfmt.Println(\"hi\")\n}\n";
        let e = edges("go", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "fmt.Println" && c.is_external));
    }

    #[cfg(feature = "lang-java")]
    #[test]
    fn java_method_invocation() {
        let code = "class A {\n  void helper() {}\n  void main() {\n    helper();\n    System.out.println(\"hi\");\n  }\n}\n";
        let e = edges("java", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "println" && c.is_external));
    }

    #[cfg(feature = "lang-c")]
    #[test]
    fn c_calls() {
        let code =
            "int helper() { return 1; }\nint main() { helper(); printf(\"hi\"); return 0; }\n";
        let e = edges("c", code);
        assert!(e.contains(&CallEdge {
            caller: "main".into(),
            callee: "helper".into(),
            is_external: false,
        }));
        assert!(e
            .iter()
            .any(|c| c.caller == "main" && c.callee == "printf" && c.is_external));
    }

    #[cfg(feature = "lang-rust")]
    #[test]
    fn no_functions_yields_no_edges() {
        assert!(edges("rust", "const X: i32 = 1;\n").is_empty());
    }

    /// Regression for the sqlite3Init/sqlite3InitOne false MSC04-C
    /// indirect-recursion cycle (tools_sqc task 267/296), reproduced with the
    /// real sqlite3/src/prepare.c source (trimmed to just the two functions
    /// involved): a brace that opens under `#ifndef SQLITE_OMIT_AUTHORIZATION`
    /// and closes under a second, identical `#ifndef SQLITE_OMIT_AUTHORIZATION`
    /// guard a few lines later can't be reconciled by tree-sitter-c without a
    /// real preprocessor. It emits a local error and `sqlite3InitOne`'s
    /// function_definition never closes normally, nesting `sqlite3Init` (and
    /// everything after it in the file) as a descendant. Without the
    /// has_error() guard, `sqlite3InitOne`'s callee set wrongly absorbs
    /// `sqlite3Init`'s calls too -- including a call back to `sqlite3InitOne`
    /// itself, producing a false self-recursion edge that doesn't exist in
    /// the source.
    #[test]
    fn c_ifdef_spanning_brace_does_not_leak_swallowed_sibling_calls() {
        let code = r#"int sqlite3InitOne(sqlite3 *db, int iDb, char **pzErrMsg, u32 mFlags){
  int rc;
  int i;
#ifndef SQLITE_OMIT_DEPRECATED
  int size;
#endif
  Db *pDb;
  char const *azArg[6];
  int meta[5];
  InitData initData;
  const char *zSchemaTabName;
  int openedTransaction = 0;
  int mask = ((db->mDbFlags & DBFLAG_EncodingFixed) | ~DBFLAG_EncodingFixed);

  assert( (db->mDbFlags & DBFLAG_SchemaKnownOk)==0 );
  assert( iDb>=0 && iDb<db->nDb );
  assert( db->aDb[iDb].pSchema );
  assert( sqlite3_mutex_held(db->mutex) );
  assert( iDb==1 || sqlite3BtreeHoldsMutex(db->aDb[iDb].pBt) );

  db->init.busy = 1;

  /* Construct the in-memory representation schema tables (sqlite_schema or
  ** sqlite_temp_schema) by invoking the parser directly.  The appropriate
  ** table name will be inserted automatically by the parser so we can just
  ** use the abbreviation "x" here.  The parser will also automatically tag
  ** the schema table as read-only. */
  azArg[0] = "table";
  azArg[1] = zSchemaTabName = SCHEMA_TABLE(iDb);
  azArg[2] = azArg[1];
  azArg[3] = "1";
  azArg[4] = "CREATE TABLE x(type text,name text,tbl_name text,"
                            "rootpage int,sql text)";
  azArg[5] = 0;
  initData.db = db;
  initData.iDb = iDb;
  initData.rc = SQLITE_OK;
  initData.pzErrMsg = pzErrMsg;
  initData.mInitFlags = mFlags;
  initData.nInitRow = 0;
  initData.mxPage = 0;
  sqlite3InitCallback(&initData, 5, (char **)azArg, 0);
  db->mDbFlags &= mask;
  if( initData.rc ){
    rc = initData.rc;
    goto error_out;
  }

  /* Create a cursor to hold the database open
  */
  pDb = &db->aDb[iDb];
  if( pDb->pBt==0 ){
    assert( iDb==1 );
    DbSetProperty(db, 1, DB_SchemaLoaded);
    rc = SQLITE_OK;
    goto error_out;
  }

  /* If there is not already a read-only (or read-write) transaction opened
  ** on the b-tree database, open one now. If a transaction is opened, it
  ** will be closed before this function returns.  */
  sqlite3BtreeEnter(pDb->pBt);
  if( sqlite3BtreeTxnState(pDb->pBt)==SQLITE_TXN_NONE ){
    rc = sqlite3BtreeBeginTrans(pDb->pBt, 0, 0);
    if( rc!=SQLITE_OK ){
      sqlite3SetString(pzErrMsg, db, sqlite3ErrStr(rc));
      goto initone_error_out;
    }
    openedTransaction = 1;
  }

  /* Get the database meta information.
  **
  ** Meta values are as follows:
  **    meta[0]   Schema cookie.  Changes with each schema change.
  **    meta[1]   File format of schema layer.
  **    meta[2]   Size of the page cache.
  **    meta[3]   Largest rootpage (auto/incr_vacuum mode)
  **    meta[4]   Db text encoding. 1:UTF-8 2:UTF-16LE 3:UTF-16BE
  **    meta[5]   User version
  **    meta[6]   Incremental vacuum mode
  **    meta[7]   unused
  **    meta[8]   unused
  **    meta[9]   unused
  **
  ** Note: The #defined SQLITE_UTF* symbols in sqliteInt.h correspond to
  ** the possible values of meta[4].
  */
  for(i=0; i<ArraySize(meta); i++){
    sqlite3BtreeGetMeta(pDb->pBt, i+1, (u32 *)&meta[i]);
  }
  if( (db->flags & SQLITE_ResetDatabase)!=0 ){
    memset(meta, 0, sizeof(meta));
  }
  pDb->pSchema->schema_cookie = meta[BTREE_SCHEMA_VERSION-1];

  /* If opening a non-empty database, check the text encoding. For the
  ** main database, set sqlite3.enc to the encoding of the main database.
  ** For an attached db, it is an error if the encoding is not the same
  ** as sqlite3.enc.
  */
  if( meta[BTREE_TEXT_ENCODING-1] ){  /* text encoding */
    if( iDb==0 && (db->mDbFlags & DBFLAG_EncodingFixed)==0 ){
      u8 encoding;
#ifndef SQLITE_OMIT_UTF16
      /* If opening the main database, set ENC(db). */
      encoding = (u8)meta[BTREE_TEXT_ENCODING-1] & 3;
      if( encoding==0 ) encoding = SQLITE_UTF8;
#else
      encoding = SQLITE_UTF8;
#endif
      sqlite3SetTextEncoding(db, encoding);
    }else{
      /* If opening an attached database, the encoding much match ENC(db) */
      if( (meta[BTREE_TEXT_ENCODING-1] & 3)!=ENC(db) ){
        sqlite3SetString(pzErrMsg, db, "attached databases must use the same"
            " text encoding as main database");
        rc = SQLITE_ERROR;
        goto initone_error_out;
      }
    }
  }
  pDb->pSchema->enc = ENC(db);

  if( pDb->pSchema->cache_size==0 ){
#ifndef SQLITE_OMIT_DEPRECATED
    size = sqlite3AbsInt32(meta[BTREE_DEFAULT_CACHE_SIZE-1]);
    if( size==0 ){ size = SQLITE_DEFAULT_CACHE_SIZE; }
    pDb->pSchema->cache_size = size;
#else
    pDb->pSchema->cache_size = SQLITE_DEFAULT_CACHE_SIZE;
#endif
    sqlite3BtreeSetCacheSize(pDb->pBt, pDb->pSchema->cache_size);
  }

  /*
  ** file_format==1    Version 3.0.0.
  ** file_format==2    Version 3.1.3.  // ALTER TABLE ADD COLUMN
  ** file_format==3    Version 3.1.4.  // ditto but with non-NULL defaults
  ** file_format==4    Version 3.3.0.  // DESC indices.  Boolean constants
  */
  pDb->pSchema->file_format = (u8)meta[BTREE_FILE_FORMAT-1];
  if( pDb->pSchema->file_format==0 ){
    pDb->pSchema->file_format = 1;
  }
  if( pDb->pSchema->file_format>SQLITE_MAX_FILE_FORMAT ){
    sqlite3SetString(pzErrMsg, db, "unsupported file format");
    rc = SQLITE_ERROR;
    goto initone_error_out;
  }

  /* Ticket #2804:  When we open a database in the newer file format,
  ** clear the legacy_file_format pragma flag so that a VACUUM will
  ** not downgrade the database and thus invalidate any descending
  ** indices that the user might have created.
  */
  if( iDb==0 && meta[BTREE_FILE_FORMAT-1]>=4 ){
    db->flags &= ~(u64)SQLITE_LegacyFileFmt;
  }

  /* Read the schema information out of the schema tables
  */
  assert( db->init.busy );
  initData.mxPage = sqlite3BtreeLastPage(pDb->pBt);
  {
    char *zSql;
    zSql = sqlite3MPrintf(db,
        "SELECT*FROM\"%w\".%s ORDER BY rowid",
        db->aDb[iDb].zDbSName, zSchemaTabName);
#ifndef SQLITE_OMIT_AUTHORIZATION
    {
      sqlite3_xauth xAuth;
      xAuth = db->xAuth;
      db->xAuth = 0;
#endif
      rc = sqlite3_exec(db, zSql, sqlite3InitCallback, &initData, 0);
#ifndef SQLITE_OMIT_AUTHORIZATION
      db->xAuth = xAuth;
    }
#endif
    if( rc==SQLITE_OK ) rc = initData.rc;
    sqlite3DbFree(db, zSql);
#ifndef SQLITE_OMIT_ANALYZE
    if( rc==SQLITE_OK ){
      sqlite3AnalysisLoad(db, iDb);
    }
#endif
  }
  assert( pDb == &(db->aDb[iDb]) );
  if( db->mallocFailed ){
    rc = SQLITE_NOMEM_BKPT;
    sqlite3ResetAllSchemasOfConnection(db);
    pDb = &db->aDb[iDb];
  }else
  if( rc==SQLITE_OK || ((db->flags&SQLITE_NoSchemaError) && rc!=SQLITE_NOMEM)){
    /* Hack: If the SQLITE_NoSchemaError flag is set, then consider
    ** the schema loaded, even if errors (other than OOM) occurred. In
    ** this situation the current sqlite3_prepare() operation will fail,
    ** but the following one will attempt to compile the supplied statement
    ** against whatever subset of the schema was loaded before the error
    ** occurred.
    **
    ** The primary purpose of this is to allow access to the sqlite_schema
    ** table even when its contents have been corrupted.
    */
    DbSetProperty(db, iDb, DB_SchemaLoaded);
    rc = SQLITE_OK;
  }

  /* Jump here for an error that occurs after successfully allocating
  ** curMain and calling sqlite3BtreeEnter(). For an error that occurs
  ** before that point, jump to error_out.
  */
initone_error_out:
  if( openedTransaction ){
    sqlite3BtreeCommit(pDb->pBt);
  }
  sqlite3BtreeLeave(pDb->pBt);

error_out:
  if( rc ){
    if( rc==SQLITE_NOMEM || rc==SQLITE_IOERR_NOMEM ){
      sqlite3OomFault(db);
    }
    sqlite3ResetOneSchema(db, iDb);
  }
  db->init.busy = 0;
  return rc;
}

/*
** Initialize all database files - the main database file, the file
** used to store temporary tables, and any additional database files
** created using ATTACH statements.  Return a success code.  If an
** error occurs, write an error message into *pzErrMsg.
**
** After a database is initialized, the DB_SchemaLoaded bit is set
** bit is set in the flags field of the Db structure.
*/
int sqlite3Init(sqlite3 *db, char **pzErrMsg){
  int i, rc;
  int commit_internal = !(db->mDbFlags&DBFLAG_SchemaChange);

  assert( sqlite3_mutex_held(db->mutex) );
  assert( sqlite3BtreeHoldsMutex(db->aDb[0].pBt) );
  assert( db->init.busy==0 );
  ENC(db) = SCHEMA_ENC(db);
  assert( db->nDb>0 );
  /* Do the main schema first */
  if( !DbHasProperty(db, 0, DB_SchemaLoaded) ){
    rc = sqlite3InitOne(db, 0, pzErrMsg, 0);
    if( rc ) return rc;
  }
  /* All other schemas after the main schema. The "temp" schema must be last */
  for(i=db->nDb-1; i>0; i--){
    assert( i==1 || sqlite3BtreeHoldsMutex(db->aDb[i].pBt) );
    if( !DbHasProperty(db, i, DB_SchemaLoaded) ){
      rc = sqlite3InitOne(db, i, pzErrMsg, 0);
      if( rc ) return rc;
    }
  }
  if( commit_internal ){
    sqlite3CommitInternalChanges(db);
  }
  return SQLITE_OK;
}

"#;
        let e = edges("c", code);
        // The real edge must still be found (via sqlite3Init's own, correctly
        // scoped span).
        assert!(e.contains(&CallEdge {
            caller: "sqlite3Init".into(),
            callee: "sqlite3InitOne".into(),
            is_external: false,
        }));
        // sqlite3InitOne must NOT be credited with calling itself -- that
        // edge only exists because the corrupted span swallowed sqlite3Init
        // (whose real body calls sqlite3InitOne) as a descendant. Crediting
        // it to sqlite3InitOne is exactly the false-recursion bug.
        assert!(!e
            .iter()
            .any(|c| c.caller == "sqlite3InitOne" && c.callee == "sqlite3InitOne"));
        // Likewise sqlite3CommitInternalChanges is only ever called from
        // sqlite3Init's real body, never sqlite3InitOne's.
        assert!(!e
            .iter()
            .any(|c| c.caller == "sqlite3InitOne" && c.callee == "sqlite3CommitInternalChanges"));
    }
}
