# M7b and follow-on: implementation, test and debugging plan

As of 2026-10-27 (HEAD of `philipp` after DSViewR 1.0.0). It gathers every open point from
STATUS.md, PLAN.md, DEVIATIONS.md, PARITY.md, RELEASE-R1.md and the README into one ordered plan.
F-numbers refer to DEVIATIONS.md; PLAN.md's M7b section stays the summary and links here.

## 1. Open points

**Status:** everything up to and including M7a is done. Still open:

- **M7b (post-parity fixes)**, estimated at 2–4 person-weeks;
- the **release** (merge `philipp` into `master`, tag `v1.0.0`; the user decides);
- **follow-ons**: wgpu as the default renderer, Windows/macOS ports, more hardware coverage, and
  debugging tools.

### B1. Correctness bugs (highest priority; they can corrupt or lose data)

| ID | Problem | Fix | Test |
|---|---|---|---|
| F11 ✔ (done 2026-10-27) | Logic captures whose length is not a multiple of 64 display shifted and lose samples on save | Keep the overrun out of `loop_offset` in single mode; trim in `capture_ended` (`dsview-store/src/logic.rs`, `dsview-session/src/session.rs`) | Proptest over capture lengths 1..10⁵: store → save `.dsl` → reload, byte-identical; snapshot of a 100-sample capture |
| F31 ✔ (done 2026-10-27) | `update_dso_data_scale` is never called, so a V/div change after a capture stretches the trace (GUI and control API) | Call it on a PROBE_VDIV change when a capture is held (`dsview-viewmodel/src/dso.rs`, `dsview-gui/src/dsoview.rs`, `dsview-control/src/api.rs`) | Kittest: capture the demo DSO, change V/div, check the pixel height of a known amplitude; a control transcript step |
| F8 ✔ (done 2026-10-27) | Unpadded sample counts in the CSV header | Pad as documented (`dsview-formats` CSV export) | Golden update + unit test |
| F21 ✔ (done 2026-10-27) | Demo logic playback ignores disabled channels | `dsview-drivers/src/demo.rs` | Session test: disable channel 3 and check that its data is absent or zero |
| F19 ✔ (done 2026-10-27) | Analog demo spins at 100 % CPU below 2048 samples | Pace or sleep in the demo analog loop | Test that 1000 polls take ≥ the expected wall time; the `demo` fuzz target stays green |
| F25 ✔ (done 2026-10-27) | DSO channel-1 burst width is a copy-paste slip | Use the correct channel index | Unit test in `dscope.rs` |
| F28 ✔ (done 2026-10-27) | Auto-set writes the trigger level twice | Drop the duplicate write | Replay/sim transcript |

### B2. Decoder engine and decoders

1. ✔ (done 2026-10-27) **F13, fresh pin tuple per `wait()`.** Fixes `pjdl` and `ieee488` (`dsview-decode`).
   - Test: both decoders on generated captures produce annotations; add both to
     `testdata/decoders/coverage.toml`.
2. ✔ (done 2026-10-27) **F14, check of the `skip` count** (`flexray`).
3. ✔ (done 2026-10-27) **`OUTPUT_LOGIC`** for `pca9571` and `tca6408a`.
4. ✔ (done 2026-10-27) **F32 chunk-edge shift** (±1 sample; 1-sample chunks decode nothing).
   - Fix the chunk boundary handling.
   - Tighten `dsview-decode/tests/chunking.rs` from "within 1 sample" to exact equality.
5. ✔ (resolved 2026-10-27, not built: the F32 fix makes the engine chunk-independent and the
   golden is unchanged) **`Quirks::FIXED` is not implemented.**
   - Implement it.
   - List every annotation that changes under DEVIATIONS "Planned deviations".
   - Re-bless `testdata/golden/decode` deliberately, with `xtask golden bless` and a reviewed diff.
6. ✔ (done 2026-10-27; `onewire_link` and `midi` not reproduced, see DEVIATIONS F16; new
   findings in F35) **F16 Python decoder bugs:**
   - infinite loops in `gpib`, `onewire_link` and `ltc26x7`, plus unbounded memory in
     `onewire_link`;
   - a `midi` hang;
   - a `lfast` NameError;
   - `ir_irmp` needs `libirmp.so`, and its `__del__` raises.

   Patch them in the shipped decoders, add a per-decoder watchdog test (timeout and RSS cap), and
   prepare the upstream patches.

### B3. Qt-parity quirks to retire (F27/F29/F30)

✔ Done 2026-10-27, except the multi-monitor window position (F30, follow-up): see STATUS "Qt quirks
retired", parts 1–4. F24/F26 (item 6) reviewed 2026-10-27: F26's three envelope defects fixed, F24's re-open kept.

1. **Ruler and viewport:**
   - 1-2-5 ruler steps;
   - a float `x_offset`;
   - kinetic scroll without integer truncation.

   Change in `dsview-viewmodel` (ruler, viewport, input); extend the proptests in
   `viewport_props.rs`.
2. **Cursors and protocol table:**
   - stable cursor numbers and colours (`cursors.rs`);
   - a time-aligned protocol table;
   - a real TXT export instead of a copy of the CSV.
3. **FFT:** use the true π instead of `3.1415` (`dsview-dsp`); update the FFT goldens.
4. **Header and device selection (F29):**
   - Ctrl while dragging header labels;
   - check "don't ask to save" on device selection;
   - hover snapping respects the trigger.
5. **Log and window (F30):**
   - Log Options appends instead of truncating;
   - clamp the window position to a visible screen.
6. ✔ **F24/F26:** decide fix or keep for DSCope re-open and the DSO envelope quirks. Each decision is
   recorded in DEVIATIONS.

### B4. Performance decisions (M7b)

1. ✔ **Ingest:** 766 MiB/s against a gate of ≥ 1 GB/s. Done 2026-10-27: an ingest queue, pooled
   pre-faulted leaves and a blockwise mip-map; 1.1–1.3 GiB/s first capture (STATUS, "M7b B4").
2. ✔ **mmap spill** for captures larger than RAM: not built (user decision); a memory guard refuses
   such a capture with a clear message instead.
3. ✔ **`loom`/lock-free readers.** Add `loom` tests for the store's reader/writer handoff, under
   `cfg(loom)` with a CI job. *Decided 2026-10-27: not built. There are no lock-free readers; every
   handoff (store mutex, ingest `mpsc` queue, leaf pool under its mutex with a generation checked
   under the same lock) is lock- or channel-based, and loom cannot model the pool's process-wide
   statics without rewriting them. Covered instead by stress runs (STATUS "M7b B7").*
4. ✔ **`AnnotationStore`.** Keep the `DecoderStack` index unless a profile on `protocol_large` (262k
   rows) shows a need. *Kept 2026-10-27: 1.4 ms per whole-app frame with 262,142 rows shown in the
   table and the time view (release), about 40 bytes per annotation with shared texts.*
5. **Frame budget** of ≤ 8 ms at 4K × 32 channels. Run `dsview --bench` on a real GPU with glow and
   with wgpu. This is a prerequisite for B5.

### B5. Rendering and UI follow-ons

- **wgpu as the default renderer:**
  - measure with `--bench` on Intel, AMD and NVIDIA plus Mesa llvmpipe;
  - switch the default, keeping `DSVIEW_RENDERER=glow` as the fallback;
  - CI already uses Mesa for wgpu snapshots.
- ✔ **Shipped CJK font:** bundle Noto Sans CJK SC (subset) in the deb and the AppImage. `i18n_flow`
  then stops being skipped. Done 2026-10-27: an 810 KB subset built into the binary
  (`tools/fonts/make-cjk-subset.sh`), used when the system has no CJK font.
- **DSO/analog only verified on the demo/sim:** the auto-set path has unit tests only. See B6.

### B6. Hardware coverage

1. **Hardware not yet tested:**
   - a host FX2 firmware upload (original DSLogic or Pro);
   - DSCope;
   - MSO-E8;
   - USB3, 32-channel and 1 GHz paths.

   Plan: get the 14 missing `.fw` files (4 of 18 models' are in `assets/res`); ask owners for `dsviewr-cli selftest --out DIR`
   bundles; add an issue template that asks for them.
2. **Manual pcapng recording** of an unplug during capture (`cargo xtask usb record`), pinned in
   the manifest.
3. **Self-hosted `hw-dslogic` runner:** a nightly `hwmatrix --quick` + `verify-counter`.
4. **Full `hwmatrix`** (132 steps) on the packaged build; only the quick run (37/37) was done.
5. **RP2040 pattern generator** for real decode and trigger signals.

### B7. Release, packaging and CI

1. **Release:** merge, tag `v1.0.0`, publish (needs the user's decision).
2. **CI:**
   - confirm that the 24 h cumulative fuzzing was reached and record it. *Not reachable yet: the
     nightly workflow is scheduled, and GitHub runs schedules only on the default branch, so it
     has never run while `philipp` is unmerged. Recorded in STATUS; recheck after the merge.*
   - ✔ widen Miri beyond two tests: every store unit test but the length proptest (about 5 min).
     `dsview-decode` and `dsview-session` cross into CPython and libc, which Miri cannot run.
   - ✔ add a `bench-gate` job: `cargo bench -p dsview-store -- ingest` with a 500 MiB/s floor
     (`DSVIEW_BENCH_MIN_MIBS`), std timing instead of criterion/iai.
   - ✔ add a Rust-side differential `.dsl` fuzz (writer→reader round trip): fuzz target
     `dsl_roundtrip` and the proptest `dsview-formats/tests/dsl_roundtrip.rs`.
3. **Ports (follow-on):** Windows and macOS need:
   - a `localtime_r` cfg;
   - a replacement for `/proc` use;
   - a control transport other than the Unix socket (named pipe or TCP on localhost with a token);
   - installers.
4. **Test data:** an LFS or separate repository for the pcapng traces; the `.sr`→`.dsl` converter
   plus the sigrok-dumps corpus.

### B8. Documentation drift (cheap, do with M7b) ✔ done 2026-10-27

| Where | Stale entry | Fix |
|---|---|---|
| PLAN:26–30 | "41 of 52" | 52 of 52 |
| PLAN:616–618, 652–655 | R1/M7a listed as open | Mark them done |
| PLAN:80–98, 859–875 | "As built" sections predate M6f–M7a | Refresh |
| STATUS:972 | `~/DSView.log` | `~/DSViewR.log` |
| DEV, STATUS, PLAN | Firmware count of 14 vs 16 | Reconcile |
| DEV:596 | Live decoding row is stale | Update |
| PARITY:3–5 | Qt removal described as future | Present tense |
| STATUS | Moot "A/B default" entries | Mark them moot |

---

## 2. Implementation order

| Step | Work | Size |
|---|---|---|
| 1 | Bigger trigger squares in the logic header | done 2026-10-27 (STATUS) |
| 2 | B8: documentation drift | 0.5 d |
| 3 | B1: correctness bugs. F11 first (data loss), then F31, then F8, F21, F19, F25 and F28 | 4–5 d |
| 4 | B2: decoder engine fixes. F13, then F14, `OUTPUT_LOGIC`, F32; then `Quirks::FIXED` with the golden re-bless; then the F16 decoder patches | 4–5 d |
| 5 | B3: quirk retirement, one PR per group: ruler/viewport, cursors/table/TXT, FFT π, F29/F30 | 3–4 d |
| 6 | B4: benchmarks first, then the decisions | 2–3 d |
| 7 | B5: wgpu default after the GPU measurements; CJK font | 2 d |
| 8 | B7 CI hardening; B6 as hardware becomes available | ongoing |

Each step is one commit series on `philipp`. Each step updates STATUS.md (what was done, how it was
verified) and DEVIATIONS.md (decisions).

## 3. Test plan

Every fix comes with a test that fails before the fix and passes after it.

| Layer | Tool | Applies to |
|---|---|---|
| Unit/proptest | `cargo test`, `proptest` | F11 (length sweep), F8, F25, viewport/ruler (`viewport_props.rs`), FFT |
| Decoder | `dsview-decode/tests/chunking.rs` (exact), per-decoder watchdog test (timeout and RSS), `coverage.toml` | F13, F14, F16, F32, `OUTPUT_LOGIC` |
| Goldens | `cargo xtask golden check`; re-bless only for documented deviations | `Quirks::FIXED`, FFT π, CSV padding |
| GUI | `egui_kittest` snapshot and interaction (`crates/dsview-gui/tests`) | Trigger squares, F31 V/div, cursor numbering, TXT export, window clamp |
| Control/MCP | `cargo xtask control check`, `cargo xtask mcp suite` | F31 over the API; trigger set/get |
| Fuzz | the 11 cargo-fuzz targets (60 s in PR CI, 1 h nightly); new `.dsl` round trip | Formats, demo driver (F19/F21) |
| Concurrency | `loom` (new), Miri (widened) | Store handoff |
| Performance | `criterion` benches, `dsview --bench`, `soak --mode loop` RSS trend | B4, B5 |
| Hardware | `dsviewr-cli hwmatrix` (full 132), `verify-counter`, `selftest` bundles | Before the release; B6 |
| Packaging | `check-deb.sh`, `check-appimage.sh` on Ubuntu 22.04/24.04/26.04 and Fedora 44 | Release |

**Release gate:**

- the full workspace tests, golden check, control and MCP suites, and the 60 s fuzz are all green;
- the full `hwmatrix` passes on a DSLogic Plus;
- `soak` for 1 h shows no RSS trend;
- the snapshots have been reviewed.

## 4. Debugging plan

**Existing aids (use these first):**

- **Logs:**
  - log level 0–5 with `-l`, and `-s`/`--storelog` to write `~/DSViewR.log`;
  - Help → Log Options;
  - crash reports go to `$XDG_STATE_HOME/dsview/crash/`;
  - `dsviewr-cli selftest --out DIR` bundles the driver and decoder logs.
- **Headless reproduction:**
  - `dsviewr-cli serve --mcp` plus the JSON-RPC socket (`DSVIEW_MCP_SOCKET`; 31 methods) to script
    repro steps and take screenshots;
  - `dsview-mcp` (30 tools) for agent-driven repros.
- **USB:**
  - `cargo xtask usb record --iface usbmonN` (pcapng);
  - `usb-dump`, `usb-diff`, `usb-probe`;
  - replay through `dslogic_replay`.
- **Performance:** `dsview --bench SECONDS`, `soak`, `DSVIEW_RENDERER` to compare renderers.

**Per-bug method:**

1. Reproduce headless (a control transcript or CLI one-shot).
2. Turn the repro into a failing test (unit, kittest, transcript step).
3. Fix.
4. Add the test to CI.
5. Record the change in STATUS/DEVIATIONS.

For a regression relative to C behaviour, check out the tag `c-final` (`a6cf7a7d`) to run the old
oracle.

**Tools to build (follow-on, in priority order):**

1. `tracing` with `DSVIEW_LOG` filters, plus a crash ring buffer of the last N events in the crash
   report.
2. `DSVIEW_PY_FAULTHANDLER` (Python tracebacks on a decoder hang); this pairs with the F16 watchdog.
3. `dsviewr-cli diff-dsl` / `diff-ann` (compare two captures or annotation sets) and `replay`.
4. Perfetto/Tracy exporters for frame and ingest profiling (B4/B5).
5. `decode --pdb` and `DSVIEW_DEBUGPY` for decoder authors.
