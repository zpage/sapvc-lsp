//! Hover: semantic information for the node under the cursor, based on the
//! tree-sitter parse tree (not just the raw word under the mouse).
//!
//! Node classification:
//! - identifier inside bare_ref / object_ref  -> characteristic/object reference
//! - identifier inside call                   -> function name (FRAC/ABS/PART_OF/...)
//! - identifier inside named_argument         -> parameter name of a TABLE/PFUNCTION call
//! - keyword-ish nodes (IF/AND/IN/SPECIFIED/...) -> control keyword
//! - string / number / comment                -> literal / comment
//! - system_call ($DEL_DEFAULT ...)           -> system procedure verb

use tower_lsp::lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind, Position, Range};

use crate::material::MaterialIndex;
use crate::refs::ReferenceIndex;

/// Hover: report what kind of node the cursor is on, with occurrence count
/// for references.
pub fn hover(
    text: &str,
    position: Position,
    mat: &MaterialIndex,
    refs: &ReferenceIndex,
    current_file: &str,
) -> Option<Hover> {
    let offset = position_to_byte(text, position)?;

    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_sapvc::language())
        .ok()?;
    let tree = parser.parse(text, None)?;

    let node = tree.root_node().descendant_for_byte_range(offset, offset)?;
    let kind = node.kind();

    let value = match kind {
        "identifier" => hover_identifier(node, text, mat, refs, current_file),
        "comment" => format!("Comment\n\n```\n{}\n```", text[node.start_byte()..node.end_byte()].trim()),
        "string" => format!("String literal\n\n`{}`", text[node.start_byte()..node.end_byte()].trim()),
        "number" => format!("Numeric literal\n\n`{}`", text[node.start_byte()..node.end_byte()].trim()),
        "IF" | "if" => "IF — guard clause; the preceding statement/condition applies only when this condition holds (trailing `,` or `.` ends the guard).".to_string(),
        "AND" | "and" => "AND — condition conjunction (multi-line chains continue while a line ends with AND/OR).".to_string(),
        "OR" | "or" => "OR — condition disjunction.".to_string(),
        "NOT" | "not" => "NOT — condition negation.".to_string(),
        "IN" | "in" => "IN — membership test against a list or range: `X IN ('A','B')` / `X IN (>2400 - 2600)`.".to_string(),
        "SPECIFIED" | "specified" => "SPECIFIED — presence test (prefix and postfix forms both occur in production code).".to_string(),
        "TABLE" | "table" | "FUNCTION" | "function" | "PFUNCTION" | "pfunction" => {
            format!("`{}` call — variant table / custom function invocation with named parameters.", kind)
        }
        "OBJECTS" | "CONDITION" | "RESTRICTIONS" | "INFERENCES" => {
            format!("`{}:` — constraint section header.", kind)
        }
        "$SET_DEFAULT" | "$DEL_DEFAULT" | "$SET_PRICING_FACTOR" | "$COUNT_PARTS" | "$SUM_PARTS" => {
            format!("`{}` — system procedure (engine builtin).", kind)
        }
        "?=" => "`?=` — conditional assignment: set the value only if not already set.".to_string(),
        "=" => "`=` — assignment / comparison equals.".to_string(),
        _ => return None, // operators, parens etc. get no hover
    };

    let (start, end) = (byte_to_position(node.start_byte(), text), byte_to_position(node.end_byte(), text));
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(Range { start, end }),
    })
}

/// identifier nodes: classify by parent (ref vs call vs parameter); append
/// value-range info and cross-file where-used for characteristic references.
fn hover_identifier(
    node: tree_sitter::Node,
    text: &str,
    mat: &MaterialIndex,
    refs: &ReferenceIndex,
    current_file: &str,
) -> String {
    let name = &text[node.start_byte()..node.end_byte()];
    let mut parent = node.parent();
    let mut grand = parent.and_then(|p| p.parent());

    // ascend: identifier -> bare_ref/object_ref -> reference -> value -> ...
    while let Some(p) = parent {
        match p.kind() {
            "bare_ref" | "object_ref" => {
                let occurrences = count_occurrences(text, name);
                let kind_label = if p.kind() == "object_ref" { "object variable reference" } else { "characteristic/object reference" };
                let dollar = if name.starts_with('$') { " (builtin object variable)" } else { "" };
                // characteristic metadata (value range / description / sign)
                let mut extra = String::new();
                if !name.starts_with('$') {
                    if let Some(info) = mat.char_values.get(name) {
                        if let Some(d) = &info.description {
                            extra.push_str(&format!("\n\n*{}*", d));
                        }
                        if info.has_values() {
                            let vals: Vec<&str> = info.values.iter().take(5).map(|s| s.as_str()).collect();
                            extra.push_str(&format!(
                                "\n\nValue range ({} values, e.g.): `{}`",
                                info.values.len(),
                                vals.join("`, `")
                            ));
                        }
                        if info.numeric {
                            extra.push_str(&format!(
                                "\nNumeric characteristic{}",
                                if info.with_sign { " — **accepts negative values**" } else { " — no negative values" }
                            ));
                        }
                    }
                    // cross-file where-used
                    let (total, nfiles, others) = refs.summary(name, current_file, 4);
                    if total > 0 {
                        extra.push_str(&format!(
                            "\n\n**Where used:** {total} refs in {nfiles} files"
                        ));
                        if !others.is_empty() {
                            let locs: Vec<String> = others
                                .iter()
                                .map(|l| format!("`{}:{}`", l.file, l.line))
                                .collect();
                            extra.push_str(&format!(" — {}", locs.join(", ")));
                        }
                    }
                }
                return format!(
                    "`{}` — {}{}\n\nOccurrences in this document: **{}**{}",
                    name, kind_label, dollar, occurrences, extra
                );
            }
            "keyword_call" => {
                // callee identifier of TABLE <name>( ... — show the table's
                // column structure (vt_chars) with key fields marked
                if let Some(cols) = mat.vt_chars.get(name) {
                    let keys = mat.vt_keys.get(name).cloned().unwrap_or_default();
                    let col_lines: Vec<String> = cols
                        .iter()
                        .map(|c| {
                            if keys.contains(c) {
                                format!("- `{c}` — **key**")
                            } else {
                                format!("- `{c}`")
                            }
                        })
                        .collect();
                    let key_hint = if keys.is_empty() {
                        String::new()
                    } else {
                        format!(
                            "\n\nKeys: `{}`",
                            keys.join("`, `")
                        )
                    };
                    return format!(
                        "`{name}` — variant table ({} columns)\n\n{}",
                        cols.len(),
                        col_lines.join("\n")
                    ) + &key_hint;
                }
                return format!("`{name}` — variant table / function call callee.");
            }
            "call" => {
                return format!("`{}` — function call (math builtin or user function).", name);
            }
            "named_argument" => {
                return format!("`{}` — parameter of a variant table / function call.", name);
            }
            "object_decl" => {
                return format!("`{}` — constraint object declaration.", name);
            }
            _ => {}
        }
        grand = parent;
        parent = parent.and_then(|p| p.parent());
    }
    format!("`{}`", name)
}

/// Count how many times `name` appears in dotted-reference positions in text.
fn count_occurrences(text: &str, name: &str) -> usize {
    text.lines()
        .map(|line| {
            line.split('.')
                .skip(1) // skip the chain head ($SELF / ET1 / ...)
                .filter(|seg| {
                    let seg = seg.trim_start();
                    seg.starts_with(name)
                        && seg[name.len()..]
                            .chars()
                            .next()
                            .map(|c| !(c.is_ascii_alphanumeric() || c == '_'))
                            .unwrap_or(true)
                })
                .count()
        })
        .sum()
}

/// Convert a Position to a byte offset; None if out of range.
pub fn position_to_byte(text: &str, position: Position) -> Option<usize> {
    let mut offset = 0usize;
    for (i, l) in text.split('\n').enumerate() {
        if i as u32 == position.line {
            let line_len = l.chars().count() as u32;
            let char_off = position.character.min(line_len);
            offset += l.chars().take(char_off as usize).map(|c| c.len_utf8()).sum::<usize>();
            return Some(offset);
        }
        offset += l.len() + 1;
    }
    None
}

fn byte_to_position(byte: usize, text: &str) -> Position {
    let prefix = &text[..byte.min(text.len())];
    let lines: Vec<&str> = prefix.split('\n').collect();
    let line = lines.len().saturating_sub(1);
    let character = lines.last().map(|l| l.chars().count()).unwrap_or(0);
    Position { line: line as u32, character: character as u32 }
}
