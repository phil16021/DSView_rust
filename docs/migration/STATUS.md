# Migration status

| Phase | State | Notes |
|---|---|---|
| 0 Infrastructure, oracle, baselines | **done, one item deferred** | Deferred: performance baselines (moved to Phase 1b/2, see DEVIATIONS.md). USB traces recorded 2026-10-17 |
| 1 Formats + store | **done**: 1a formats behind `DSVIEW_IMPL_FORMATS`, 1b sample stores behind `DSVIEW_IMPL_STORE`; Rust becomes the default after one release of A/B | See "Phase 1a" and "Phase 1b" below |
| 2 Decoder engine | **done**: Rust/PyO3 engine behind `DSVIEW_IMPL_DECODE`; Rust becomes the default after one release of A/B | See "Phase 2" below |
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

**USB baseline (recorded 2026-10-17, DSLogic Plus, PID 0x0034, firmware 2.2, C driver):**
- `cargo xtask usb record --iface usbmon1` recorded all 11 scenarios: 1/5/20 MHz × 16/8/4 channels,
  plus two "trigger never fires, stop while waiting" cases. All passed.
- `startup.pcapng` was recorded by hand. It holds the device open and the initial configuration
  (0xB0/0xB1/0xB2 vendor requests). It has no FX2 firmware upload because the firmware was
  already loaded; recording that needs a replug.
- The 12 traces total 16 MB and are not in git (`*.pcapng` is ignored).
  `testdata/usb/dslogic/manifest.json` pins each trace's sha256 and size, and records the
  capture result DSView reported for it.
- Each capture trace contains the 372-byte `DSL_setting` on EP2 and the data on EP 0x86.
  EP 0x86 byte counts differ by rate for the same 1000448 samples (2.18 MB at 1 MHz,
  2.80 MB at 5 MHz, 1.72 MB at 20 MHz with 16 channels). Phase 3 replay must explain this
  from the C driver's transfer sizing.
- Not recorded yet (by hand, see `tools/usb/README.md`): firmware upload after a replug,
  unplug during a capture, and internal test-counter mode.

**CI:** `.github/workflows/rust.yml` runs lint, tests, cargo-deny, golden check and a portability check.

**Removed:** the stale `DSView/test` and `libsigrok4DSL/tests`.

## Phase 1a (formats): status

**Done and verified:**
- **`crates/dsview-formats`:**
  - GKeyFile-compatible INI parser.
  - The header interpreted exactly like the C loader, including all quirks and the security hardening.
  - Samplerate parsing (F3).
  - `.dsl`/`.demo` reader for logic, DSO and analog, versions 1-3, with size limits.
  - Pull-based packet stream with the C driver's packet boundaries.
  - A writer for logic captures.
- **Differential tests:**
  - The Rust reader equals the C session driver on all 9 corpus files: headers, channels, data hashes and
    packet counts (`tests/golden_dsl.rs`).
  - Rust-written files read back identically in C (`cargo xtask golden check`).
  - Three synthetic multi-block captures (up to 3 blocks, odd sample counts, packet boundaries in the middle
    of a block) read identically by C and Rust.
- **Fuzzing:** targets `header` and `dsl_reader`. 2.3 million and 145,000 runs found no crash.
- **FFI seam:**
  - `dsv_dsl_*` in `crates/dsview-ffi` with a cbindgen header.
  - `libsigrok4DSL/session_driver.c` loads captures through Rust when `DSVIEW_IMPL_FORMATS=rust` (or
    `DSVIEW_IMPL=rust`) is set and DSView is built with `-DDSVIEW_RUST_CORE=ON`.
  - `cargo xtask control ab --bin <DSView>` runs the same probes through a live DSView with both readers;
    all 9 captures give identical results.
  - The crafted malicious files are rejected by the Rust reader with a clear log message.

**Saving and export (second step of 1a):**
- **Saving:** `DslZipWriter` behind `ZipMaker` (`dsv_zip_writer_*`) and `dsv_dsl_entry` behind `ZipReader`.
  `StoreSession` uses `ZipMaker::Discard`, so a cancelled Rust save keeps the previous file.
- **Export:** CSV and VCD exporters behind libsigrok's output-module interface
  (`libsigrok4DSL/output/rs_output.c`, chosen in `sr_output_list()`).
- **One switch:** `dsv_use_rust("FORMATS")` decides for the session driver, export and save.
- **Byte-exact checks** (`dev/dsview-oracle/tests/export_diff.rs`):
  - Rust CSV (logic/DSO/analog) and VCD against the C modules on 400 random cases each.
  - `%.15g`, `%.5f` and `%.0f` against glibc `snprintf`.
  - Findings F5 (locale) and F6 (crash) reproduced.
- **Live A/B** (`cargo xtask control ab`): opening, CSV/VCD export and saving every corpus capture inside a
  running DSView gives identical results with `DSVIEW_IMPL=c` and `rust`. Rust-saved files read
  identically in the C loader; the only header difference is the wall-clock `trigger time`.
  - Also runs in CI from a `DESTDIR`-staged install.

**Left for later (see DEVIATIONS.md, "Departures"):**
- `.dsc` profiles (Phase 4/5).
- srzip export.
- Making Rust the default after one release of A/B.

## Phase 1b (sample stores): status

**Done and verified:**
- **`crates/dsview-store`:** ports of the three `pv::data` snapshot classes.
  - `logic`: `LogicSnapshot`. Same layout, so the pointers handed to C decoders keep their meaning:
    2^24-sample leaves with three toggle mip-map levels, 64-leaf root nodes, constant leaves freed,
    loop (ring) mode with root rotation, decoder pins. Also edge search, display edges, pattern
    search and block access for saving.
  - `dso`: `DsoSnapshot` (frame copy, out-of-range flag). The envelope, `cal_vrms` and `cal_vmean`
    are never called in DSView and are not ported.
  - `analog`: `AnalogSnapshot` (sample ring, unit pitch, 10-level min/max envelope).
- **Oracle:** `dev/dsview-oracle` compiles the original `logicsnapshot.cpp`, `dsosnapshot.cpp`,
  `analogsnapshot.cpp` and `snapshot.cpp` unchanged against Qt Core. It reads their private state
  through the `…SnapshotTest` friend classes the headers already declare.
- **Differential tests** (`dev/dsview-oracle/tests/store_diff.rs`, `store_ad_diff.rs`): every packet
  and every query DSView makes, mid-capture and after the end. They compare:
  - counts;
  - the complete internal state (fractions, write position, mip-map progress, decoder pins, every
    root node's bits, every leaf's samples and mip-maps);
  - samples, decoder runs, block buffers, display edges, next/previous edge, pattern search;
  - DSO buffers, analog ring and envelopes.
- **Exit run:**
  - logic: 10^4 one-leaf scenarios, 10^4 one-leaf loop scenarios, 400 multi-leaf, 400 multi-leaf
    loop, plus a 1.16G-sample loop run that rotates root nodes;
  - DSO and analog: 10^4 scenarios each;
  - result: 0 differences.
  - Mutation checks: every planted bug was caught.
  - Under ASan/UBSan the C++ code is clean on the inputs DSView produces; CI job `oracle-asan`.
- **Robustness:**
  - `crates/dsview-store/tests/robustness.rs`: packets of any size, captures without `clear()`, and
    queries outside the C++ preconditions. No panic.
  - Fuzz target `stores`, 2 × 20 min. The first run found a panic on analog rows wider than 8 bytes,
    now fixed (F12, regression test). The second run did 438k executions without a finding.
- **FFI seam:**
  - `dsv_logic_*`, `dsv_dso_*`, `dsv_analog_*` in `crates/dsview-ffi`.
  - `LogicSnapshot`, `DsoSnapshot` and `AnalogSnapshot` delegate to Rust when
    `DSVIEW_IMPL_STORE=rust` (or `DSVIEW_IMPL=rust`). They mirror the counters into the `Snapshot`
    fields, so the rest of the GUI is unchanged.
  - The `version` RPC reports the active implementations, and `cargo xtask control ab` checks them.
- **Live A/B:** all 9 corpus captures give identical answers with `DSVIEW_IMPL=c` and `rust` (Rust
  formats plus Rust stores).
- **Hardware:** the 11 DSLogic Plus scenarios captured through the Rust stores give the same results
  as the C++ baseline in `testdata/usb/dslogic/manifest.json`.

**Performance** (`cargo bench -p dsview-oracle --bench store`, same data through both):

| Benchmark | C++ | Rust | Gate |
|---|---|---|---|
| Ingest, 16 channels, 128M samples | 777 MiB/s | 766 MiB/s | ≥ 1 GB/s: **not met by either** (see DEVIATIONS) |
| Display edges, 256M samples, 1920 px, 6 zoom/signal cases | 6.2 µs – 360 µs | 3.0 µs – 285 µs | ≤ 1.1× C++: met, Rust 1.2–2.4× faster |
| 20k next-edge steps | 0.77 – 1.24 ms | 0.49 – 0.64 ms | — |

**Findings in the original code** (DEVIATIONS.md):
- **F9:** packets that are not whole 64-sample groups corrupt the capture.
- **F10:** a capture into an uncleared snapshot uses stale state; ASan reports heap overflow and
  use-after-free.
- **F11:** captures whose length is not a multiple of 64 are shown shifted and lose samples when
  saved. Confirmed in the shipping DSView with a 1000-sample `.dsl`.
- **F12:** the analog envelope reads one entry past its ring.

**Left for later:**
- Making Rust the default (after one release of A/B).
- Fixing F11 in Rust once Rust is the default.
- Lock-free readers (`loom`) and the mmap spill, with the Rust capture path in Phase 4.
- The annotation store, with the decoder engine in Phase 2.

## Phase 2 (decoder engine): status

**Done and verified:**
- **`crates/dsview-decode`:** the libsigrokdecode4DSL engine in Rust on PyO3. It has:
  - the loader with `decoder.c`'s checks;
  - the `sigrokdecode` module: `Decoder` with `put`/`register`/`wait`/`has_channel`/`printlog`, and the
    output and config constants;
  - instances, stacks and sessions with one thread per stack and the same handoff as C;
  - `wait()` condition matching, transliterated from `instance.c`, including the constant-chunk
    shortcut, `skip`, sample-0 seeding and the `matched` bitmask.
  The DSView extensions (ann_type 3-tuples, `@hex`, integer payloads, `end()`) behave as in C. So do the
  C quirks that change output: the refilled pin tuple (F13), the pending skip error (F14) and NUL
  truncation (F15).
- **FFI seam:**
  - `dsv_decode_*` in `crates/dsview-ffi/src/decode.rs`.
  - `libsigrokdecode4DSL/rs_decode.c` hooks the `srd_*` entry points when `DSVIEW_IMPL_DECODE=rust`
    (or `DSVIEW_IMPL=rust`). It builds the `srd_decoder` lists from Rust metadata and turns Rust
    output events into `srd_proto_data`, so the GUI is unchanged.
  - CMake passes the interpreter it found to PyO3 (`PYO3_PYTHON`), so both engines use one libpython.
- **Differential tests** (`dev/dsview-oracle`): `rust-decode` mirrors `oracle-decode` (C engine) and
  emits the same JSONL. Each run of `decode-diff`:
  - builds a random 16-channel capture: noise plus I²C, SPI and UART traffic in 80% of runs;
  - stacks every one of the 157 decoders on it with random options and channel maps, and puts protocol
    decoders on top of the matching bus decoders;
  - runs both engines with the same random chunking and compares everything: annotations, binary, meta,
    errors and exception texts.
  Decoders that never return (F16) are cut off by a per-stack watchdog, and both engines must agree on
  the common prefix.
- **Exit run:**
  - `protocol.demo`: byte-identical output from both engines (9337 lines); the golden check covers both.
  - Metadata of all 157 decoders is identical.
  - Exit batch: 1000 runs (`decode-diff --runs 1000 --seed 100000`), each with all 157 decoders.
    - 417M output lines compared, 406M of them annotations, in 188 min with 6 jobs.
    - 141 of 153 logic decoders produced annotations; the rest need inputs the generators do not make.
    - **No Rust divergence.** Two seeds were flagged, and neither is a Rust difference:
      - 100223: C itself changed its output in that run (F17); it matched Rust in 29 replays.
      - 100983: 1-sample chunks; both engines need about 650 s, and only C finished inside the
        harness's 600 s process limit. The harness now resumes after the last finished stack, and
        the seed passes (run again with a 120 s limit, so it resumed several times).
    - Decoder stacks that looped forever in both engines and were cut off by the watchdog: `gpib` 90,
      `ltc26x7` 78, `midi` 1.
  - Divergences found and fixed in the Rust port before the exit run:
    - NUL truncation (`amulet_ascii`);
    - the pending skip error (`flexray`);
    - the pending error being lost on a type-cache miss (F17). This one was nondeterministic: it was
      found at run 354 of a first 1000-run batch, which was then restarted with the fix.
- **Live A/B:** `cargo xtask control ab` with formats, stores and decoders all in Rust: 8379
  annotations from all protocol.demo decoders are identical to the C run.

**Performance:** `decode-diff --bench 40`, i.e. the protocol.demo signals repeated 40 times (5.2M
samples) and decoded by its 4 saved stacks. Idle machine, each engine timed with `/usr/bin/time` over
3 runs:

| Engine | Wall time | User CPU | Gate |
|---|---|---|---|
| C | 1.98 – 2.00 s | 1.84 – 1.88 s | |
| Rust | 1.98 – 2.00 s | 1.84 – 1.86 s | ≥ 0.9× C: **met** (1.00×) |

The time goes into the Python decoders. Under load (the batch running alongside) Rust was 0.99–1.05×
C.

**Findings in the original code and decoders** (DEVIATIONS.md, identical in both engines):
- **F13:** decoders that keep the tuple `wait()` returned fail with SystemError on their next `wait()`
  (`pjdl`, `ieee488`).
- **F14:** a negative `skip` count leaves an OverflowError pending and fails later (`flexray`).
- **F15:** decoder strings are cut at a NUL character (`amulet_ascii`).
- **F17:** whether that pending error survives depends on CPython's type-attribute cache, so in
  principle the result can differ between runs. Rust now keeps the error deterministically.
- **F16:** `gpib`, `onewire_link` and `ltc26x7` can loop forever; `onewire_link` also grows memory
  without bound. Plus `lfast` (NameError), `ir_irmp` (missing `libirmp.so`), and `pca9571`/`tca6408a`
  (no `OUTPUT_LOGIC`).

**Left for later:**
- Making Rust the default (after one release of A/B), then deleting `libsigrokdecode4DSL/*.c`.
- After the flip: fresh pin tuples (F13), the skip check (F14), `OUTPUT_LOGIC`.
- Resetting path/exec decoder options from saved files: GUI work (Phase 4/5).
- The annotation store: annotations are still kept by the C++ `pv::data::decode` classes. It moves to
  Rust with the GUI data model.
- sigrok-dumps captures as decoder corpus, with the CLI (Phase 4).
