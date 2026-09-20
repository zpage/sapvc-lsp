//! Cross-file reference index: where-used for characteristics / tables /
//! classes across ALL dependency & constraint sources of a material.
//!
//! Built once at server startup by scanning the material-data directory.
//! Collection is deliberately coarse — every `identifier` node (dotted tails,
//! TABLE callees, IS_A class names, bare refs) is counted; keywords (IF/AND/...)
//! are separate token types and never appear. `$SELF`-style heads are skipped.

use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RefLoc {
    pub file: String,
    pub line: usize,
}

#[derive(Debug, Default)]
pub struct ReferenceIndex {
    pub refs: HashMap<String, Vec<RefLoc>>,
}

impl ReferenceIndex {
    pub fn scan(dir: &Path) -> Self {
        let mut idx = ReferenceIndex::default();
        if !dir.is_dir() {
            return idx;
        }
        let mut files: Vec<std::path::PathBuf> = vec![];
        let mut stack: Vec<std::path::PathBuf> = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p
                        .extension()
                        .and_then(|x| x.to_str())
                        .map(|x| {
                            matches!(
                                x,
                                "vcpro" | "vcnet" | "vccons" | "vcsel" | "vcpre" | "vcfct" | "sapvc"
                            )
                        })
                        .unwrap_or(false)
                    {
                        files.push(p);
                    }
                }
            }
        }

        let mut parser = tree_sitter::Parser::new();
        if parser.set_language(&tree_sitter_sapvc::language()).is_err() {
            return idx;
        }
        for f in files {
            let Ok(text) = std::fs::read_to_string(&f) else { continue };
            let Some(tree) = parser.parse(&text, None) else { continue };
            let fname = f
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut work: Vec<tree_sitter::Node> = vec![tree.root_node()];
            while let Some(n) = work.pop() {
                if n.kind() == "identifier" {
                    let name = &text[n.start_byte()..n.end_byte()];
                    if name.starts_with('$') {
                        continue; // $SELF / $PARENT heads, not characteristics
                    }
                    let line = n.start_position().row + 1;
                    idx.refs
                        .entry(name.to_string())
                        .or_default()
                        .push(RefLoc { file: fname.clone(), line });
                }
                let mut c = n.walk();
                for ch in n.children(&mut c) {
                    work.push(ch);
                }
            }
        }
        idx
    }

    /// Cross-file where-used summary for a name: (total refs, distinct files,
    /// up to `limit` locations outside `current_file`).
    pub fn summary(
        &self,
        name: &str,
        current_file: &str,
        limit: usize,
    ) -> (usize, usize, Vec<&RefLoc>) {
        let Some(locs) = self.refs.get(name) else {
            return (0, 0, vec![]);
        };
        let total = locs.len();
        let mut files: Vec<&str> = locs.iter().map(|l| l.file.as_str()).collect();
        files.sort_unstable();
        files.dedup();
        let others: Vec<&RefLoc> = locs
            .iter()
            .filter(|l| l.file != current_file)
            .take(limit)
            .collect();
        (total, files.len(), others)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_corpus_and_summarizes() {
        let dir = std::env::temp_dir().join("sapvc_refs_test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("A.vcpro"),
            "RESTRICTIONS:\n$SELF.TYP_CDO = 'X'\n$SELF.DIM_A = 1\n",
        )
        .unwrap();
        // sub/ must exist before writing B
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/B.vccons"), "RESTRICTIONS:\nET1.TYP_CDO = 'Y'\n").unwrap();

        let idx = ReferenceIndex::scan(&dir);
        // TYP_CDO referenced in both files (dotted tails)
        let (total, nfiles, _) = idx.summary("TYP_CDO", "A.vcpro", 10);
        assert_eq!(total, 2, "two dotted references across corpus");
        assert_eq!(nfiles, 2, "both files");
        let (_, _, others) = idx.summary("TYP_CDO", "A.vcpro", 10);
        assert_eq!(others.len(), 1, "B.vccons only (current file excluded)");
        assert_eq!(others[0].file, "B.vccons");
        // DIM_A only in A
        let (t, n, _) = idx.summary("DIM_A", "B.vccons", 10);
        assert_eq!((t, n), (1, 1));
        // keywords are NOT identifiers: no entry for IF/AND
        assert!(idx.refs.get("IF").is_none() || idx.refs["IF"].is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
