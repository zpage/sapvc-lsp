//! Completion.
//!
//! v1 (offline): trigger on '.' after `$SELF` / bare object refs; candidates
//! are characteristic names collected from the CURRENT document (every
//! `.<identifier>` pattern). RFC-backed characteristic lists (via the C#
//! sapvc-cli JSON bridge) replace this in M3-5.

use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, Position,
};
use std::collections::BTreeSet;

use crate::material::MaterialIndex;

/// Return completion items for a position.
///
/// Two tiers:
/// 1. after a dot in a dotted chain (`$SELF.` / `ET1.`) -> characteristic names
///    from the loaded index ∪ names seen in this document
/// 2. anywhere else -> statement-level starters. NOTE: SAP VC's IF is a
///    POSTFIX guard (`IF <condition>,` — no THEN/ENDIF), so no block keywords
///    are ever offered.
pub fn complete(
    text: &str,
    position: Position,
    mat: &MaterialIndex,
) -> Option<CompletionResponse> {
    let offset = position_to_byte(text, position)?;
    let before = &text[..offset];

    // partial: the word (letters/digits/_/$) immediately before the cursor
    let partial: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    // Dotted-chain detection is LINE-LOCAL: only the current line's text up to
    // the cursor counts. A '.' from an earlier line (e.g. a multiplication
    // like `0.5*M.WGT_LOAD`) must NOT put us into characteristic completion.
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_before = &before[line_start..];
    let dot_idx = line_before.rfind('.');

    if let Some(d) = dot_idx {
        let chain_start = line_before[..d]
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$' || c == '.'))
            .map(|i| i + 1)
            .unwrap_or(0);
        let chain = &line_before[chain_start..d];
        // only offer characteristic names while the dotted tail is still
        // being typed — if the reference is complete (cursor after a space),
        // fall through to operator/starter completions (`IF $SELF.DIM_X ` → ops)
        let tail_word: String = line_before[d + 1..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        // typing continues only when the tail word reaches the cursor
        // (`$SELF.DI` yes; `$SELF.DIM_X ` with trailing space no → operators)
        let tail_typing = !tail_word.is_empty() && line_before.ends_with(tail_word.as_str());
        if !chain.is_empty() && (tail_typing || line_before.ends_with('.')) {
            return Some(completions_for_characteristics(text, &partial, mat));
        }
    }

    // TABLE <name>( ... — column completion inside an unclosed table call
    // (vt structure chars; key fields tagged in the detail).
    if let Some(tbl) = table_call_context(line_before) {
        return Some(table_column_completions(&tbl, &partial, mat));
    }

    // Inside an IF condition (line has IF / AND / OR, cursor after a
    // reference): offer condition operators incl. the SPECIFIED postfix.
    // (empty partial = cursor right after the reference → full operator list)
    if in_if_condition(before, line_before) {
        return Some(condition_operator_completions(&partial));
    }

    // Not inside a dotted chain: statement-level starters.
    Some(statement_completions(&partial))
}

/// If the current line is inside an unclosed `TABLE <name>(`, return the name.
fn table_call_context(line_before: &str) -> Option<String> {
    // crude but robust: TABLE keyword, an identifier, then '(' with no ')'
    // on the line after the '('
    let line = line_before;
    let kw = line.rfind("TABLE")?;
    let after = &line[kw + 5..];
    let name: String = after
        .chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        return None;
    }
    let rest = &after[after.find(&name)? + name.len()..];
    let open = rest.find('(')?;
    let after_open = &rest[open + 1..];
    // unclosed: no ')' after the '(' (line-local)
    if after_open.contains(')') {
        return None;
    }
    Some(name)
}

/// Completion items for a variant table's columns inside a TABLE call.
fn table_column_completions(
    table: &str,
    partial: &str,
    mat: &MaterialIndex,
) -> CompletionResponse {
    let mut names: BTreeSet<String> = BTreeSet::new();
    if let Some(cols) = mat.vt_chars.get(table) {
        names.extend(cols.iter().filter(|c| c.starts_with(partial)).cloned());
    }
    let keys: BTreeSet<String> = mat
        .vt_keys
        .get(table)
        .map(|k| k.iter().cloned().collect())
        .unwrap_or_default();
    let items: Vec<CompletionItem> = names
        .iter()
        .map(|n| CompletionItem {
            label: n.clone(),
            kind: Some(CompletionItemKind::FIELD),
            detail: Some(if keys.contains(n) {
                format!("{table} — key field")
            } else {
                format!("{table} — column")
            }),
            insert_text: Some(format!("{n} = ")),
            ..Default::default()
        })
        .collect();
    CompletionResponse::Array(items)
}

/// Heuristic: we are inside an IF condition when the current line carries an
/// IF/AND/OR token and the cursor follows a `$SELF.`/`OBJ.` reference tail.
fn in_if_condition(before: &str, line_before: &str) -> bool {
    let has_cond_kw = line_before
        .to_uppercase()
        .contains(" IF ")
        || line_before.to_uppercase().starts_with("IF ")
        || line_before.to_uppercase().contains(" AND ")
        || line_before.to_uppercase().contains(" OR ")
        || before.to_uppercase().contains(" IF ");
    if !has_cond_kw {
        return false;
    }
    // cursor after a reference tail: `$SELF.DIM_X` / `ET1.TXT_Y`
    line_before.rfind('.').is_some()
}

/// Operators valid inside an IF condition (incl. the SPECIFIED postfix).
fn condition_operator_completions(partial: &str) -> CompletionResponse {
    const OPS: &[(&str, &str)] = &[
        ("SPECIFIED", "postfix: <ref> SPECIFIED — value is entered"),
        ("NOT ", "negation: NOT <condition>"),
        ("IN (", "membership: <ref> IN ('A','B') / (min - max)"),
        (">=", "comparison: <ref> >= <value>"),
        ("<=", "comparison: <ref> <= <value>"),
        ("=", "comparison: <ref> = <value>"),
        ("<>", "comparison: <ref> <> <value>"),
        ("IS INVISIBLE", "postfix: <ref> IS INVISIBLE"),
        ("AND ", "condition conjunction"),
        ("OR ", "condition disjunction"),
    ];
    let items: Vec<CompletionItem> = OPS
        .iter()
        .filter(|(text, _)| text.starts_with(partial))
        .map(|(text, detail)| CompletionItem {
            label: text.to_string(),
            kind: Some(CompletionItemKind::OPERATOR),
            detail: Some(detail.to_string()),
            insert_text: Some(text.to_string()),
            ..Default::default()
        })
        .collect();
    CompletionResponse::Array(items)
}

fn completions_for_characteristics(
    text: &str,
    partial: &str,
    mat: &MaterialIndex,
) -> CompletionResponse {
    // 1. names from the loaded characteristic index (the "real" universe)
    let mut names: BTreeSet<String> = mat.completions_for(partial).into_iter().collect();

    // 2. names seen in this document (covers newly introduced characteristics)
    for line in text.lines() {
        let mut rest = line;
        while let Some(pos) = rest.find('.') {
            let after = &rest[pos + 1..];
            let name: String = after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && name.starts_with(partial) {
                names.insert(name);
            }
            rest = &rest[pos + 1..];
        }
    }

    let items: Vec<CompletionItem> = names
        .iter()
        .map(|n| CompletionItem {
            label: n.clone(),
            kind: Some(CompletionItemKind::FIELD),
            ..Default::default()
        })
        .collect();

    CompletionResponse::Array(items)
}

/// Statement-level starters. IF is a postfix guard in VC/AVC — no THEN/ENDIF,
/// no block structure, so only these opening forms are offered.
fn statement_completions(partial: &str) -> CompletionResponse {
    const STARTERS: &[(&str, &str, CompletionItemKind)] = &[
        ("$SELF.", "object variable reference", CompletionItemKind::VARIABLE),
        ("IF ", "postfix guard — `IF <condition>,` guards the preceding statement", CompletionItemKind::KEYWORD),
        ("TABLE ", "variant table call", CompletionItemKind::FUNCTION),
        ("PFUNCTION ", "custom function call", CompletionItemKind::FUNCTION),
        ("FUNCTION ", "SAP function call", CompletionItemKind::FUNCTION),
        ("$DEL_DEFAULT (", "system procedure", CompletionItemKind::FUNCTION),
        ("$SET_DEFAULT (", "system procedure", CompletionItemKind::FUNCTION),
        ("$SET_PRICING_FACTOR (", "system procedure", CompletionItemKind::FUNCTION),
        ("$COUNT_PARTS (", "system procedure", CompletionItemKind::FUNCTION),
        ("$SUM_PARTS (", "system procedure", CompletionItemKind::FUNCTION),
        ("OBJECTS:", "constraint section header", CompletionItemKind::KEYWORD),
        ("CONDITION:", "constraint section header", CompletionItemKind::KEYWORD),
        ("RESTRICTIONS:", "constraint section header", CompletionItemKind::KEYWORD),
        ("INFERENCES:", "constraint section header", CompletionItemKind::KEYWORD),
    ];

    let items: Vec<CompletionItem> = STARTERS
        .iter()
        .filter(|(text, _, _)| text.starts_with(partial))
        .map(|(text, detail, kind)| CompletionItem {
            label: text.to_string(),
            kind: Some(*kind),
            detail: Some(detail.to_string()),
            insert_text: Some(text.to_string()),
            ..Default::default()
        })
        .collect();

    CompletionResponse::Array(items)
}

/// Convert a Position to a byte offset; None if out of range.
fn position_to_byte(text: &str, position: Position) -> Option<usize> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn mat_with_table() -> MaterialIndex {
        let mut m = MaterialIndex::default();
        m.chars.insert("TYP_CDO".into());
        m.chars.insert("DIM_X".into());
        m.vt_chars.insert(
            "TB_DOOR_SILL_A1".into(),
            vec!["TYP_CDO".into(), "DIM_X".into(), "TYP_CDO_OPENING".into()],
        );
        m.vt_keys.insert("TB_DOOR_SILL_A1".into(), vec!["TYP_CDO".into()]);
        m
    }

    fn items(resp: Option<CompletionResponse>) -> Vec<String> {
        match resp {
            Some(CompletionResponse::Array(a)) => {
                a.into_iter().map(|i| i.label).collect()
            }
            _ => vec![],
        }
    }

    #[test]
    fn table_column_completion_inside_call() {
        let m = mat_with_table();
        // cursor after "TABLE TB_DOOR_SILL_A1 (TY"
        let text = "TABLE TB_DOOR_SILL_A1 (TY";
        let pos = Position { line: 0, character: text.chars().count() as u32 };
        let resp = complete(text, pos, &m);
        let labels = items(resp);
        assert!(labels.contains(&"TYP_CDO".to_string()), "key column offered");
        assert!(labels.contains(&"TYP_CDO_OPENING".to_string()));
        assert!(!labels.contains(&"DIM_X".to_string()), "partial 'TY' filters");
        // key field is marked in detail
        if let Some(CompletionResponse::Array(a)) = complete(text, pos, &m) {
            let key_item = a.iter().find(|i| i.label == "TYP_CDO").unwrap();
            assert!(key_item.detail.as_deref().unwrap_or("").contains("key field"));
        }
    }

    #[test]
    fn condition_operator_completion_after_ref() {
        let m = MaterialIndex::default();
        // inside IF: cursor after "$SELF.DIM_X "
        let text = "IF $SELF.DIM_X ";
        let pos = Position { line: 0, character: text.chars().count() as u32 };
        let resp = complete(text, pos, &m);
        let labels = items(resp);
        eprintln!("  COND OPS: {:?}", labels);
        assert!(labels.contains(&"SPECIFIED".to_string()));
        assert!(labels.contains(&"IN (".to_string()));
        assert!(labels.contains(&">=".to_string()));
        assert!(labels.contains(&"IS INVISIBLE".to_string()));
    }

    #[test]
    fn no_table_completion_after_closed_call() {
        let m = mat_with_table();
        let text = "TABLE TB_DOOR_SILL_A1 (TYP_CDO = 'A') ";
        let pos = Position { line: 0, character: text.chars().count() as u32 };
        let resp = complete(text, pos, &m);
        // closed call + trailing space: not table-column context; starters offered
        let labels = items(resp);
        assert!(!labels.contains(&"TYP_CDO".to_string()) || labels.contains(&"IF ".to_string()));
    }
}
