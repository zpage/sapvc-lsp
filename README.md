# sapvc-lsp

Language server for **SAP Variant Configuration (LO-VC / AVC) dependency source**.

Gives a real editor experience for VC dependency code: syntax highlighting,
semantic diagnostics, completion, hover, and go-to-references. Built on
[tower-lsp](https://github.com/ebkalderon/tower-lsp) and the
[`tree-sitter-sapvc`](https://github.com/zpage/tree-sitter-sapvc) grammar.

## Why

SAP VC dependencies are plain text with no tooling: no highlighting, no
validation, no navigation. A single wrong character breaks the constraint net
at runtime. This server moves that feedback into the editor.

## Features

| Capability | Detail |
|---|---|
| Diagnostics | Syntax errors from the grammar; unknown variant tables, classes and characteristics from the material package |
| Completion | Statement starters; `TABLE` column prefills from variant table keys |
| Hover | Characteristic / table / class info, with the tree-sitter node classification |
| Go to references | Where-used across every dependency and constraint of a material |
| Semantic checks | Value-range and type checks against `BAPI` characteristic metadata |

## Build

```bash
cargo build --release
# -> target/release/sapvc-lsp.exe   (Windows)
# -> target/release/sapvc-lsp       (Linux/macOS)
```

## Usage

The server speaks LSP over stdio:

```bash
sapvc-lsp --data /path/to/material_package.json
```

| Flag | Purpose |
|---|---|
| `--data <path>` | Material data package JSON. Enables semantic diagnostics, completion and hover. |
| `--chars-file <path>` | Optional characteristic index, when no full package is available. |

Without `--data` the server still reports syntax diagnostics.

## Material data package

The server loads a per-material JSON describing the universe the edited file
lives in: characteristics, variant tables, classes, objects, dependencies and
characteristic value sets. A synthetic example ships with this repo:

```
material-data/DEMO_MATERIAL/material_DEMO_MATERIAL.json
```

Real material packages derive from a live SAP system. They are **not** included:
they contain customer configuration data. Produce your own from your system's
dependency and characteristic tables.

## Editor integration

- **Zed** — see [`zed-sapvc`](https://github.com/zpage/zed-sapvc). It spawns this
  binary and reads `SAPVC_LSP_BIN` / `SAPVC_MATERIAL` from the worktree
  environment.
- Any LSP-capable editor works if it can start the binary with `--data`.

## Tests

```bash
cargo test
```

The suite covers the semantic layer: unknown-table detection, value-range and
type checks, alias handling and reference resolution, using inline JSON fixtures.

## Related

- [`tree-sitter-sapvc`](https://github.com/zpage/tree-sitter-sapvc) — the grammar
- [`sap-vc-od-graph`](https://github.com/zpage/sap-vc-od-graph) — interactive
  dependency graph viewer (three-panel HTML)

## License

MIT
