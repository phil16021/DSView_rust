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
| **M6f polish** | i18n (en/de/cn JSON, runtime switch, CJK font subset), four themes as egui visuals, 212 SVG icons with dark/light, shortcuts, command line (`-l -v -s --mcp FILE`), settings and first-run import of `DSView.conf`, help links, screenshot, AccessKit audit | PARITY.md 100 % | 3-4 pw |

The total stays in the original 35-46 pw.

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

## 6. Immediate next step
*Updated 2026-10-18; Phases 0-4 and M6.0-M6d are done (STATUS.md).*

**M6e, docks and dialogs, after which the Rust GUI becomes the default:**
1. **Advanced and serial trigger** (16 stages, the trigger dock's position slider) in the session
   and the dock.
2. **Measure and search docks** (cursor list, per-channel measurements, search pattern dialog).
3. **Device options** generated from the driver's option list; calibration (with the start-up
   prompt for DSO hardware); store progress (cursor range, compressed or original).
4. **Application options** that M6a-d left in menus (persistence, line widths, wheel).
5. **The full MCP suite** against `dsview --mcp`, then the default flip.

Each milestone ends with a commit, STATUS/DEVIATIONS updates, and a report before the next one
starts.

## Verification of this plan's own claims
Every phase gate above can be checked by commands: `cargo nextest run`, `cargo xtask golden check`, `cargo fuzz run <t> -- -max_total_time=60`, `cargo bench`, the `hw-dslogic` runner, and the MCP end-to-end scenarios. No phase is declared done on inspection alone.
