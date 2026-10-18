<!-- Approved migration plan (2026-10-17), comprehensively revised 2026-10-18 after M6e. Phase status is tracked in STATUS.md. -->

# DSView → 100% Rust: migration, test and debug plan

## Context
DSView (branch `philipp`) is about 97k lines of C/C++ plus 157 Python protocol decoders (about 55k lines):
- the Qt GUI `DSView/pv`, about 65k lines;
- the C acquisition library `libsigrok4DSL`, about 25k lines (12.6k of it USB drivers);
- the C decoder engine `libsigrokdecode4DSL`, which embeds CPython, about 7k lines;
- `common/`, about 8.5k lines.

The goal is a complete switch to Rust. Only the Python decoders stay, running on an embedded CPython.

Decisions already made:
- **Incremental strangler migration.** DSView stays shippable at every step; the Qt GUI is replaced last.
- **Hardware:** only a DSLogic Plus/Basic/U2 is available (USB 2 High-Speed, 16 channels). DSCope, the Hermelin E8, USB 3 and 32-channel devices are tested by trace replay only.
- **GUI toolkit:** chosen by analysis (see Phase 5).
- **Platforms:** Linux first, with a portable design (nusb, wgpu) so Windows/macOS can follow.

Analysis findings that shape the plan:
- **USB protocol** is small: vendor requests 0xB0/0xB1/0xB2, a 372-byte `DSL_setting` struct over bulk EP2, data on EP 0x86 with one header transfer and then LA_CROSS data. It ports almost 1:1. The C code's weak points are a racy libusb event model, stop paths with no cancellation and no timeouts, and unbounded busy-polls.
- **Decoder API:** DSView deviates from upstream sigrok (`matched` as a u64 bitmask, ann_type 3-tuples, `@hex`/int payloads, `end()`, per-channel bit-packed input). `wait()` semantics must be reproduced exactly, so golden outputs come from the **C engine**, not upstream.
- **GUI shape:** about 70% custom-painted traces. The data core (LogicSnapshot mipmap and friends) is self-contained.
- **No working tests exist today.** A differential oracle against the C code is therefore the backbone of the plan.

### State on 2026-10-18 (revision after M6e)
- **Done:** Phases 0-4 and GUI milestones M6.0-M6e.
  - The Rust GUI is the default on Linux: CMake `DSVIEW_RUST_GUI` installs it as `DSView`, and the
    Qt GUI as `DSView-qt`.
  - PARITY.md stands at 41 of 52.
- **Still open:**
  - M6f polish;
  - a hardening milestone (M6g, new);
  - one A/B release (R1, new);
  - Phase 6, which is milestone M7: removal and packaging (M7a) and the post-parity fixes (M7b);
  - the follow-on work listed at the end of §2.
- **This revision** schedules every open, deferred or "after the flip" item from STATUS, DEVIATIONS,
  PARITY and the phase gates in exactly one place: a milestone, the follow-on list, or "dropped" with
  a DEVIATIONS row.
- **Decisions taken 2026-10-18:**
  - **Linux only after Phase 6.** Windows and macOS become follow-on work. Deleting Qt also removes
    today's Windows build.
  - **srzip export is ported to Rust.** Gnuplot is dropped.
  - **Packages bundle Python** (libpython and the stdlib), as `build.yml` does for Qt today.
  - **One A/B release** (R1) ships Rust as the default everywhere, with C/Qt as the fallback. Phase
    6 deletes the C/Qt code after it.

---

## 1. Target architecture

### 1.1 Cargo workspace
Virtual root `Cargo.toml`, resolver 3, edition 2024, toolchain 1.98.

| Crate | Responsibility |
|---|---|
| `dsview-core` | WorkMode, ConfKey (= SR_CONF ids, checked against `libsigrok.h` by a test), ConfValue, ChannelInfo, TriggerSpec (simple / 16-stage / serial), Packet, errors |
| `dsview-formats` | Bounded INI, `.dsl` reader v1-3 and writer v3, `.demo`, `.dsc` (serde), decoders JSON, export csv/vcd/srzip (drop gnuplot), sigrok `.sr` → `.dsl` converter, `ParseLimits` |
| `dsview-store` | LogicStore (1-bit packed, 2^24-sample leaves, 3-level mipmap, 64-leaf roots, constant leaves elided, loop/ring mode), DsoStore and AnalogStore (envelopes), AnnotationStore (interval index + interned strings; fixes the C linear scan under a global mutex), `BlockAlloc {Heap, MmapSpill}` |
| `dsview-dsp` | realfft spectrum (replaces FFTW), math stack (11 operations), measurements |
| `dsview-decode` | PyO3 engine: loader, `#[pymodule] sigrokdecode`, instances and sessions, pure `wait_scan` core |
| `dsview-usb` | `UsbBackend`/`UsbHandle`/`BulkInStream` traits. Backends: Nusb, Recording (pcapng), Replay, Sim (device models) |
| `dsview-drivers` | Registry; demo; file replay; `dsl::{profiles, command, ezusb, fpga, setting, dslogic, dscope, calib, eeprom, header}` |
| `dsview-session` | DeviceManager (hotplug), CaptureSession state machine (single/repeat/loop/instant), ingest, DecodeScheduler |
| `dsview-control` | JSON-RPC proto types (shared with `dsview-mcp`), Unix-socket server, ControlBackend trait, path policy |
| `dsview-ffi` | `dsv_*` C ABI (glib-free, cbindgen header) for the transitional Qt GUI |
| `dsview-viewmodel` | Toolkit-free: viewport math, ruler ticks, cursors, trace and decode-row layout, measure, search, settings, i18n, theme tokens |
| `dsview-gui` | eframe/wgpu + egui_dock + egui_extras + catppuccin-egui + AccessKit (binary `dsview`) |
| `dsview-cli` | Headless tool (see §4) |
| `dsview-mcp` | Existing crate, becomes a workspace member; `bridge.rs` uses `dsview-control::proto` |

Dev crates:
- `dev/dsview-oracle`: `cc` build of the original C/C++ with ASan/UBSan; binaries `oracle-dsl`, `oracle-store`, `oracle-decode`, `oracle-drv`.
- `dev/dsview-usb-fake`: a cdylib exporting the ~30 `libusb_*` functions the C drivers use, backed by the Rust SimBackend, so C and Rust drivers talk to the **same** simulated device.
- `dev/dsview-siggen`: protocol signal generators plus seeded noise.
- `dev/dsview-testkit`: golden I/O (zstd JSONL), corpus manifest (sha256), masks, diff helpers.

Other top-level directories: `compat/` (transitional C shims), `fuzz/`, `xtask/`.

**As built (2026-10-18), where it differs from the table above:**
- **`dsview-gui`:** egui/eframe 0.37 with glow (`DSVIEW_RENDERER=wgpu` optional), `rfd` file
  dialogs and egui_kittest tests.
  - **No egui_dock or catppuccin-egui.** Docks are fixed panels, and the themes come as hand-written
    `Visuals` in M6f.
  - **No AccessKit platform adapter yet.** It needs `accesskit_winit`, which is not in the offline
    registry (M6f).
- **`dsview-ffi`:** exists only for the Qt GUI and the oracle. It is archived with the oracle in
  Phase 6.
- **Not built:**
  - `compat/`: the shims sit next to the C code (`libsigrok4DSL/ds_api_rs.c`,
    `hardware/rust/rs_driver.c`, `output/rs_output.c`, `libsigrokdecode4DSL/rs_decode.c`).
  - `dev/dsview-usb-fake`: the fake libusb is `dev/dsview-oracle/csrc/fake_libusb.c`.
  - `dev/dsview-siggen`: the generators live in `decode-diff`.
- **`dsview-formats`:**
  - `.dsc` profiles are handled by `dsview-session::store`, not by a serde model.
  - srzip export comes in M6g; gnuplot is dropped.
  - The `.sr` converter is follow-on work.
- **`dsview-cli`:** has only `serve` and `soak`. M6g adds the subcommands of §4.

### 1.2 Transition seam
GVariant, `sr_*` and `srd_*` are spread across about 50 GUI files, so rewriting them in C++ would be throwaway work. Instead:

- **Rust stays glib-free.** The crates expose a plain C ABI, `dsv_*`.
- **Thin C shims in `compat/`** implement the existing contracts on top of `dsv_*`, so the GUI is untouched:
  - `ds_api_rs.c`: the 51 `ds_*` functions the GUI uses, including GVariant config and the datafeed callback;
  - `srd_api_rs.c`: the ~20 `srd_*` functions;
  - `rs_driver.c`: `sr_dev_driver` vtables for Rust drivers.
- **Runtime A/B switching:** the C and Rust implementations are built as `.so` files with identical symbols. A generated loader (`compat/impl_loader.c`) `dlopen`s the one selected by `DSVIEW_IMPL=rust|c`, with per-subsystem overrides `DSVIEW_IMPL_{FORMATS,STORE,DECODE,ACQ,DRV_DEMO,DRV_FILE,DRV_DSLOGIC,DRV_DSCOPE}`. The active implementation is shown in About and in the `version` RPC.
  - A CMake compile-time switch is the fallback if the loader causes trouble.
  - After each default flip, the C path stays for exactly **one** release and is then deleted.
- **Snapshots:** the C++ snapshot classes get backend interfaces (`LogicStoreCpp` vs `LogicStoreRs` via FFI) and the views stay unchanged.
- **Build:** CMake imports the Rust crates through **Corrosion**. Always call `/usr/bin/cmake` (xtask reads `$CMAKE`).

**As built:**
- **Switching:** `dsview-ffi/src/switch.rs` checks `DSVIEW_IMPL*` at run time (Rust only for
  `rust`; the default is C). The C and Rust code sit in one binary; there is no dlopen loader and no
  per-subsystem `.so`.
- **Build:**
  - Instead of Corrosion, CMake uses custom targets that run `cargo build --locked`:
    `DSVIEW_RUST_CORE` (default off) links `dsview-ffi`, and `DSVIEW_RUST_GUI` (default on on Linux)
    builds and installs the Rust GUI.
  - The Rust GUI and `dsview-cli` always use the Rust code; they have no switch.
- **The "one release" rule** is carried out by release R1. It flips every subsystem's default to
  Rust inside `DSView-qt` and keeps C selectable; Phase 6 deletes C after it.

### 1.3 Key traits
```rust
trait UsbBackend { list(); open(id) -> Box<dyn UsbHandle>; watch() -> hotplug events }
trait UsbHandle  { claim(); control_in/out(setup, buf, timeout); bulk_out(ep, data, timeout);
                   bulk_in_stream(ep, xfer_size, inflight) -> Box<dyn BulkInStream>; speed() }
trait BulkInStream { next(timeout) -> Completed; cancel_all() }          // finite timeouts everywhere
trait Driver { id(); scan(&dyn UsbBackend) -> Vec<DeviceDescriptor>; open(desc, env) -> Box<dyn Device> }
trait Device { info/modes/channels; config_get/set/list(ConfKey, ch); status(); start(&TriggerSpec, Box<dyn PacketSink>) -> Acquisition }
struct Acquisition { stop() /* FORCE_RDY + grace + cancel_all */; abort(); join() }
fn wait_scan(conds, chunk, &mut WaitState, Quirks) -> WaitResult         // pure; proptest + oracle target
trait ControlBackend { async fn call(method, params) -> Result<Value, RpcError> }
```

### 1.4 Threading and memory
- **No tokio in the core crates.** They use OS threads plus crossbeam channels; tokio appears only in `dsview-control` and `dsview-mcp`.
- **Capture path:**
  - USB reader thread with 8-32 transfers in flight from a buffer pool;
  - an SPSC queue (`rtrb`) to the ingest thread;
  - LA_CROSS u64 words copied directly into each channel's tail leaf;
  - queue full → `Packet::Overflow`.
- **Store:** single writer and lock-free readers. Length is published through an `AtomicU64` (Release/Acquire), finished leaves are immutable, root vectors are append-only, and loop mode adds a generation counter. Verified with `loom`.
- **Decode:** one thread per stack, using the same coroutine handoff as C. Rust-side scans run under `py.allow_threads`. Stacked decoders run inline in `put(OUTPUT_PYTHON)`.
- **Hotplug:** a dedicated `nusb::watch_devices` thread. This removes the C bug where two threads pumped the same libusb context.
- **Huge captures:** constant leaves cost nothing; above a RAM budget the store spills to mmap in `$XDG_CACHE_HOME/dsview`. Annotations are 24-byte records with interned text.

**As built:**
- The stores sit behind a mutex, as the C++ classes do.
- There is no `rtrb` ingest queue: overflow is reported by the driver.
- There is no mmap spill, no `loom` verification and no separate `AnnotationStore`. The Rust
  `DecoderStack` has its own interval index since M6.0.
- All four are performance decisions for M7 (§2, "M7"), not parity work.

---

## 2. Phases

Each phase lists scope, seam, exit criteria and what gets deleted.

### Phase 0: Infrastructure, oracle, baselines (S, 4-5 pw, milestone M0)
- **Workspace setup:** the workspace (with `dsview-mcp` as a member), `rust-toolchain.toml`, `deny.toml` (license allowlist: MIT/Apache-2.0/BSD/ISC/Zlib/Unicode/MPL-2.0/CC0/OFL; GPL-3 compatible), `.config/nextest.toml`, and Corrosion in `CMakeLists.txt` linking an empty `dsview-ffi`.
- **`dsview-oracle`:** compiles the original C/C++ (libsigrok4DSL, libsigrokdecode4DSL, `pv/data/*snapshot.cpp` against Qt Core) and emits JSONL.
- **Test corpus:**
  - `testdata/` (committed, under 20 MB): the 9 demo files plus small synthesized captures.
  - A separate data repo (or LFS) with a sha256 manifest for sigrok-dumps-derived and large captures.
- **Baselines, recorded while C is still the reference:**
  1. **usbmon pcapng traces of your DSLogic** under the C driver. Scenario matrix: firmware/FPGA upload; buffer and stream; 3 rates; 4/8/16 channels; internal test-counter mode; edge/advanced/serial trigger; stop while waiting for trigger; stop mid-transfer; unplug.
  2. **Control-API transcripts:** all 31 JSON-RPC methods against Qt `DSView --mcp` with the demo devices. The headless setup from the MCP work is reused: offscreen Qt, isolated HOME, seeded `DSView.conf`.
  3. **Performance baselines:** `get_display_edges` on 1G samples, decode throughput per decoder, ingest rate.
- **CI** (new `.github/workflows/rust.yml` next to the existing `build.yml`):
  - `rust-lint`: fmt, clippy `-D warnings`, `cargo deny`.
  - `rust-test`: nextest; ubuntu 22.04/24.04 × Python 3.10/3.12.
  - `rust-portability`: `cargo check` on Windows and macOS.
  - `oracle-diff`: with ASan.
  - `fuzz-smoke`: 60 s per target on PRs; 1 h nightly.
  - `bench-gate`: iai-callgrind on PRs; criterion nightly.
  - `cpp-build`: the existing deb/AppImage build, now linking the Rust libraries.
  - `e2e-mcp`: offscreen DSView plus scripted `dsview-mcp` scenarios.
  - `hw-dslogic`: self-hosted runner with the DSLogic attached; nightly and on demand.
- **Exit:** oracle builds and runs in CI; baselines committed; Corrosion links; `cpp-build` green.
- **Deleted:** the stale `DSView/test` and `libsigrok4DSL/tests`.
- **Status 2026-10-18: done.**
  - **CI that exists** (`rust.yml`):
    - lint, tests and cargo-deny;
    - golden check, `decode-diff`, `drv-diff` and `trigger_diff`;
    - fuzz smoke tests (60 s per target on every push);
    - `oracle-asan`, store diffs only;
    - a portability job that only runs `cargo check -p dsview-ffi`;
    - `cpp-with-rust-core`.
  - **Missing CI:**
    - the nightly fuzz run, `e2e-mcp` and the Python 3.10/3.12 matrix: M6g;
    - Rust in `build.yml`: R1;
    - `bench-gate` and `hw-dslogic`: follow-on.
  - **Performance baselines:** `display_edges` (256M samples) and the aggregate decode rate were
    measured in 1b/2. Per-decoder throughput and a 1G-sample run are follow-on.
  - **Hardware traces not recorded:** firmware upload (follow-on), the test-counter trace under the C
    driver, and advanced/serial trigger traces (M6g).

### Phase 1: Formats + store (M, 8-11 pw, milestones M1/M2)
- **1a `dsview-formats`:**
  - `.dsl` v1-3 read, v3 byte-compatible write; `.demo`; `.dsc`; csv/vcd/srzip export.
  - The zip layer has hard `ParseLimits`: entries, sizes, compression ratio, channel and probe bounds, header ≤ 64 KiB, no repeated header sections. This restates the hardening from commit 5224e047.
  - Seam: `session_driver.c` and `storesession.cpp` call `dsv_dsl_*` / `dsv_export_*`.
  - Deleted: `ZipMaker.cpp`, the `.dsl`/export bodies in `storesession.cpp`, `libsigrok4DSL/output/*`, `input/*` (dead code), and the parser half of `session_driver.c`.
- **1b `dsview-store`:**
  - FFI covers `append_cross`, `get_samples` (stable pointer plus release token), `get_sample`, `get_display_edges`, next/prev edge, pattern search, loop mode, and the DSO/analog envelopes.
  - Seam: backend interfaces inside the existing snapshot classes.
- **Exit criteria:**
  - Round trips are byte-identical (Rust ↔ C) for every corpus file.
  - 0 differences in `oracle-store` over 10⁴ proptest scenarios.
  - 24 h of cumulative fuzzing with no findings.
  - `display_edges` ≤ 1.1× the C time; ingest ≥ 1 GB/s per core.
  - Rust is the default in a release.
- **Status 2026-10-18: done**, behind `DSVIEW_IMPL_FORMATS` / `_STORE`. What remains:
  - the default flip in R1; the deletions in Phase 6;
  - srzip export: M6g;
  - 24 h of cumulative fuzzing: M6g. So far 2.3M + 145k runs, and the stores 2 × 20 min;
  - the ingest gate (766 MiB/s measured), mmap spill, `loom`, and fixes F8/F11 once Rust is the
    default: M7.

### Phase 2: Decoder engine (L, 10-14 pw, milestone M3)
- **`dsview-decode`:** PyO3 with embedded, versioned libpython (≥ 3.10).
  - Mixed Python setups: the system `python3-embed` is 3.14 while pyenv provides 3.12. Pin one interpreter per build and test both in CI.
  - The loader mirrors `decoder.c` validation (api_version 3, required attributes, module names like `0-i2c` and `smart qspi`).
  - The `sigrokdecode` module provides: `Decoder` as `#[pyclass(subclass)]` with `put`/`register`/`wait`/`has_channel`/`printlog`; OUTPUT_ANN/PYTHON/BINARY/META; SRD_CONF_SAMPLERATE.
  - DSView extensions: `matched` as a u64 bitmask; ann_type 3-tuples and the counter starting at 7; int payloads → `str_number_hex`/`numberic_value`; `@hex`; `{$}`; `end()`/`last_samplenum`; "returning from `decode()` = error"; options/channels with `idn`/`type`.
  - Input: one bit-packed LSB-first buffer per decoder channel, with NULL + const for constant chunks; chunks ≤ 16384 samples aligned to leaves; decoding may start at a nonzero sample.
  - Unused optional channels read 0xff.
  - **Fresh pin tuple per `wait()`.** This avoids the C tuple-aliasing quirk, listed as a documented deviation.
  - Termination is a custom `BaseException` raised in `wait()`, so it is not swallowed by `except Exception`.
- **`wait()` semantics:** first a line-referenced transliteration of `instance.c:762-1054` and `type_decoder.c:799-1165`. That covers sample-0 seeding, `skip` (including `skip:0`), `matched` over all conditions, and the constant-chunk skip shortcut **with its inaccuracies** under `Quirks::C_COMPAT` (the default). Optimize only after the differential tests are green; `Quirks::FIXED` is opt-in.
  - Optional: add `OUTPUT_LOGIC` so that `pca9571` and `tca6408a` stop failing.
- **Security:**
  - Keep the 3 decoder patches.
  - Decoder options loaded from files are untrusted: path/exec options (`objdump`, `elffile`, `mapping`) are reset to defaults until the user confirms them.
  - Set `PYTHONDONTWRITEBYTECODE=1`.
- **Seam:** `compat/srd_api_rs.c` builds `srd_decoder`/GSList metadata once and translates output callbacks into `srd_proto_data`.
- **Exit criteria:**
  - All 157 decoders produce output identical to C (annotations, binary, meta, exception text) on the corpus and on 10³ seeded noise runs each, with identical random chunking.
  - The protocol.demo stacks match their goldens.
  - Throughput ≥ 0.9× C.
  - Rust is the default.
- **Deleted:** `libsigrokdecode4DSL/*.c`.
- **Status 2026-10-18: done** (`DSVIEW_IMPL_DECODE`). What remains:
  - the default flip in R1; deleting `libsigrokdecode4DSL/*.c` in Phase 6;
  - resetting the path/exec decoder options from files: M6g. It was moved to the GUI work and is not
    built yet;
  - the chunking-invariance suite: M6g;
  - the fresh pin tuple (F13), the skip-count check (F14), `OUTPUT_LOGIC`, `Quirks::FIXED`, the
    decoder bugs of F16 and an `AnnotationStore` in `dsview-store`: M7;
  - the sigrok-dumps corpus and `testdata/decoders/coverage.toml`: follow-on.

### Phase 3: Drivers (L, 14-19 pw, milestone M4)
- **3a** Demo driver: `.demo` playback packet-identical to C; random mode is seeded and checked structurally only.
- **3b** File-replay driver on top of `dsview-formats`.
- **3c `dsview-usb`:**
  - NusbBackend (nusb 0.2), with an optional `rusb` backend behind a feature as a fallback.
  - Recording backend (`DSVIEW_USB_RECORD=x.pcapng`).
  - Replay backend: strict mode, plus a lenient mode that masks the C code's uninitialized header-offset bytes.
  - Sim backend: `DsLogicSim` (FX2 0xA0 upload, the 0xB0/0xB1/0xB2 protocol, FPGA registers, PROG_B/INIT_B/DONE, `DSL_setting` parser, header + LA_CROSS data, test-counter mode), `DsCopeSim` (DSO frames + status tail, EEPROM, zero/tune), `E8Sim`.
- **3d DSLogic:**
  - About 13 PIDs, matched by VID 0x2A0E + PID + speed + string descriptors.
  - FX2/FPGA upload with **bounded** polls.
  - `DSL_setting` (372 B) / `ext32` (204 B) encoders written with explicit little-endian byte order.
  - Header check `0x55555555`.
  - Stop = FORCE_RDY → grace period → `cancel_all`, with finite timeouts.
  - Hotplug including the 500 ms reconnect merge.
- **3e DSCope + E8:**
  - DSO frames and `get_measure` parsed by byte offset.
  - Calibration as explicit `ZeroCalState`/`TuneState` instead of the C function-statics.
  - The EEPROM 30/32-byte overflow is **deliberately fixed**.
  - The E8 vdiv 218/182 mismatch and its NULL bitstream name are handled.
- **Seam:** `compat/rs_driver.c` registers each Rust driver next to its C counterpart; `DSVIEW_IMPL_DRV_*` selects which one runs.
- **Exit criteria:**
  - Under `dsview-usb-fake`, C and Rust send byte-identical command/arm streams in every config scenario (masks listed in `masks.toml`).
  - The Phase 0 traces replay green.
  - The DSLogic hardware matrix (§3) passes 3 nights in a row.
  - 1000 start/stop cycles without a hang.
  - DSCope/E8 Rust is made the default **only after one external hardware confirmation** (via `dsview-cli selftest` bundles); until then their C drivers stay the default.
- **Deleted:** `hardware/demo`, `hardware/DSL/*` (DSCope part only after confirmation), `ezusb.c`, and `session_driver.c`.
- **Status 2026-10-18: done except the hardware steps.**
  - **M6g:**
    - the DSLogic matrix of §3 item 6 (trigger at 0/50/100 %, the test counter at every rate and
      channel count in buffer and stream, a 10 min stream, 1000 cycles);
    - `dsview-cli selftest`, so external testers can confirm DSCope/E8;
    - a clear error when firmware is missing. 16 of the FX2 `.fw` files that `dsl/tables.rs` names
      are not in `DSView/res` (upstream removed them), among them `DSLogicPlus.fw`.
  - **Follow-on:**
    - the host firmware upload, which needs a device without firmware in flash, and the files;
    - three nights in a row, which need a self-hosted runner;
    - the DSCope/E8 confirmation.
  - **As built, not as planned:** the masks live in code (F22) instead of a `masks.toml`, and there
    is no `rusb` fallback. Neither is planned any more.
  - **M7:** fixes F19 and F21 once Rust is the default.
  - **Phase 6 caveat:** it removes the C fallback for DSCope/E8 even without a confirmation (§5).

### Phase 4: Session + control API (M, 6-9 pw, milestone M5)
- **`dsview-session`:**
  - CaptureSession states: Idle → Arming → WaitTrigger → Running → Stopping → Done/Error, with single/repeat/loop/instant, double buffers and the repeat timer.
  - DeviceManager; DecodeScheduler with cancellation.
- **Seam:** `compat/ds_api_rs.c` implements all 51 `ds_*` functions, including the global `ds_trigger_*`. Qt `SigSession` stays as it is.
- **`dsview-control`:** same socket rules as `ControlServer::default_socket_path` (including `DSVIEW_MCP_SOCKET` and the 107-byte path limit). `dsview-cli serve --mcp` gives a GUI-less DSView that `dsview-mcp` can drive.
- **Exit criteria:**
  - The Qt GUI runs entirely on Rust acquisition.
  - The Phase 0 control transcripts pass against both Qt and `dsview-cli serve`.
  - 1 h loop-mode soak with RSS growth under 1%.
- **Deleted:** the rest of libsigrok4DSL and the libusb dependency.
- **Status 2026-10-18: done.** What remains:
  - **M6g:**
    - the `dsview-cli` subcommands from §4;
    - the hardware data scale in `measure_dso`/`read_waveform` (DEVIATIONS, 4b rows);
    - a 1000-cycle start/stop soak.
  - **Phase 6:** the deletions.

### Phase 5: Rust GUI (XL, 35-46 pw, milestones M6.0, M6a-g)
*Revised 2026-10-18 after Phase 4b, from a detailed analysis of `DSView/pv`, an egui rendering
spike on the development VM, and the Rust session as it now stands.*

#### 5.0 What the analysis found

**Size.** `DSView/pv` is 65k lines (.cpp + .h). What the Rust GUI has to replace, by area:

| Area | C++ LOC | Portable logic (→ `dsview-viewmodel`/`dsview-dsp`) | Paint/widget code (→ `dsview-gui`) |
|---|---|---|---|
| Trace view: view, viewport, ruler, header, logic and decode traces, cursors | ~9.0k | ~3.0k: index↔pixel, zoom/offset, layout, ruler ticks, edge→segment, decode-row culling and colours, cursors, interaction state machine, mouse measure, kinetic scroll | ~6.0k, much of it popups and QPainter detail; far less in egui |
| DSO, analog, math, spectrum, lissajous, ref waves, histogram, channel measure, calibration | ~11k | ~2.5k: FFT windows and magnitudes, 11 math operations, envelopes (DSO/math 256×10, analog 16×), edge detect and measurements, auto-set, dial tables, voltage mappings | ~3.0-3.5k: traces, grids, dials, persistence, plots |
| Docks, toolbars, dialogs, prop binders, config, i18n, mainwindow/mainframe | ~21k | ~1.5k form model (device, probe and decoder options from `config_list`), ~1k settings, i18n and profiles | ~6-8k |
| Session, stores, decoder stack, store session, control API | ~10k | **done in Phases 1-4** | — |

Estimated Rust: about 20-25k lines plus tests. That is consistent with the original XL estimate.

**Rendering on this machine.** Development and daily use here is a Hyper-V VM over xrdp. There is no
GPU, only Mesa llvmpipe (GL) and lavapipe (Vulkan).
- The egui spike (`egui` 0.37, `egui_kittest` with wgpu, off-screen) paints 32 logic traces × 4,000
  edges at 3840×2160 in **0.8 ms of CPU per frame**: layout and tessellation.
- Rasterising on lavapipe, including the read-back, costs **110 ms per frame at 1920×1080** and
  1.1-1.9 s at 4K.
- So the plan's "≤ 4 ms per frame at 4K × 32 channels" can only be a CPU budget here. The GPU
  budget needs real hardware.
- A software-rendered desktop must stay usable: the Qt GUI draws with its CPU raster engine and
  does.

**Defects found that affect the plan** (details in DEVIATIONS.md F26 once fixed):
- **Rust DSO store has no envelope.** Phase 1b left out the DSO envelope as "never called". It is
  called: `DsoSignal::paint_mid` (`dsosignal.cpp:874-884`) switches to `paint_envelope` at
  ≥ 256 samples per pixel.
  - With `DSVIEW_IMPL_STORE=rust`, a deep DSO frame (more than ~256k samples on a 1000 px view)
    draws nothing when zoomed out.
  - The demo oscilloscope (10k samples) never reaches that point, so no test saw it.
- **Annotation lookup is a linear scan.** `RowData::get_annotation_subset` scans every annotation
  under a mutex on every repaint. The Rust `DecoderStack::annotations` copies that. A GUI needs an
  index.
- **The Qt view paints with side effects.**
  - `check_update()` and DSO auto-set run inside paint.
  - Decoder headings lag one frame.
  - Logic mode has no cache, so every mouse move walks all channels again.
  - The Rust GUI separates update from paint.
- **Quirks to keep for parity, then fix in M7:**
  - The ruler's tick steps are not 1-2-5 (`ScaleUnits` is unused).
  - `x_offset` is an integer pixel count.
  - Kinetic scroll stalls because of int truncation.
  - Cursor colours and numbers shift when a cursor is deleted.
  - The protocol table puts annotation *n* of each row in one table row, not time-aligned.
  - The protocol export's TXT is the same as its CSV.
  - The FFT uses `PI = 3.1415`.

**What the Rust session already gives the GUI:**
- devices, modes, the sampling bar, the simple trigger;
- single/repeat/loop/instant, double buffers;
- the stores with `get_display_edges`, edge and pattern search;
- decoder stacks, profiles, save/export, and the control API (`dsview-control::api`, all 31 methods).

**What it lacks for a GUI:**
- change notifications instead of polling;
- live decoding during a stream capture (C decodes while capturing; 4b decodes at the end);
- annotation queries by range with an index;
- the advanced and serial trigger;
- the DSO trigger settings;
- auto calibration;
- math/spectrum/ref-wave state;
- per-view hooks for `show_region`/`set_cursors` and the "dialog is open" refusal of mutating
  control calls.

#### 5.1 Architecture

| Crate | Content |
|---|---|
| `dsview-viewmodel` (new, no UI toolkit) | `viewport`: scale and offset as f64 in sample space, zoom around a point, limits, `show_region`, auto-scroll. `ruler`: tick algorithm (Qt's first), time/sample formatting. `layout`: trace stacking, heights, view_index order, v-snap, DSO split. `logic`: `get_display_edges` → line segments, sample dots. `decode`: row enumeration, detail gate, colours, culling, text fit through a `TextMeasure` trait. `cursors`. `measure`: mouse measure, edge counts, cursor deltas. `input`: wheel/drag/pinch/kinetic/rubber-band state machine, shortcuts with the 200 ms debounce. `forms`: option forms generated from `config_list` (device, probe, decoder). `trigger`: advanced/serial model → `Trigger`. `settings`: AppConfig model, Qt `DSView.conf` (INI) import. `i18n`: `lang/*/*.json` with `L_S(page, id, default)`. `theme`: tokens for dark/light/frappé/latte |
| `dsview-dsp` (new) | FFT (realfft) with the five windows and Qt's scaling, math operations, envelopes, DSO edge detection and measurements (moved from `dsview-control::measure`), auto-set, histogram/jitter, channel delay/skew/phase |
| `dsview-session` (extended) | event channel (`SessionEvent`: device, capture started/ended, data appended, decode progress); live decoding while streaming; advanced/serial trigger; DSO trigger keys; calibration calls; math/spectrum/ref state; annotation interval index |
| `dsview-gui` (new, binary `dsview`) | eframe app: panels and docks (egui_dock), trace painters on the viewmodel, dialogs as non-blocking modal windows, `rfd` file dialogs, egui_extras SVG icons and virtual tables, the control API hosted through `dsview-control::api` plus a `ViewHooks` implementation |

Rules:
- The GUI thread never takes a store lock longer than one frame's queries.
- Painting reads; updates happen before painting.
- Every algorithm with a C++ original gets a differential or golden test from the oracle, or a
  port-exact unit test when the C++ cannot run in isolation (the ruler, layout and measure come
  from Qt widgets).

**Rendering**, decided in M6.0 by `dsview --bench` in the real app on this VM. The window is
1920×1080 on the xrdp desktop; the protocol.demo capture is panned continuously.

| Configuration | Median frame | Frames/s |
|---|---|---|
| empty window (clear + present only), glow | 57-78 ms | 13-17 |
| 16 traces as anti-aliased line segments (egui default), glow | 162 ms | 6.2 |
| the same, wgpu (lavapipe) | 171 ms | 5.9 |
| **16 traces as one mesh of 1 px rectangles, feathering off, glow** | **77 ms** | **13** |
| the same, wgpu | 117 ms | 8.6 |

The egui CPU work stays under 1 ms per frame (p95 6-13 ms with the first frames).

**Decisions:**
- eframe with **glow** by default; `DSVIEW_RENDERER=wgpu` selects wgpu.
- Logic traces are drawn as axis-aligned rectangle meshes without feathering. They are horizontal
  and vertical lines only, so they need no anti-aliasing.
- Anti-aliasing stays for DSO, analog and decoder shapes, which are few.
- The floor is set by presenting the whole window over xrdp in software (about 60 ms at 1080p,
  25 ms at 960×540), not by drawing. Redrawing only on new data or input matters more than the
  painter.
- **Gate (M6a):** the trace drawing adds ≤ 25 ms to the empty-window frame at 1080p on this VM.
  The CPU per frame stays ≤ 4 ms at 4K × 32 channels. A CPU raster fallback is not needed.

#### 5.2 Milestones (revised)

| Milestone | Content | Exit | Estimate |
|---|---|---|---|
| **M6.0 prerequisites** | DSO envelope in `dsview-store` (256×10 mip-map, differential against the C++ class); annotation interval index; `SessionEvent` channel; `ViewHooks` for the control API; `dsview-viewmodel` and `dsview-dsp` skeletons; renderer spike in-app on this VM (glow vs wgpu, frame-time log) | envelope diff test green; Qt DSO trace drawn at ≥ 256 spp with `DSVIEW_IMPL_STORE=rust`; rendering gate decided | 2-3 pw |
| **M6a viewer** | open .dsl/.demo; logic traces; ruler; header (rename, colour, drag reorder, trigger squares); zoom/pan/kinetic/rubber-band; cursors (add, drag with edge snap, labels); mouse measure and edge-count clicks; control API hosted natively (all methods, the view ones through `ViewHooks`) | `testdata/control/demo-scenario.json` passes against `dsview`; egui_kittest snapshots of the protocol.demo view; viewmodel unit tests against port-exact expectations for the ruler, layout and measure | 6-8 pw |
| **M6b logic capture** | device selector, sampling bar (rate/depth lists already in the session), channel enable, simple trigger, single/repeat/loop/instant, auto-scroll and 30 Hz refresh while streaming, hot-plug prompts, profiles | DSLogic matrix via MCP scenarios against `dsview`; 1 h loop soak with the GUI open | 5-6 pw |
| **M6c decoders** | decoder picker (search, stacked decoders pull their bases), runtime option forms with channel binding and start/end cursors, decode traces (rows, colours, text fit, progress, errors, region), live decoding, protocol dock (virtual table, search with dash sequences and match count, jump, export) | the 8,379 demo annotations identical to the C GUI through `get_annotations` (as in 4b); protocol table and export compared with Qt's on protocol.demo; 100k-row table interactive | 6-8 pw |
| **M6d DSO/analog** | DSO traces (polyline, column bars, envelope), dials, split layout, zero/trigger-level drag, probe factor, hover value, X/Y cursors, auto-set, persistence; DSO trigger dock; measurements panel; analog traces and mapping; FFT, math, lissajous, ref waves, histogram, channel measure | `dsview-dsp` against the C++ (FFT magnitudes ≤ 1e-9 relative with `PI = 3.1415` reproduced; math, envelopes and edge detection exact); DSO transcript steps value-identical | 8-10 pw |
| **M6e docks/dialogs** | advanced 16-stage and serial trigger (validated mask inputs, Qt's exact `ds_trigger_*` sequence), measure and search docks, device options generated from `config_list` (with mode polling), calibration (manual and auto), store progress with cursor range, protocol export/list, app options, about/messages | full MCP suite green against `dsview`; trigger commits byte-identical to the Qt GUI's `ds_trigger` for a scenario set → **Rust GUI default** | 6-8 pw |
| **M6f polish** | settings and first-run import of `DSView.conf`; log file and log options; help/logo menu and About; shortcuts with debounce; docks per mode, start-up document and driver hint; i18n (en/de/cn, runtime switch, CJK font); four themes with SVG icons; AccessKit (detail in 5.5) | PARITY.md 52/52 | 3-4 pw |
| **M6g hardening** | bundled-Python start-up, firmware errors, decoder option reset, DSO data scale, one version source, data dir, crash reports; srzip export; `dsview-cli` subcommands and `selftest`; the DSLogic matrix; CI gaps (detail in 5.6) | the DSLogic matrix recorded green; `e2e-mcp` and the nightly fuzz job green; srzip byte-identical to C | 3-4 pw |

The total stays in the original 35-46 pw. M6.0-M6e are done (STATUS.md). M6g is new; it gathers
the work that must be done before release R1.

**As built:** M6e did not use egui_dock (fixed panels) or egui_extras SVG icons (no icons yet);
both are M6f items below, done without those crates.

**Parallelism:**
- M6.0's store fix can go first, on its own.
- `dsview-dsp` (M6d's computation) can be built alongside M6a-c.

**Order of work inside each milestone:** viewmodel and tests first, then painting, then the MCP
and snapshot exits.

#### 5.3 Testing the GUI
- **viewmodel:** unit tests with port-exact expectations taken from the Qt code paths; proptests
  for index↔pixel round trips and zoom invariants.
- **dsp:** differential tests against `spectrumstack.cpp`/`mathstack.cpp` built in the oracle
  (FFTW from the system), as Phase 1b did for the stores.
- **Snapshots:** `egui_kittest`, kept small (≤ 1280×720, one per scenario) because lavapipe needs
  ~0.1 s per 1080p frame here; AccessKit queries drive clicks and drags.
- **End to end:** the control transcript and MCP scenarios against `dsview --mcp` exactly as against
  Qt and `dsview-cli serve`; the C GUI stays the reference until M6e.

#### 5.4 Risks specific to Phase 5

| Risk | Mitigation |
|---|---|
| Software rendering (VM/xrdp) too slow | glow default, small meshes, partial repaint (only on new data or input), CPU raster fallback for traces |
| Protocol table with millions of rows | egui_extras virtual rows over the annotation index; filtering in a background thread with progress, as Qt does above 100k rows |
| Masked inputs (trigger values) and modal flows (Qt `exec()`) | a validated text widget; dialogs as state machines that never block the frame |
| Docking/window state | egui_dock layouts per work mode; Qt's `windowState` blob is not imported (documented) |
| Frameless window, blurred shadows, QSS themes | native decorations; themes rebuilt as egui visuals from the four palettes |
| CJK font size | Noto Sans CJK subset (OFL) loaded only for the Chinese UI |

The parity checklist is PARITY.md (extended 2026-10-18 with features the first inventory missed).
No new features are added before M7.

#### 5.5 M6f in detail (offline items first) — done 2026-10-19
The analysis behind these steps was done 2026-10-18: Qt sources, the strings, and the crates in the
offline registry.

1. **Settings.**
   - **Model:** a settings model in `dsview-viewmodel` with Qt's groups: `[Application]` (all
     `appOptions`, including log level/file/append, `transDecoderDlg`, trace height factor, DSO split),
     `[MainFrame]` (style, language, window geometry, docks per mode) and `[UserHistory]`
     (directories, export format, `showDocuments`, `showDriverHint`).
   - **Storage:** JSON next to `dsview-rs.json`.
   - **First-run import of `~/.config/DreamSourceLab/DSView.conf`:**
     - a small hand-written QSettings INI reader (no INI crate is cached): groups, `a\b` sub-keys,
       quoted values, `true`/`false`;
     - the clamping of `appconfig.cpp`;
     - Qt's `windowState` blob is not imported.
   - **Window geometry:** through `ViewportBuilder`. eframe's `persistence` feature needs ron 0.12.2,
     which is not cached.
   - **History:** `rfd` dialogs start in the directories from the history.
2. **Log.**
   - **File sink:** behind `dsview_drivers::sys::set_log_sink`, writing `~/DSView.log` (`DSView: <msg>`,
     levels 0-5, append or overwrite, clear).
   - **Log Options** dialog.
   - **Command line:**
     - `-s/--storelog`;
     - the configured level wins when it is higher;
     - "Only one file can be opened" for a second file.
3. **Help menu (Qt's logo bar):**
   - About, with the version, links and thanks;
   - Manual: `ug<lang>.pdf`, falling back to `ug31`;
   - Bug Report and Update links;
   - Language;
   - Log Options;
   - all opened through the cached `open` crate.
   - One message/confirm helper replaces the ad-hoc windows.
4. **Shortcuts.**
   - The key table, the 200 ms debounce and the Ctrl/Shift/Alt filter move to
     `dsview-viewmodel/src/input.rs`.
   - New keys:
     - O: device options;
     - 0/1: the active DSO dial;
     - ↑/↓: V/div, through the existing `step_vdiv`;
     - Esc: closes every dialog.
   - T/D/M/R only for the docks the mode has (`TrigBar::reload`).
5. **Docks and start-up.**
   - Docks per work mode, saved per mode: logic has trigger/decode/measure/search, analog only
     measure, DSO trigger and measure.
   - The document dialog at start-up (`showDoc<lang>.png`, "Not show again").
   - The driver hint: the udev rule paths, the bundled copy, and `UsbError::Access`. Phase 6's
     device-access hint reuses it.
6. **i18n.**
   - **Lookup:** a `Lang` store over `lang/<cn|en|de>/*.json` (pages toolbar, msg, dlg, dsl, dec)
     with `tr(page, id, default)`, Qt's `L_S` lookup rules (German falls back to English for the DSL
     and decoder pages), and the locale taken from `LANG`.
   - **Strings:** about 200 GUI strings move onto the existing `IDS_*` ids; device option and label
     texts and decoder `idn` are looked up too.
   - **Switching:** at run time, together with the per-language profile names and manuals.
   - **CJK:** egui's fonts have no CJK glyphs. A fallback face is loaded only for Chinese: the
     system's Noto Sans CJK (TTC index) or Droid Sans Fallback. A shipped OFL subset font follows with
     the packaging (Phase 6).
7. **Themes and icons.**
   - Four `Visuals` from the qss palettes: dark `#1F1F1F`, light `#F8F8F8`, frappé `#303446`, latte
     `#eff1f5`, with accent and text colours.
   - A `Palette` replaces the about 116 hard-coded colours in the views.
   - The theme choice goes in the Display menu.
   - SVG icons (`DSView/icons/{dark,light}`, 53 each, plus flags) are rasterised with resvg/usvg 0.44
     directly. egui_extras' `svg` feature needs resvg 0.45, which is not cached.
8. **AccessKit.**
   - The kittest accessibility check runs over every dialog and window (offline).
   - Then eframe's `accesskit` feature is enabled. That needs one network fetch of
     `accesskit_winit` (with `atspi`/`zbus`), asked for at that point.

**Exit:** PARITY.md 52/52; `tests/app_flow.rs` cases for settings import, shortcuts, docks per mode
and the language switch; snapshots per theme and for Chinese.

#### 5.6 M6g hardening (before release R1)
1. **Runtime.**
   - **Bundled Python:** `dsview-decode::init` sets `PYTHONHOME`/`PYTHONPATH` for the bundled layout
     (`<prefix>/lib/dsview/python3.X`) before `Python::initialize`. The binaries get a
     `$ORIGIN/../lib/dsview` RUNPATH; today `dsview` and `dsview-cli` link `libpython3.12.so.1.0`
     without one and do not start outside pyenv.
   - **Firmware:** a clear error for a missing firmware or bitstream file.
   - **Decoder options:** path/exec options from files (`objdump`, `elffile`, `mapping`) are reset
     until the user confirms them (Phase 2 security item).
   - **DSO:** the hardware data scale is applied in the control API's DSO voltages.
   - **One version source:** the workspace version. CMake, `dsview-gui/src/lib.rs` and
     `dsview-cli/src/main.rs` all hard-code 1.6.1 today.
   - **Data dir:** `share/dsview` with `share/DSView` as the fallback.
   - **Crash reports:** a panic hook writes them to `$XDG_STATE_HOME/dsview/crash/`.
2. **srzip export.**
   - A writer in `dsview-formats` (zip with `metadata` INI and `logic-1-N` chunks), offered by the GUI
     and the control API.
   - Byte-checked against `libsigrok4DSL/output/srzip.c` in `dev/dsview-oracle/tests/export_diff.rs`
     while C still exists.
   - Gnuplot is dropped, with a DEVIATIONS row.
3. **`dsview-cli` subcommands** from §4:
   - `devices`, `info`, `capture`, `decode`, `export`;
   - `verify-counter`, from `crates/dsview-drivers/examples/dslogic.rs`;
   - `selftest`: a trace bundle for DSCope/E8 owners.
4. **The DSLogic matrix** (§3 item 6) on the DSLogic Plus, through `dsview-cli` and MCP scenarios:
   - simple, advanced and serial trigger at 0/50/100 %;
   - the test counter at every rate with 1/4/8/16 channels, in buffer and stream mode;
   - stop while waiting and mid-transfer;
   - a 10 min stream;
   - 1000 start/stop cycles.
   - The results go into STATUS. A nightly self-hosted runner stays a later decision.
5. **Tests and CI.**
   - an `e2e-mcp` job: `cargo xtask mcp suite` on `dsview-cli`;
   - a nightly fuzz schedule, with new targets `dsc_json`, `decoders_json`, `demo`, `usb_header`,
     `dso_status_block`, `jsonrpc_request`, `pcapng_reader` and `srzip`, until 24 h in total are
     reached;
   - proptests for the viewmodel (index↔pixel, zoom);
   - the chunking-invariance suite;
   - miri on the store's and bit-packing's `unsafe`;
   - `cargo xtask golden bless`;
   - the portability job checks every crate on Linux and macOS. Windows is dropped by decision; the
     known blockers are `libc::localtime_r` in `dsview-session/src/store.rs` and the Unix socket in
     `dsview-control/src/server.rs`;
   - `libfftw3-dev` added to `ORACLE_DEPS`;
   - the test job runs with Python 3.10 and 3.12, the interpreters of Ubuntu 22.04 and 24.04;
   - M6c's unrecorded exit checks: a protocol table with 100k+ rows stays interactive, and the table
     and its export are compared with Qt's on protocol.demo.
6. **Documentation.**
   - STATUS: the unplug traces were recorded 2026-10-18, but Phase 0 still lists them as missing; pin
     them in the manifest.
   - DEVIATIONS: tick the USB stop/abort row, and add departure rows for the "as built" points of §1.
   - PARITY: a note that the export row lacks srzip until step 2.

### Release R1: the A/B release (S, 1-2 pw)
*New 2026-10-18. It carries out the "C stays for exactly one release" rule of §1.2 for every
subsystem at once.*
- **Defaults:** `dsview-ffi/src/switch.rs` makes Rust the default for every `DSVIEW_IMPL_*`
  subsystem inside `DSView-qt`; `DSVIEW_IMPL=c` still selects C. The Rust GUI (`DSView`) is the
  default program.
- **`build.yml`:**
  - gets a Rust toolchain;
  - the deb and the AppImage carry the Rust `DSView` and `DSView-qt`, both with the bundled Python
    (the existing patchelf, stdlib and `PYTHONHOME` steps, extended to the Rust binary);
  - the Windows job stays as it is. It makes the last Qt Windows build.
- **Release:** merge `philipp` into `master` and tag it (e.g. v1.7.0). The release notes list the
  deviations, the Qt fallback, and that Windows ends with this release.
- **Exit:**
  - the packages install and run on Ubuntu 22.04, 24.04 and 26.04: the Rust GUI starts with the
    bundled Python, the demo decodes, and the DSLogic captures;
  - `DSView-qt` runs on Rust by default;
  - CI is green.

### Phase 6: Removal + packaging (M, 5-9 pw, milestone M7 = M7a removal and packaging + M7b fixes)
*Rewritten 2026-10-18 from an inventory of the remaining C tree. Starts after R1.*

#### M7a removal and packaging (3-5 pw), in this order
1. **Assets out of `DSView/`.**
   - Move `res/` (bitstreams, FX2 firmware, default profiles, `license.txt`), `demo/`, `icons/`, the
     udev rule, the desktop file (without `Qt;`), `packaging/debian/{postinst,postrm}`, `README` and
     `COPYING` to `assets/`.
   - Update the Rust paths that point into `DSView/`: tests, `dsview-ffi/src/dsl.rs`,
     `dsview-drivers/tests/demo.rs`, `testdata/corpus.json` (9 SHA-pinned entries) and
     `xtask corpus init`.
2. **Freeze what was generated from C.**
   - `crates/dsview-drivers/src/dsl/tables.rs` becomes the source of truth; `tools/gen/dsl_tables.py`
     (which reads `dsl.h`, `libsigrok.h` and `dslogic.c`) is removed.
   - `crates/dsview-core/tests/keys.rs` checks a frozen key list instead of parsing `libsigrok.h`.
3. **Archive the oracle.**
   - Tag the last commit with C as `c-final`.
   - `dev/dsview-oracle` (with `dsview-ffi`) builds against a checkout of that tag, which
     `cargo xtask oracle-src` makes as a git worktree. Today its `build.rs` compiles straight out of
     `DSView/`, `libsigrok4DSL/`, `libsigrokdecode4DSL/` and `common/`, and links Qt Core and FFTW.
   - It leaves the workspace's default members.
   - The C goldens stay as stored fixtures that Rust checks alone.
   - These checks move to a manual "archive" workflow: `decode-diff`, `drv-diff`, `trigger_diff`,
     `oracle-asan`, the store/dsp/export diffs, `xtask control ab` and `cpp-with-rust-core`.
4. **CI for Rust only:** lint, tests, fuzz, `e2e-mcp` and the golden check, without the C, Qt, glib
   or FFTW packages.
5. **Delete (about 117k lines):**
   - `DSView/`'s C++ and Qt resources (73k);
   - `libsigrok4DSL/`, including the shims (27k);
   - `libsigrokdecode4DSL/*.c|h` (7.5k). `decoders/` stays: 342 `.py` files;
   - `common/` (8.5k);
   - `CMakeLists.txt`, `CMake/`, `GNUmakefile`, `cross-compile-windows/` and the helper scripts;
   - the `DSVIEW_IMPL_*` switches.
   - The Windows installer files are kept for the follow-on port.
6. **Packaging.**
   - **cargo-deb**, with explicit dependencies. GL/EGL, the Vulkan loader, libX11/Xcursor/Xi/Xrandr,
     libxkbcommon, wayland-client and libdbus are all loaded with `dlopen`, so `$auto` does not see
     them. Recommends: `xdg-desktop-portal` or `zenity`, for the file dialogs.
   - **Layout:** `/usr/share/dsview/{res,decoders,lang,demo,icons}`, decoders without `__pycache__`.
     The bundled Python goes under `/usr/lib/dsview`.
   - **udev:** `60-dreamsourcelab.rules` (`MODE="0660", TAG+="uaccess"`) in `/usr/lib/udev/rules.d`,
     with postinst/postrm.
   - **AppImage:** a new recipe without the Qt plugin.
   - **`build.yml`:** Linux only (the Windows job removed); the release job reads the workspace
     version.
   - **Device access:** the first-run hint when it is denied (M6f's driver hint); the app never
     escalates privileges itself.
   - **Tools needing one fetch:** cargo-deb, plus linuxdeploy, appimagetool and patchelf, unless they
     run in CI only.
7. **Exit:**
   - 100 % Rust plus the Python decoders;
   - clean installs on Ubuntu 22.04, 24.04 and 26.04 (deb) and on Fedora (AppImage);
   - the DSLogic matrix green.

#### M7b post-parity fixes (2-4 pw; can start right after R1)
- **Fixes that waited for Rust to be the default:**
  - a fresh pin tuple per `wait()` (F13);
  - the skip-count check (F14);
  - `OUTPUT_LOGIC` for `pca9571`/`tca6408a`;
  - F11 (loop offset in single mode), F8 (CSV header padding), F19 (analog demo spin), F21 (disabled
    demo channels);
  - `Quirks::FIXED` where a fix changes decoder output.
- **Qt quirks kept for parity (5.0, F27):**
  - 1-2-5 ruler steps;
  - a float `x_offset`;
  - kinetic scroll without the integer truncation;
  - stable cursor numbers and colours;
  - a time-aligned protocol table;
  - a real TXT export;
  - the FFT with the true π.
- **"Ported as is" quirks (F24-F29):** reviewed one by one; each is fixed or kept with a reason.
- **Decoder bugs (F16):** fixed as patches to the Python decoders, upstreamed where possible.
- **Performance decisions:**
  - the ingest gate: accept 766 MiB/s, or redesign around an `rtrb` queue and pooled leaves;
  - the mmap spill for huge captures;
  - an `AnnotationStore` in `dsview-store`;
  - `loom` for the store's publication, once readers stop locking. `loom` needs a fetch.

### Follow-on (not scheduled; listed so nothing is lost)
- **Windows and macOS ports:**
  - the `cfg` fixes (`localtime_r`, `/proc` in the soak tools);
  - a control transport other than a Unix socket (named pipe or TCP on localhost);
  - installers, re-using `installer/windows`.
- **Hardware:**
  - the DSCope/E8 confirmation through `dsview-cli selftest` bundles;
  - a host firmware upload test, which needs a device without flash firmware and the 16 missing
    `.fw` files;
  - the GPU frame budget (≤ 8 ms at 4K × 32) on a real GPU;
  - a nightly self-hosted runner with the DSLogic (three nights in a row);
  - an RP2040 pattern generator for real decode and trigger signals.
- **Decoder corpus:** sigrok-dumps through a `.sr` → `.dsl` converter, and
  `testdata/decoders/coverage.toml`.
- **Debug and measurement tooling:** tracing spans and exporters (`DSVIEW_LOG`, Perfetto, Tracy), the
  crash ring buffer, `DSVIEW_PY_FAULTHANDLER`/`DSVIEW_DEBUGPY`, iai/criterion bench gates,
  per-decoder throughput baselines, nightly heaptrack.
- **Test data:** a repository or LFS for traces and large captures.
- **Upstream:** a monthly diff review against DreamSourceLab.

### Effort
The original estimate was 80-109 person-weeks (92-125 with 15 % contingency). Phases 0-4 and
M6.0-M6e are done.

Remaining, as of 2026-10-18:

| Step | Estimate |
|---|---|
| M6f | 3-4 pw |
| M6g | 3-4 pw |
| R1 | 1-2 pw |
| M7a | 3-5 pw |
| M7b | 2-4 pw |
| **Total** | **12-19 pw** |

Follow-on work is not counted.

**Order:** M6f → M6g → R1 → M7a. M7b runs alongside M7a after R1. M6g's hardware steps can run
alongside M6f.

---

## 3. Test plan
1. **Differential oracle at every seam.** C and Rust run as separate processes (each needs its own interpreter), both emitting JSONL with `PYTHONHASHSEED=0`. `testkit::diff` reports the first divergence with surrounding context.
2. **Property tests (proptest):**
   - `DSL_setting`/`ext32` field round trips;
   - sync words and divider/count/trigger-position math;
   - trigger masks (stages, serial, half/quarter-rate replication);
   - LA_CROSS ↔ per-channel bit packing;
   - mipmap invariants;
   - `display_edges` and next/prev edge against naive references;
   - loop-ring arithmetic;
   - INI/.dsc round trips;
   - `wait_scan` against a brute-force evaluator.
3. **Decoder corpus.** `testdata/decoders/coverage.toml` maps each of the 157 decoders to inputs; CI fails if a decoder has none. Inputs:
   - protocol.demo stacks;
   - `dsview-siggen` signals with jitter and glitches;
   - sigrok-dumps `.sr` files converted to `.dsl` (decoder/options taken from sigrok-test definitions, remapped to DSView ids);
   - seeded noise for every decoder.
4. **Chunking invariance.** Rust(X) == C(X) always. Rust(X) == Rust(Y) wherever C is invariant, with an allowlist of chunk-dependent decoders.
5. **Fuzzing (cargo-fuzz):**
   - targets: `ini_header`, `dsl_zip` (structure-aware), `dsc_json`, `decoders_json`, `demo`, `usb_header`, `dso_status_block`, `la_cross_ingest`, `eeprom_calib`, `jsonrpc_request`, `pcapng_reader`;
   - plus Rust-vs-C differential fuzzing of the `.dsl` parser;
   - every crash is minimized into a regression test.
6. **USB:** Phase 0 traces replayed through the Replay backend; Sim devices in CI. **DSLogic Plus hardware matrix** (USB 2 High-Speed, 16 channels, via `dsview-cli`):
   - internal test-counter mode in buffer and stream, at every rate step, with 1/4/8/16 channels; `verify-counter` checks every sample;
   - edge/level/advanced/serial trigger at 0/50/100% position;
   - stop during upload, during trigger wait and mid-transfer;
   - unplug/replug mid-capture;
   - 10 min stream with no overflow;
   - 1000-cycle soak.

   For real decode/trigger signals, an RP2040 Pico pattern generator (UART/I²C/SPI) is recommended. USB 3, 32-channel, 1 GHz and DSCope paths are covered by replay/Sim only plus external tester bundles.
7. **GUI:**
   - `dsview-viewmodel` unit tests;
   - `egui_kittest` snapshot tests (lavapipe, pixel-difference threshold, AccessKit queries);
   - MCP end-to-end YAML scenarios run against both Qt (baseline) and Rust DSView.
8. **Performance:**
   - criterion: `display_edges`, ingest GB/s, decode samples/s per decoder, 1 GB `.dsl` load, annotation viewport query;
   - iai-callgrind gate: a regression over 10% fails the PR;
   - frame time ≤ 8 ms at 4K × 32 channels while streaming.
9. **Memory/safety:**
   - ASan/UBSan/LSan on the oracle and on `compat/`;
   - miri on the `unsafe` in the store and bit-packing code;
   - loom on store publication;
   - nightly heaptrack;
   - soak tests with RSS limits.
10. **File compatibility:** Rust-written `.dsl` files must load in the C reader until Phase 6; afterwards, a frozen corpus of C-written files is kept as fixtures.

**Status of the test plan (2026-10-18):**

| Item | Done | Scheduled |
|---|---|---|
| 1 Differential oracle | goldens, decode-diff, drv-diff, store/dsp/export/trigger diffs | archived with the oracle in M7a (manual workflow) |
| 2 Property tests | store, formats, oracle | M6g: viewmodel. Follow-on: `DSL_setting`/`ext32`, LA_CROSS, INI/.dsc round trips, `wait_scan` against a brute-force evaluator |
| 3 Decoder corpus | protocol.demo, generators and noise in `decode-diff` (about 120 of 153 decoders) | follow-on: sigrok-dumps, `coverage.toml` |
| 4 Chunking invariance | Rust(X) == C(X) | M6g: Rust(X) == Rust(Y) with an allowlist |
| 5 Fuzzing | `header`, `dsl_reader`, `stores`; 60 s smoke runs in CI | M6g: the eight new targets and the nightly schedule up to 24 h. Differential `.dsl` fuzzing: follow-on |
| 6 USB and hardware | replay of the Phase 0 traces, the Sim devices, a part of the DSLogic matrix | M6g: the rest of the matrix. Follow-on: firmware upload, nightly runner, DSCope/E8 |
| 7 GUI | 33 snapshot and interaction tests, 2 window tests, viewmodel unit tests, the MCP suite against `dsview`/`dsview-cli`/Qt | M6f: tests for every new flow. M6g: `e2e-mcp` in CI |
| 8 Performance | `display_edges` and aggregate decode measured; `dsview --bench` frame times | follow-on: criterion/iai gates, GPU frame budget |
| 9 Memory and safety | ASan on the store oracle; 1 h soaks with RSS limits | M6g: miri. M7b: loom. Follow-on: ASan on all oracle diffs, heaptrack |
| 10 File compatibility | Rust-written files load in C (`golden check`) | M7a: frozen C-written fixtures |

## 4. Debug plan
- **tracing:** `DSVIEW_LOG=dsview_usb=trace,...`. Spans: `capture`, `usb_xfer`, `ingest_leaf`, `decode_chunk{stack,start,end}`, `rpc{method}`, `frame`. Exporters: `tracing-chrome` (Perfetto) and `tracing-tracy`. A ring buffer holds the last 10k events for crash reports. During the hybrid phase, `pv/log` is routed through `dsv_log_*`.
- **USB:** record to pcapng (Wireshark-readable). `dsview-cli usb-dump` decodes vendor commands, destinations and FPGA registers, prints `DSL_setting` field by field, and shows header packets and data rate. Optionally a Lua dissector generated from the same tables.
- **`dsview-cli`** subcommands: `devices`, `info`, `capture` → `.dsl`, `decode file.dsl --stack 'i2c:scl=0,sda=1>eeprom24xx' --chunks random:SEED -o out.jsonl`, `convert`, `export`, `replay trace.pcapng`, `verify-counter`, `diff-dsl`, `diff-ann` (shows the first divergence with a sample window), `serve --mcp`, `selftest` (records a trace bundle for external testers), `bench`.
- **PyO3 decoders:**
  - full tracebacks (decoder id + sample range) in the log, the decode error row and JSONL;
  - `DSVIEW_PY_FAULTHANDLER=1`;
  - `DSVIEW_DEBUGPY=5678` to attach VS Code to `pd.py`;
  - `py-spy dump --native` for stuck decode threads;
  - `dsview-cli decode --pdb`.
- **Crashes:** a panic hook writes local reports to `$XDG_STATE_HOME/dsview/crash/` (nothing is uploaded). Every `extern "C"` function is wrapped in `catch_unwind`, mapped to an error code, and keeps a thread-local last error (like `ds_get_last_error`).
- **A/B:** use `DSVIEW_IMPL*` to bisect a regression to one subsystem in seconds.
- **Status (2026-10-18):**
  - **Done:**
    - USB recording to pcapng (`cargo xtask usb record`) and `usb-diff`;
    - `dsview-cli serve --mcp` and `soak`;
    - `DSVIEW_IMPL*` bisecting;
    - goldens `gen`/`check`.
  - **M6g:**
    - the `dsview-cli` subcommands `devices`, `info`, `capture`, `decode`, `export`,
      `verify-counter`, `selftest`;
    - the panic hook;
    - `golden bless`.
  - **Follow-on:**
    - tracing and its exporters;
    - `usb-dump`, `replay`, `diff-dsl`, `diff-ann`, `bench`, `decode --pdb`;
    - the PyO3 debug switches;
    - the Lua dissector.
  - **Dropped:** routing `pv/log` through `dsv_log_*`, since Qt goes in M7a.
- **Goldens:** `cargo xtask golden {gen,check,bless}`. Small goldens use insta; large ones are zstd JSONL plus a hash manifest. Intentional differences are recorded in `docs/migration/deviations.md`, e.g. the EEPROM overflow fix, the fresh pin tuples, and `Quirks::FIXED` cases.

## 5. Risk register
| Risk | Mitigation |
|---|---|
| No DSCope/E8/USB 3 hardware | Byte-identical C/Rust command streams under `dsview-usb-fake`; trace replay; E8 traces from the fork author if obtainable; `selftest` bundles from external testers; flip each driver's default only after external confirmation; C fallback kept until Phase 6 |
| `wait()` semantic drift → silently wrong decodes | Transliterate first; pure `wait_scan` checked against a brute-force reference and the C oracle; noise differential for all 157 decoders; `Quirks::C_COMPAT` default; chunking-invariance suite |
| GUI parity scope creep | Frozen parity checklist; milestone gates; Qt stays the default until M6e; feature freeze until M7 |
| libpython at run time (found 2026-10-18: the binaries link `libpython3.12.so.1.0` with no RUNPATH) | Bundled Python with RUNPATH and `PYTHONHOME` (M6g); the R1 packages tested on three Ubuntu releases |
| Missing FX2 firmware (16 `.fw` files named in `dsl/tables.rs` are not in the repo) | A clear error (M6g); the upload path stays untested until a device and the files are available (follow-on) |
| The oracle and the C-vs-Rust CI compile out of the trees Phase 6 deletes | Archive against the `c-final` tag through `cargo xtask oracle-src` before deleting (M7a steps 3-4) |
| Run-time libraries loaded with `dlopen` (GL, Vulkan, X11, Wayland, xkbcommon, dbus) are invisible to package tools | Explicit deb dependencies; AppImage tested on Fedora (M7a) |
| Crates not in the offline registry (`accesskit_winit`, cargo-deb, loom) | One planned fetch per need, asked for at that step; resvg 0.44 and a hand-written INI reader avoid the others |
| No Windows/macOS build after Phase 6 (decision 2026-10-18) | R1 is the last Qt Windows release; the ports are follow-on work with the known blockers listed |
| No C fallback for DSCope/E8 after Phase 6, without a hardware confirmation | `dsview-cli selftest` in M6g; DEVIATIONS records the unconfirmed state; testers are asked before M7a |
| Performance regression | Phase 0 baselines; iai gates; wgpu-callback escape hatch; mmap spill |
| PyO3/GIL/embedding/packaging | Same thread-per-stack model as C; `allow_threads` only around Rust scans; stress tests; pinned Python per build; bundling reused from current CI |
| nusb maturity (hotplug, FX2 re-enumeration, WinUSB) | Trait-based backend with an optional `rusb` fallback; hardware soak |
| Licensing | `cargo deny` allowlist; firmware and bitstreams redistributed as today |
| Dual-implementation burden | One release of A/B per flip, then delete; after a flip, fixes go to Rust only |
| Upstream DreamSourceLab changes | Monthly upstream diff review; port with tests |
| Missing crates offline (nusb, pyo3, egui_dock, cbindgen, criterion not cached) | First fetch needs network; optionally `cargo vendor` |

## 6. Immediate next step
*Updated 2026-10-19: Phases 0-4 and M6.0-M6f are done (STATUS.md); PARITY.md stands at 52 of 52.*

**M6g hardening (§5.6)**, starting with its offline items: libpython RUNPATH and `PYTHONHOME`,
missing-firmware errors, the srzip port, the `dsview-cli` subcommands. Its hardware steps follow on
the DSLogic Plus.

Two things left over from M6f wait for the user: eframe's `accesskit` feature (a one-time fetch of
`accesskit_winit`), and the font licence for `cargo deny` (STATUS.md, M6f step 7).

After M6g come release R1, then Phase 6 (M7a, with M7b alongside).

Each milestone ends with a commit, STATUS/DEVIATIONS updates, and a report before the next one
starts. Steps that need the network (`accesskit_winit`, cargo-deb, loom) are asked for when they
come up.

## Verification of this plan's own claims
Every phase gate above can be checked by commands: `cargo nextest run`, `cargo xtask golden check`, `cargo fuzz run <t> -- -max_total_time=60`, `cargo bench`, the `hw-dslogic` runner, and the MCP end-to-end scenarios. No phase is declared done on inspection alone.
