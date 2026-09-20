//! Material index: per-material universe loaded from a material data package
//! (export_material.py output). Powers semantic diagnostics (unknown
//! characteristic / variant table / constraint object) and scopes completion
//! to the material actually being edited.
//!
//! JSON shape (material_<MATNR>.json):
//!   {
//!     "material": "000000008000000038",
//!     "system": "R1E",
//!     "chars":   ["VAL_MODEL_VERSION", ...],   // characteristics of the material
//!     "tables":  ["TB_DOOR_SILL_A1", ...],     // variant tables
//!     "objects": ["ET1", "M", "SPB", ...],     // constraint objects (OBJECTS:)
//!     "deps": [ {"name": "PRO_...", "type": "PROC"}, ... ]
//!   }

use std::collections::HashSet;
use std::path::Path;

/// Value information for one characteristic (from BAPI_CHARACT_GETDETAIL or
/// source inference).
#[derive(Debug, Default, Clone)]
pub struct CharValueInfo {
    pub values: Vec<String>,
    pub numeric: bool,
    pub description: Option<String>,
    /// WITH_SIGN=X: characteristic accepts negative values (R1E verified)
    pub with_sign: bool,
}

impl CharValueInfo {
    pub fn has_values(&self) -> bool {
        !self.values.is_empty()
    }

    /// Case-insensitive membership (SAP characteristic values are case-ordered
    /// but usually uppercase; be lenient).
    pub fn contains_value(&self, v: &str) -> bool {
        self.values.iter().any(|x| x.eq_ignore_ascii_case(v))
    }
}

#[derive(Debug, Default)]
pub struct MaterialIndex {
    pub material: Option<String>,
    pub system: Option<String>,
    pub chars: HashSet<String>,
    pub tables: HashSet<String>,
    pub objects: HashSet<String>,
    pub deps: Vec<String>,
    pub char_values: std::collections::HashMap<String, CharValueInfo>,
    /// IS_A(300) target classes (direct classification + BOM K-item classes)
    pub classes: HashSet<String>,
    /// variant table -> its characteristic columns (for TABLE(...) param checks)
    pub vt_chars: std::collections::HashMap<String, Vec<String>>,
    /// variant table -> key fields (VL_ASSG_NO=0001 group)
    pub vt_keys: std::collections::HashMap<String, Vec<String>>,
    /// known variant functions (FUNCTION calls in dep sources)
    pub variant_functions: HashSet<String>,
    /// characteristic -> reference-to-table info (REFERENCE_TO_TABLE)
    pub char_refs: std::collections::HashMap<String, Vec<(String, String)>>,
}

impl MaterialIndex {
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else { return Self::default() };
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!("[sapvc-lsp] material package not readable: {}", path.display());
            return Self::default();
        };
        let Ok(data) = serde_json::from_str::<MaterialFile>(&text) else {
            eprintln!("[sapvc-lsp] material package not valid JSON: {}", path.display());
            return Self::default();
        };
        let idx = Self {
            material: data.material,
            system: data.system,
            chars: data.chars.into_iter().collect(),
            tables: data.tables.into_iter().collect(),
            objects: data.objects.into_iter().collect(),
            deps: data.deps.into_iter().map(|d| d.name).collect(),
            char_values: data
                .char_values
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        CharValueInfo {
                            values: v.values,
                            numeric: v.numeric,
                            description: v.description,
                            with_sign: v.with_sign,
                        },
                    )
                })
                .collect(),
            classes: data.classes.into_iter().collect(),
            vt_chars: data
                .vt_chars
                .iter()
                .map(|(k, v)| (k.clone(), v.chars.clone()))
                .collect(),
            vt_keys: data
                .vt_chars
                .iter()
                .map(|(k, v)| (k.clone(), v.key_fields.clone()))
                .collect(),
            variant_functions: data.variant_functions.into_iter().collect(),
            char_refs: data
                .char_refs
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().map(|r| (r.table, r.field)).collect()))
                .collect(),
        };
        eprintln!(
            "[sapvc-lsp] material index loaded: {} chars, {} tables, {} objects, {} value sets, {} classes ({}), {} VT structures, {} variant funcs",
            idx.chars.len(),
            idx.tables.len(),
            idx.objects.len(),
            idx.char_values.len(),
            idx.classes.len(),
            idx.material.as_deref().unwrap_or("?"),
            idx.vt_chars.len(),
            idx.variant_functions.len(),
        );
        idx
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty() && self.tables.is_empty() && self.objects.is_empty()
    }

    /// Characteristic names matching the partial prefix, sorted.
    pub fn completions_for(&self, partial: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .chars
            .iter()
            .filter(|n| n.starts_with(partial))
            .cloned()
            .collect();
        v.sort();
        v
    }
}

#[derive(serde::Deserialize)]
struct MaterialFile {
    material: Option<String>,
    system: Option<String>,
    chars: Vec<String>,
    tables: Vec<String>,
    objects: Vec<String>,
    #[serde(default)]
    deps: Vec<DepEntry>,
    #[serde(default)]
    char_values: std::collections::HashMap<String, CharValueFile>,
    #[serde(default)]
    classes: Vec<String>,
    #[serde(default)]
    vt_chars: std::collections::HashMap<String, VtStructFile>,
    #[serde(default)]
    variant_functions: Vec<String>,
    #[serde(default)]
    char_refs: std::collections::HashMap<String, Vec<CharRefFile>>,
}

#[derive(serde::Deserialize)]
struct VtStructFile {
    #[serde(default)]
    chars: Vec<String>,
    #[serde(default)]
    key_fields: Vec<String>,
}

#[derive(serde::Deserialize)]
struct CharRefFile {
    #[serde(default)]
    table: String,
    #[serde(default)]
    field: String,
}

#[derive(serde::Deserialize)]
struct CharValueFile {
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    numeric: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    with_sign: bool,
}

#[derive(serde::Deserialize)]
struct DepEntry {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_material_package() {
        let dir = std::env::temp_dir();
        let path = dir.join("sapvc_material_test.json");
        std::fs::write(
            &path,
            r#"{"material":"X","system":"R1E","chars":["A_X","B_Y"],"tables":["TB_1"],"objects":["ET1"],"deps":[{"name":"PRO_X"}]}"#,
        )
        .unwrap();
        let idx = MaterialIndex::load(Some(&path));
        assert_eq!(idx.chars.len(), 2);
        assert_eq!(idx.completions_for("A"), vec!["A_X"]);
        assert!(idx.tables.contains("TB_1"));
        assert!(idx.objects.contains("ET1"));
        std::fs::remove_file(&path).ok();
    }
}
