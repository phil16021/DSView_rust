# Migration status

| Phase | State | Notes |
|---|---|---|
| 0 Infrastructure, oracle, baselines | **done, except two items deferred** | Deferred: USB pcapng recording (needs root once, see `tools/usb/README.md`) and performance baselines (moved to Phase 1b/2, see DEVIATIONS.md) |
| 1 Formats + store | not started | |
| 2 Decoder engine | not started | |
| 3 Drivers | not started | |
| 4 Session + control API | not started | |
| 5 Rust GUI | not started | Parity checklist: PARITY.md |
| 6 Removal + packaging | not started | |

## Phase 0 deliverables

**Workspace:** `Cargo.toml`, `rust-toolchain.toml` (1.98), `deny.toml`, `.config/nextest.toml`, and the
`cargo xtask` alias. `dsview-mcp` is now a member.

**Rust core in the C++ build:**
- `crates/dsview-ffi` exports `dsv_version()`.
- `-DDSVIEW_RUST_CORE=ON` links it into DSView, which then logs "Rust core: 0.1.0" at startup.

**C oracle** (`dev/dsview-oracle`) compiles the original libsigrok4DSL, libsigrokdecode4DSL and common
sources:
- `oracle-decode` runs the C decoder engine exactly as `DecoderStack` does and prints JSONL. On
  protocol.demo it produces 8,379 annotations, the same count DSView reports for the same file over
  the control API.
- `oracle-dsl` loads a capture through the C session driver and prints per-channel hashes. These match
  the raw `L-n/0` blocks in the file.

**Test kit, corpus and goldens:**
- `testdata/corpus.json` pins the 9 demo captures by SHA-256.
- `testdata/golden/{dsl,decode}` holds the C outputs.
- Commands: `cargo xtask golden gen|check`, `cargo xtask corpus init|verify`.

**Control-API baseline:**
- `testdata/control/demo-scenario.json` has 38 steps covering all 31 methods.
- It was recorded from the Qt DSView with `cargo xtask control record`, and is checked with
  `cargo xtask control check`.

**USB baseline tooling:**
- `cargo xtask usb record` covers 11 DSLogic scenarios.
- All of them pass on the DSLogic Plus in `--dry-run` mode.

**CI:** `.github/workflows/rust.yml` runs lint, tests, cargo-deny, golden check and a portability check.

**Removed:** the stale `DSView/test` and `libsigrok4DSL/tests`.
