<!-- Approved migration plan (2026-10-17). Phase status is tracked in STATUS.md. -->

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

### Phase 5: Rust GUI (XL, 35-46 pw, milestones M6a-f)
**Toolkit: egui/eframe (wgpu)** + egui_dock + egui_extras virtualized tables (millions of protocol rows) + catppuccin-egui (matches the frappe/latte themes) + AccessKit.

Why egui:
- About 70% of the UI is custom-painted traces, and immediate mode suits a 30 Hz streaming redraw.
- Option-driven dialogs (device and decoder options generated at runtime) are natural in immediate mode.
- The required crates for docking, tables, themes and accessibility are maintained.
- Rerun shows egui scales to large time-series data.

Why not the others:
- gtk4-rs/relm4 has more native forms but harder trace rendering and painful Windows/macOS packaging.
- iced has gaps in tables, accessibility and docking, and its API still churns.
- Slint has no good custom canvas.
- cxx-qt would still mean QML plus C++.

Rendering: a per-trace `egui::Mesh` built from `display_edges`. If a frame takes more than 4 ms at 4K × 32 channels, switch to an `egui_wgpu` callback pass.

Other requirements:
- Bundle a Noto Sans CJK subset (OFL) for the Chinese UI.
- All logic lives in `dsview-viewmodel`, so the toolkit stays swappable.
- `dsview` ships as a preview next to Qt until M6e.

| Milestone | Content | Exit |
|---|---|---|
| M6a viewer | open .dsl/.demo, logic traces, ruler, header, zoom/pan/inertia, cursors, Rust-hosted control API (read methods) | egui_kittest snapshots; MCP read-tool conformance |
| M6b logic capture | device select, sampling bar, channels, simple trigger, single/repeat/loop/instant | hardware matrix via MCP scenarios |
| M6c decoders | decoder menu, runtime option forms, decode traces, protocol dock (table/search/export) | decoder goldens via `get_annotations` |
| M6d DSO/analog | DSO/analog traces (envelope, persistence, dials), DSO trigger, measure, math, spectrum, lissajous, ref waves | replay snapshots; DSP vs FFTW ≤ 1e-9 relative |
| M6e docks/dialogs | advanced 16-stage + serial trigger, search, ~22 dialogs (device options generated from `config_list`, calibration, store progress, app options …) | full 30-tool MCP suite green → **Rust GUI default** |
| M6f polish | i18n (`tr!` over existing `lang/{cn,en,de}` JSON), 4 themes, ~250 SVG icons, shortcuts (S/I/T/D/M/R/O, PgUp/PgDn, arrows, 0/1), .dsc profiles, QSettings import, AccessKit audit | parity checklist 100% |

The parity checklist is frozen from the GUI inventory and lives in `docs/migration/parity.md`. No new features are added before M7.

### Phase 6: Removal + packaging (S, 3-5 pw, milestone M7)
- **Delete:** `DSView/` (C++), `compat/`, CMake, and the Qt/boost/FFTW/glib dependencies. The oracle is kept as an archived dev crate pinned to the last C tag.
- **Packaging:**
  - `cargo-deb`: `/usr/share/dsview/{res,decoders,lang,demo,icons}`, udev rule `60-dreamsourcelab.rules` with `MODE="0660", TAG+="uaccess"`.
  - AppImage with bundled Python (reusing the build.yml steps).
  - A first-run hint when device access is denied; the app never escalates privileges itself.
  - Windows MSI and macOS .app are follow-on work.
- **Exit:** 100% Rust plus the decoders; clean installs on Ubuntu 22.04/24.04 and Fedora; hardware matrix green.

### Effort
About 80-109 person-weeks for one experienced developer, roughly 92-125 with 15% contingency. That is about 2 calendar years at 80% utilization.

Critical path: P0 → P1a → P2 → P3d → P4 → P5. With two developers, P1b runs alongside P2, P3a-c alongside P2, and M6a can start against `dsview-cli serve`.

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
- **Goldens:** `cargo xtask golden {gen,check,bless}`. Small goldens use insta; large ones are zstd JSONL plus a hash manifest. Intentional differences are recorded in `docs/migration/deviations.md`, e.g. the EEPROM overflow fix, the fresh pin tuples, and `Quirks::FIXED` cases.

## 5. Risk register
| Risk | Mitigation |
|---|---|
| No DSCope/E8/USB 3 hardware | Byte-identical C/Rust command streams under `dsview-usb-fake`; trace replay; E8 traces from the fork author if obtainable; `selftest` bundles from external testers; flip each driver's default only after external confirmation; C fallback kept until Phase 6 |
| `wait()` semantic drift → silently wrong decodes | Transliterate first; pure `wait_scan` checked against a brute-force reference and the C oracle; noise differential for all 157 decoders; `Quirks::C_COMPAT` default; chunking-invariance suite |
| GUI parity scope creep | Frozen parity checklist; milestone gates; Qt stays the default until M6e; feature freeze until M7 |
| Performance regression | Phase 0 baselines; iai gates; wgpu-callback escape hatch; mmap spill |
| PyO3/GIL/embedding/packaging | Same thread-per-stack model as C; `allow_threads` only around Rust scans; stress tests; pinned Python per build; bundling reused from current CI |
| nusb maturity (hotplug, FX2 re-enumeration, WinUSB) | Trait-based backend with an optional `rusb` fallback; hardware soak |
| Licensing | `cargo deny` allowlist; firmware and bitstreams redistributed as today |
| Dual-implementation burden | One release of A/B per flip, then delete; after a flip, fixes go to Rust only |
| Upstream DreamSourceLab changes | Monthly upstream diff review; port with tests |
| Missing crates offline (nusb, pyo3, egui_dock, cbindgen, criterion not cached) | First fetch needs network; optionally `cargo vendor` |

## 6. Immediate next step after approval
Carry out **Phase 0 only**, then report back:
- the workspace, CI files and Corrosion stub;
- `dsview-oracle` building `oracle-dsl` and `oracle-decode` against protocol.demo;
- the corpus manifest;
- the baseline control-API transcripts against Qt `DSView --mcp`;
- a script plus instructions for recording the DSLogic usbmon traces. Recording them needs you: `sudo modprobe usbmon`, then plug in the device.

Phase 1 starts only after you've reviewed Phase 0.

## Verification of this plan's own claims
Every phase gate above can be checked by commands: `cargo nextest run`, `cargo xtask golden check`, `cargo fuzz run <t> -- -max_total_time=60`, `cargo bench`, the `hw-dslogic` runner, and the MCP end-to-end scenarios. No phase is declared done on inspection alone.
