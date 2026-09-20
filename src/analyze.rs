//! Diagnostics: parse the document with tree-sitter and report ERROR nodes
//! as LSP diagnostics. Semantic validation (assignment forms, condition
//! shapes) is layered on top in later milestones.

use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

use crate::material::MaterialIndex;

/// Syntax diagnostics: tree-sitter ERROR/MISSING nodes, minus the known
/// '*' comment-vs-multiplication false alarms.
pub fn diagnostics(text: &str) -> Vec<Diagnostic> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_sapvc::language())
        .expect("sapvc language");

    let tree = match parser.parse(text, None) {
        Some(t) => t,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    collect_errors(tree.root_node(), text, &mut out, &mut None);
    out
}

/// Semantic diagnostics against the material package: unknown variant tables
/// (error) and unknown characteristic/object references (warning). No-op when
/// no material index is loaded.
pub fn semantic_diagnostics(
    text: &str,
    tree: &tree_sitter::Tree,
    mat: &MaterialIndex,
) -> Vec<Diagnostic> {
    if mat.is_empty() {
        return Vec::new();
    }
    let aliases = collect_aliases(text);
    let mut out = Vec::new();
    check_node(tree.root_node(), text, mat, &aliases, &mut out);
    out
}

/// WHERE-clause aliases: `WHERE NS = TXT_NON_STD_REMARK` declares NS as an
/// alias of the real characteristic — subsequent conditions reference NS.
/// The pattern is `WHERE <alias> = <identifier>` (identifier-to-identifier).
fn collect_aliases(text: &str) -> std::collections::HashMap<String, String> {
    let mut aliases = std::collections::HashMap::new();
    let re = regex::Regex::new(r"(?i)\bWHERE\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([A-Za-z_][A-Za-z0-9_]*)")
        .expect("alias regex");
    for caps in re.captures_iter(text) {
        aliases.insert(caps[1].to_string(), caps[2].to_string());
    }
    aliases
}

fn check_node(
    node: tree_sitter::Node,
    text: &str,
    mat: &MaterialIndex,
    aliases: &std::collections::HashMap<String, String>,
    out: &mut Vec<Diagnostic>,
) {
    match node.kind() {
        // TABLE <name>( ... ) parses as keyword_call: [keyword][callee id][args...]
        "keyword_call" => {
            let mut c = node.walk();
            let mut kids = node.children(&mut c);
            let kw = kids.next().map(|k| text[k.start_byte()..k.end_byte()].to_owned());
            let callee = kids.next();
            let kw_upper = kw.as_deref().unwrap_or("").to_uppercase();
            let callee_name = callee
                .filter(|n| n.kind() == "identifier")
                .map(|n| text[n.start_byte()..n.end_byte()].to_owned());
            match kw_upper.as_str() {
                "TABLE" => {
                    if let Some(name) = &callee_name {
                        if !mat.tables.is_empty() && !mat.tables.contains(name) {
                            let r = callee.map(|n| node_range(n, text)).unwrap_or_default();
                            out.push(Diagnostic {
                                range: r,
                                severity: Some(DiagnosticSeverity::ERROR),
                                message: format!(
                                    "Unknown variant table `{name}` (not in material package)"
                                ),
                                ..Default::default()
                            });
                        }
                        // named arguments must be columns of that table
                        if let Some(cols) = mat.vt_chars.get(name) {
                            let mut ac = node.walk();
                            for arg in node.children(&mut ac) {
                                if arg.kind() == "named_argument" {
                                    if let Some(id) = first_identifier(arg) {
                                        let an = &text[id.start_byte()..id.end_byte()];
                                        if !cols.iter().any(|c| c == an) {
                                            out.push(Diagnostic {
                                                range: node_range(id, text),
                                                severity: Some(DiagnosticSeverity::WARNING),
                                                message: format!(
                                                    "`{an}` is not a column of {name} (columns: {})",
                                                    cols.join(", ")
                                                ),
                                                ..Default::default()
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                "FUNCTION" => {
                    if let Some(name) = &callee_name {
                        let up = name.to_uppercase();
                        if !mat.variant_functions.is_empty()
                            && !mat.variant_functions.contains(&up)
                        {
                            let r = callee.map(|n| node_range(n, text)).unwrap_or_default();
                            out.push(Diagnostic {
                                range: r,
                                severity: Some(DiagnosticSeverity::WARNING),
                                message: format!(
                                    "Unknown variant function `{up}` — known: {}",
                                    mat.variant_functions.iter().cloned().collect::<Vec<_>>().join(", ")
                                ),
                                ..Default::default()
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        // OBJECTS: section declarations — IS_A(300) target class must exist
        // in the material's class universe (direct + BOM K-item classes).
        "object_decl" => {
            // object_decl: identifier 'IS_A' '(' number ')' identifier [where..]
            let mut c = node.walk();
            let ids: Vec<tree_sitter::Node> = node
                .children(&mut c)
                .filter(|n| n.kind() == "identifier")
                .collect();
            if let Some(class_node) = ids.get(1) {
                let name = &text[class_node.start_byte()..class_node.end_byte()];
                if !mat.classes.is_empty() && !mat.classes.contains(name) {
                    out.push(Diagnostic {
                        range: node_range(*class_node, text),
                        severity: Some(DiagnosticSeverity::WARNING),
                        message: format!(
                            "Unknown class `{name}` — not in material class universe ({} classes known)",
                            mat.classes.len()
                        ),
                        ..Default::default()
                    });
                }
            }
        }

        // characteristic/object reference
        "identifier" => {
            if let Some(p) = node.parent() {
                if matches!(p.kind(), "bare_ref" | "object_ref") {
                    let name = &text[node.start_byte()..node.end_byte()];
                    let is_alias = aliases.contains_key(name);
                    if !name.starts_with('$')
                        && !is_alias
                        && !mat.chars.contains(name)
                        && !mat.objects.contains(name)
                    {
                        out.push(Diagnostic {
                            range: node_range(node, text),
                            severity: Some(DiagnosticSeverity::WARNING),
                            message: format!(
                                "Unknown characteristic/object `{name}` — not in material package (chars: {}, objects: {})",
                                mat.chars.len(),
                                mat.objects.len()
                            ),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        // value / type check on assignments: `$SELF.<CHAR> = <literal>`
        // (both the statement-level node and the RESTRICTIONS/INFERENCES
        // restriction_statement arm share this check)
        "assignment" | "conditional_assignment" | "restriction_statement" => {
            check_assignment_values(node, text, mat, aliases, out);
        }
        _ => {}
    }
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        check_node(ch, text, mat, aliases, out);
    }
}

/// Value/type checks on an assignment-shaped node: LHS characteristic (with
/// WHERE-alias resolution) vs RHS string/number literals.
fn check_assignment_values(
    node: tree_sitter::Node,
    text: &str,
    mat: &MaterialIndex,
    aliases: &std::collections::HashMap<String, String>,
    out: &mut Vec<Diagnostic>,
) {
    let mut c = node.walk();
    let mut kids = node.children(&mut c);
    let lhs = kids.next();
    let rhs = kids.find(|n| n.kind() == "expression");
    let (Some(lhs), Some(rhs)) = (lhs, rhs) else { return };
    let Some(id) = first_identifier(lhs) else { return };
    let char_name = &text[id.start_byte()..id.end_byte()];
    // WHERE aliases (`WHERE NS = TXT_NON_STD_REMARK`) resolve to the real
    // characteristic for value checks.
    let real = aliases.get(char_name).map(|s| s.as_str()).unwrap_or(char_name);
    let Some(info) = mat.char_values.get(real) else { return };
    let mut rc = rhs.walk();
    for v in rhs.children(&mut rc) {
        if v.kind() != "value" {
            continue;
        }
        let mut vc = v.walk();
        for lit in v.children(&mut vc) {
            match lit.kind() {
                "string" => {
                    let raw = &text[lit.start_byte()..lit.end_byte()];
                    let val = raw.trim_matches('\'');
                    if info.numeric {
                        out.push(Diagnostic {
                            range: node_range(lit, text),
                            severity: Some(DiagnosticSeverity::WARNING),
                            message: format!(
                                "`{char_name}` is a numeric characteristic (values: {}), string literal `{val}` looks wrong",
                                info.values.first().map(|s| s.as_str()).unwrap_or("?"),
                            ),
                            ..Default::default()
                        });
                    } else if info.has_values() && !info.contains_value(val) {
                        out.push(Diagnostic {
                            range: node_range(lit, text),
                            severity: Some(DiagnosticSeverity::WARNING),
                            message: format!(
                                "Value `{val}` not in `{real}` value range{} ({} values known, e.g. {})",
                                info.description.as_deref().map(|d| format!(" ({d})")).unwrap_or_default(),
                                info.values.len(),
                                info.values
                                    .iter()
                                    .take(5)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            ..Default::default()
                        });
                    }
                }
                "number" => {
                    let raw = &text[lit.start_byte()..lit.end_byte()];
                    let v: f64 = raw.parse().unwrap_or(0.0);
                    if v < 0.0 && !info.with_sign {
                        out.push(Diagnostic {
                            range: node_range(lit, text),
                            severity: Some(DiagnosticSeverity::WARNING),
                            message: format!(
                                "`{char_name}` does not accept negative values (WITH_SIGN not set)"
                            ),
                            ..Default::default()
                        });
                    }
                }
                _ => {}
            }
        }
    }
}

/// First identifier node within a subtree (the LHS characteristic of an
/// object_ref like `$SELF.CHAR` / `ET1.CHAR`).
fn first_identifier(node: tree_sitter::Node) -> Option<tree_sitter::Node> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    let mut c = node.walk();
    for ch in node.children(&mut c) {
        if let Some(found) = first_identifier(ch) {
            return Some(found);
        }
    }
    None
}

fn collect_errors(
    node: tree_sitter::Node,
    text: &str,
    out: &mut Vec<Diagnostic>,
    last_suppressed_end_line: &mut Option<usize>,
) {
    if node.is_error() || node.is_missing() {
        // KNOWN '*' LIMITATION: a comment line directly after a value-ending
        // statement/term is swallowed by the expression as a multiplication
        // continuation (`'NO'\n*3.1 screen print` parses as `'NO' * 3.1` + junk).
        // The ERROR then starts on the comment line — which is always legal in
        // reality. Suppress those, plus cascade errors on the SAME end line of
        // a suppressed region (same mis-parse, one diagnostic would be noise).
        let start_row = node.start_position().row;
        let is_star_lim = is_star_limitation(node, text)
            || last_suppressed_end_line.map(|l| l == start_row).unwrap_or(false);
        if is_star_lim {
            *last_suppressed_end_line = Some(node.end_position().row);
        } else {
            out.push(Diagnostic {
                range: node_range(node, text),
                severity: Some(DiagnosticSeverity::ERROR),
                message: format!(
                    "Syntax error{}",
                    if node.is_missing() { " (missing token)" } else { "" }
                ),
                ..Default::default()
            });
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_errors(child, text, out, last_suppressed_end_line);
    }
}

/// True when the error region starts on a line whose first non-space char is
/// '*', i.e. the tree-sitter '*' comment-vs-multiplication limitation.
fn is_star_limitation(node: tree_sitter::Node, text: &str) -> bool {
    let start = node.start_byte();
    let line_start = text[..start.min(text.len())].rfind('\n').map(|i| i + 1).unwrap_or(0);
    text[line_start..].trim_start().starts_with('*')
}

/// Convert a tree-sitter byte range to an LSP range.
fn node_range(node: tree_sitter::Node, text: &str) -> Range {
    let start = byte_to_position(node.start_byte(), text);
    let end = byte_to_position(node.end_byte(), text);
    Range { start, end }
}

fn byte_to_position(byte: usize, text: &str) -> Position {
    // Walk the text counting lines until we reach the byte offset.
    let prefix = &text[..byte.min(text.len())];
    let lines: Vec<&str> = prefix.split('\n').collect();
    let line = lines.len().saturating_sub(1);
    let character = lines.last().map(|l| l.chars().count()).unwrap_or(0);
    Position { line: line as u32, character: character as u32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_no_diagnostics() {
        let text = "$SELF.X = 1\n$SELF.Y ?= 'A'\n";
        let d = diagnostics(text);
        eprintln!("clean_text: {} diagnostics", d.len());
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn comment_after_value_is_suppressed_as_star_limitation() {
        // The '*' limitation region starts ON the comment line -> suppressed.
        let text = "$SELF.X = 1\n* comment after value\n";
        let d = diagnostics(text);
        eprintln!("comment_after_value: {} diagnostics", d.len());
        assert_eq!(d.len(), 0, "star limitation must be suppressed");
    }

    #[test]
    fn real_file_head_parses() {
        // comment after an IF clause (top-level) is fine; only a comment
        // directly after a value-ending statement hits the known '*' limitation
        let text = "$SELF.THIS_WILL_STOP = $SELF.NULL\n\
                    IF $SELF.TXT_ELE_VENDOR SPECIFIED,\n\
                    * Chinese comment 中文注释\n\
                    $SELF.TXT_ELE_VENDOR ?= 'VER_03'\n";
        let d = diagnostics(text);
        eprintln!("real_file_head: {} diagnostics", d.len());
        assert_eq!(d.len(), 0);
    }

    #[test]
    fn semantic_unknowns_reported() {
        let dir = std::env::temp_dir();
        let path = dir.join("sapvc_mat_sem.json");
        std::fs::write(
            &path,
            r#"{"material":"X","system":"R1E","chars":["KNOWN_CHAR"],"tables":["TB_KNOWN"],"objects":["ET1"],"deps":[]}"#,
        )
        .unwrap();
        let mat = crate::material::MaterialIndex::load(Some(&path));
        std::fs::remove_file(&path).ok();

        let text = "$SELF.KNOWN_CHAR = 1\n\
                    $SELF.UNKNOWN_CHAR_X = 2\n\
                    TABLE TB_NOT_IN_PKG (A = 1)\n\
                    TABLE TB_KNOWN (A = 1)\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sapvc::language())
            .unwrap();
        let tree = parser.parse(text, None).unwrap();
        let d = semantic_diagnostics(text, &tree, &mat);
        eprintln!("semantic: {} diagnostics", d.len());
        assert_eq!(d.len(), 2, "one unknown char warning + one unknown table error");
        assert!(d.iter().any(|x| x.message.contains("UNKNOWN_CHAR_X")));
        assert!(d.iter().any(|x| x.message.contains("TB_NOT_IN_PKG")));
    }

    #[test]
    fn semantic_value_range_and_type() {
        let dir = std::env::temp_dir();
        let path = dir.join("sapvc_mat_vals.json");
        std::fs::write(
            &path,
            r#"{"material":"X","system":"R1E","chars":["TYP_LDO","WGT_CAR_TOTAL"],"tables":[],"objects":[],"deps":[],
                "char_values":{
                  "TYP_LDO":{"values":["S200","S400_E"],"numeric":false,"description":"door type"},
                  "WGT_CAR_TOTAL":{"values":["100","150"],"numeric":true,"description":""}
                }}"#,
        )
        .unwrap();
        let mat = crate::material::MaterialIndex::load(Some(&path));
        std::fs::remove_file(&path).ok();

        let text = "$SELF.TYP_LDO = 'S200'\n\
                    $SELF.TYP_LDO = 'INVALID_XXX'\n\
                    $SELF.WGT_CAR_TOTAL = 'not_a_number'\n\
                    $SELF.WGT_CAR_TOTAL ?= 150\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sapvc::language())
            .unwrap();
        let tree = parser.parse(text, None).unwrap();
        let d = semantic_diagnostics(text, &tree, &mat);
        eprintln!("value/type: {} diagnostics", d.len());
        // 2 expected: INVALID_XXX out of range, 'not_a_number' on numeric char
        assert_eq!(d.len(), 2);
        assert!(d.iter().any(|x| x.message.contains("INVALID_XXX")));
        assert!(d.iter().any(|x| x.message.contains("numeric")));
    }

    #[test]
    fn semantic_where_aliases_exempted() {
        let dir = std::env::temp_dir();
        let path = dir.join("sapvc_mat_alias.json");
        std::fs::write(
            &path,
            r#"{"material":"X","system":"R1E","chars":["TXT_NON_STD_REMARK"],"tables":[],"objects":["ETO"],
                "deps":[],
                "char_values":{"TXT_NON_STD_REMARK":{"values":["YES","NO"],"numeric":false,"description":""}}}"#,
        )
        .unwrap();
        let mat = crate::material::MaterialIndex::load(Some(&path));
        std::fs::remove_file(&path).ok();

        let text = "OBJECTS:\n\
                    ETO IS_A(300) ETOPARAMETERGROUP WHERE NS = TXT_NON_STD_REMARK,\n\
                    RESTRICTIONS:\n\
                    IF NS SPECIFIED,\n\
                    $SELF.NS = 'YES'\n\
                    $SELF.NS = 'INVALID_ALIAS_VAL'\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sapvc::language())
            .unwrap();
        let tree = parser.parse(text, None).unwrap();
        let d = semantic_diagnostics(text, &tree, &mat);
        eprintln!("alias: {} diagnostics", d.len());
        for x in &d {
            eprintln!("  DIAG: {}", x.message);
        }
        // NS must not be flagged unknown; only the out-of-range value is
        // reported, resolved through the alias to TXT_NON_STD_REMARK.
        assert_eq!(d.len(), 1, "only the alias-resolved value error");
        assert!(d[0].message.contains("INVALID_ALIAS_VAL"));
        assert!(d[0].message.contains("TXT_NON_STD_REMARK"));
    }

    #[test]
    fn semantic_multiline_assignment_value_check() {
        let dir = std::env::temp_dir();
        let path = dir.join("sapvc_mat_ml.json");
        std::fs::write(
            &path,
            r#"{"material":"X","system":"R1E","chars":["TYP_LDO","DIM_CD"],"tables":[],"objects":[],
                "deps":[],
                "char_values":{"TYP_LDO":{"values":["S200","S400"],"numeric":false,"description":"厅门型号"}}}"#,
        )
        .unwrap();
        let mat = crate::material::MaterialIndex::load(Some(&path));
        std::fs::remove_file(&path).ok();

        // RHS on the NEXT line (assignment continuation) must still be checked
        let text = "$SELF.TYP_LDO =\n'INVALID_ML'\nIF $SELF.DIM_CD < 1100,\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sapvc::language())
            .unwrap();
        let tree = parser.parse(text, None).unwrap();
        let d = semantic_diagnostics(text, &tree, &mat);
        eprintln!("multiline: {} diagnostics", d.len());
        assert_eq!(d.len(), 1, "one out-of-range value on the continued line");
        assert!(d[0].message.contains("INVALID_ML"));
        assert!(d[0].message.contains("TYP_LDO"));
    }

    #[test]
    fn semantic_class_table_function_checks() {
        let dir = std::env::temp_dir();
        let path = dir.join("sapvc_mat_sem2.json");
        std::fs::write(
            &path,
            r#"{"material":"X","system":"R1E",
                "chars":["TYP_CDO"],"tables":["TB_DOOR_SILL_A1"],"objects":["ET1"],
                "deps":[],
                "classes":["CN_SPB_1"],
                "vt_chars":{"TB_DOOR_SILL_A1":{"chars":["TYP_CDO","DIM_X"],"key_fields":["TYP_CDO"]}},
                "variant_functions":["SAP_VF_CONCATENATE"]}"#,
        )
        .unwrap();
        let mat = crate::material::MaterialIndex::load(Some(&path));
        std::fs::remove_file(&path).ok();

        let text = "OBJECTS:\n\
                    ET1 IS_A(300) CN_SPB_1,\n\
                    ET2 IS_A(300) NO_SUCH_CLASS,\n\
                    RESTRICTIONS:\n\
                    TABLE TB_DOOR_SILL_A1 (TYP_CDO = 'A', BAD_COL = 'B')\n\
                    FUNCTION SAP_VF_UNKNOWN (A = 1)\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sapvc::language())
            .unwrap();
        let tree = parser.parse(text, None).unwrap();
        let d = semantic_diagnostics(text, &tree, &mat);
        for x in &d {
            eprintln!("  DIAG: {}", x.message);
        }
        // NO_SUCH_CLASS (class), BAD_COL (vt column), SAP_VF_UNKNOWN (function)
        assert_eq!(d.len(), 3, "one per new semantic check");
        assert!(d.iter().any(|x| x.message.contains("NO_SUCH_CLASS")));
        assert!(d.iter().any(|x| x.message.contains("BAD_COL")));
        assert!(d.iter().any(|x| x.message.contains("SAP_VF_UNKNOWN")));
    }

    #[test]
    fn semantic_negative_value_with_sign_check() {
        let dir = std::env::temp_dir();
        let path = dir.join("sapvc_mat_neg.json");
        std::fs::write(
            &path,
            r#"{"material":"X","system":"R1E",
                "chars":["DIM_CDO_OD","WGT_CAR_TOTAL"],"tables":[],"objects":[],
                "deps":[],
                "char_values":{
                  "DIM_CDO_OD":{"values":[],"numeric":true,"with_sign":true},
                  "WGT_CAR_TOTAL":{"values":[],"numeric":true,"with_sign":false}
                }}"#,
        )
        .unwrap();
        let mat = crate::material::MaterialIndex::load(Some(&path));
        std::fs::remove_file(&path).ok();

        let text = "RESTRICTIONS:\n\
                    $SELF.DIM_CDO_OD = -50\n\
                    $SELF.WGT_CAR_TOTAL = -50\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_sapvc::language())
            .unwrap();
        let tree = parser.parse(text, None).unwrap();
        let d = semantic_diagnostics(text, &tree, &mat);
        for x in &d {
            eprintln!("  DIAG: {}", x.message);
        }
        // only WGT_CAR_TOTAL (no WITH_SIGN) may not take -50
        assert_eq!(d.len(), 1, "one negative-value warning");
        assert!(d[0].message.contains("WGT_CAR_TOTAL"));
        assert!(d[0].message.contains("negative"));
    }
}

