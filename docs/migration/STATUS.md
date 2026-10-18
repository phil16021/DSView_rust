# Migration status

| Phase | State | Notes |
|---|---|---|
| 0 Infrastructure, oracle, baselines | **done, one item partly deferred** | Performance baselines: `display_edges` and the aggregate decode rate were measured in 1b/2; per-decoder throughput and 1G samples are follow-on (PLAN §2). USB traces recorded 2026-10-17 |
| 1 Formats + store | **done**: 1a formats behind `DSVIEW_IMPL_FORMATS`, 1b sample stores behind `DSVIEW_IMPL_STORE`; Rust becomes the default after one release of A/B | See "Phase 1a" and "Phase 1b" below |
| 2 Decoder engine | **done**: Rust/PyO3 engine behind `DSVIEW_IMPL_DECODE`; Rust becomes the default after one release of A/B | See "Phase 2" below |
| 3 Drivers | **done except hardware steps**: 3a demo, 3b file, 3c USB layer, 3d DSLogic (replug tests passed 2026-10-18), 3e DSCope/MSO-E8 (`DSVIEW_IMPL_DRV_DEMO` / `_FILE` / `_DSLOGIC` / `_DSCOPE`). Open: a firmware upload from the host, the nightly runs, one DSCope/E8 hardware confirmation | See "Phase 3" below |
| 4 Session + control API | **done**: 4a acquisition library in Rust behind `DSVIEW_IMPL_ACQ`; 4b capture session, control server, `dsview-cli serve --mcp` (transcript identical to the Qt GUI, 1 h soaks passed) | See "Phase 4" below |
| 5 Rust GUI | **in progress**: M6.0, M6a-M6e done; the Rust GUI is the default on Linux (`DSVIEW_RUST_GUI`, Qt kept as `DSView-qt` for one release); next M6f polish | Plan revised 2026-10-18 (PLAN.md §5); PARITY.md 41 of 52, the rest M6f |
| 6 Removal + packaging | planned after release R1: M7a removal and packaging, M7b post-parity fixes | PLAN.md (revised 2026-10-18): M6f → M6g → R1 → M7a/M7b |

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
  - Since then:
    - unplug during a stream capture was tested with both drivers on 2026-10-18 (Phase 3d, "Replug
      tests"), but its traces are not in the manifest yet (PLAN M6g);
    - firmware upload cannot be tested with this device (follow-on);
    - test-counter traces under the C driver are M6g.

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

## Phase 3 (drivers): status

### 3a demo driver, 3b file driver: done

**Crates:**
- `crates/dsview-core`: config keys (checked against `libsigrok.h` by a test), config values with
  their GVariant types, channels, `struct sr_status` (same layout, checked), datafeed packets.
- `crates/dsview-drivers`:
  - the `Device` trait mirrors `struct sr_dev_driver`. Acquisition runs as in libsigrok's session
    loop: one `poll` per source callback; pacing sleeps are returned to the caller, so it can sleep
    without holding the device lock; packets come back owned, so the GUI's datafeed callback may call
    into the device.
  - `demo`: a transliteration of `hardware/demo/demo.c` (logic random and pattern playback,
    DSO with its timebase/vdiv/offset reshaping and status measurements, analog), including its
    timing and its order of `rand()` calls.
  - `file`: `session_driver.c`'s driver part on top of dsview-formats' reader (Phase 1a):
    `sr_new_virtual_device`, loading the header into config values and channels,
    `config_get`/`set`/`list`, HEADER and TRIGGER packets, playback.
  - `sys`: the clock, `rand()`, logging and `DS_USR_PATH`, process-wide like in C and replaceable
    in tests.

**C seam:**
- `dsv_drv_*` in `crates/dsview-ffi/src/drv.rs`. Every entry point catches panics, and a panic does
  not poison the device.
- `libsigrok4DSL/hardware/rust/rs_driver.c` implements `sr_dev_driver` on top of it:
  - GVariants are translated in both directions, including the `sr_list_item` lists handed out by
    pointer.
  - The `sr_channel` list stays in C because the GUI writes it directly. It is copied into Rust
    before every call and back afterwards, and rebuilt only where the C driver would free and
    recreate its channels, so the GUI's channel pointers live exactly as long.
- `sr_driver_list()` swaps in the Rust demo driver when `DSVIEW_IMPL_DRV_DEMO=rust`.
  `sr_new_virtual_device()` creates Rust file devices when `DSVIEW_IMPL_DRV_FILE=rust`. Both are on
  with `DSVIEW_IMPL=rust`.
- The `version` RPC reports `drv_demo` and `drv_file`.

**Differential test (`drv-diff`, dev/dsview-oracle):**
- Seeded scenarios run through libsigrok4DSL's public `ds_*` API, exactly as DSView calls it,
  once with the C drivers and once with the Rust ones.
  - Demo scenarios pick a device, a target mode and pattern, then random config changes, channel
    switches, device switches and captures stopped after N packets.
  - File scenarios open captures (the demo files plus synthesized logic v1/v2/v3 with several and
    odd blocks, a DSO capture with a trigger position, and one with a missing block), change config
    and channels, and replay them.
- Compared: every config get (all 124 keys × every channel) and list value with its GVariant type,
  the channel list, the mode list, the status block, every datafeed packet (all fields and a hash
  of the data), and every return code.
- Both drivers run on one virtual clock and one random sequence, so random data and time-based DSO
  output are compared exactly. `demo.c` is built with its timers, `g_usleep`, `rand` and `srand`
  mapped onto the oracle's.
- Where C crashes or reads undefined memory (F18), Rust must match C up to that point and then finish
  cleanly.
- **Result:**
  - 1000 scenarios (seeds 1000–1999): all identical. 1.22 million answers and 43,856 packets compared.
    In 68 of them C crashed or read undefined memory; Rust matched it up to there and finished.
  - Exit batch, 5000 scenarios (seeds 10000–14999): all identical. 6.18 million answers and 215,186
    packets compared, in about 7 minutes with 6 jobs. In 355 of them the C driver crashed, every
    time in F18 code (345 in the analog wrap-around `memcpy`, 10 in logic random), and 17 more read
    undefined memory (F18). Rust matched C up to there and finished each scenario.
- **Divergences found and fixed during the port:**
  - The DSO stop order (C sleeps and restarts its timer before the final END).
  - Rust panics on buffer overruns where C crashes.
  - Version-1 captures streamed with the file's channel flags instead of the live ones. This gap
    dated from Phase 1a and was fixed in both the Rust driver and `session_driver.c`'s Rust path.

**Unit tests:** `crates/dsview-drivers/tests/demo.rs` covers the device list, a complete
protocol.demo playback, channel keys without a channel, analog overruns and DSO stop.

**Live A/B (staged DSView, headless, control API):**
- The 38-step control scenario (logic capture with protocol.demo, decoders, save, export, reopen,
  DSO capture and measurement) gives byte-identical transcripts with `DSVIEW_IMPL=c` and with
  `DSVIEW_IMPL=rust` (formats, stores, decoders, demo and file drivers all in Rust). The log shows
  both Rust drivers in use.
- `cargo xtask control ab`: all 9 corpus captures open, export, save and decode identically, now
  through the Rust file driver.

**Findings in the original code** (DEVIATIONS.md):
- **F18:** the demo driver reads past its buffers: analog patterns with long packets (crash),
  logic random at rates the channel modes do not offer (`rand() % 0`, crash), and the last
  instant DSO packet.
- **F19:** the analog demo sends nothing and spins at 100% CPU for captures under 2048 samples. This
  is reachable from the GUI (10 Hz, 200 s).
- **F20:** packets carry uninitialised fields, among them DSO `samplerate_tog`/`trig_flag`/`trig_ch`
  and analog `unit_pitch`, which the GUI uses.
- **F21:** demo logic playback ignores disabled channels (it shifts data); `map_unit` dangles;
  NULL-channel crash; two END packets per DSO capture.

### 3c USB layer, 3d DSLogic driver: done (hardware steps that need a replug open)

**`crates/dsview-usb`:**
- `UsbBackend`/`UsbDevice` traits. Every call has a timeout; bulk IN is an ordered queue of
  transfers that can be cancelled as a whole.
- **nusb backend:** no libusb.
- **Trace reader:** usbmon pcapng (`dumpcap -i usbmonN`).
- **Replay device:** plays the device side of a C-driver capture. It checks every vendor request
  and bulk OUT the driver sends, and answers with the recorded responses and sample data. It
  releases bulk IN data only after the commands that preceded it in the capture.
  Timing-dependent status polls are optional; the header offsets C leaves unset (F22) are not
  compared.
- **Tools:**
  - `usb-dump`: a capture as protocol steps;
  - `usb-diff`: compares two captures with the replay's rules;
  - `usb-probe`: lists devices and reads a DSLogic's firmware version.

**DSLogic driver** (`crates/dsview-drivers/src/dsl`):
- Logic mode of `dslogic.c` and the shared parts of `dsl.c`:
  - scan, profile match and firmware upload;
  - open: firmware version, FPGA configuration or HDL version check, the security handshake,
    the threshold;
  - all config keys;
  - the 372-byte `DSL_setting` and the 32-channel extension;
  - transfer sizing as in C, the trigger header, data trimming;
  - progress and overflow polls, the two-step stop.
- Profiles, channel modes and lists are generated from `dsl.h` (`tools/gen/dsl_tables.py`; the
  DSCope profiles are already in).
- C seam: `rs_driver.c` registers the driver in place of `DSLogic_driver_info`.
  - Each device is keyed by the `libusb_device` that lib_main's hotplug thread knows, so attach,
    detach, the 500 ms reconnect merge and the duplicate check work unchanged.
  - The GUI's `ds_trigger` is passed in when a capture starts.
  - Device errors reach `ds_set_last_error()`.

**Checks:**
- **Replay of the Phase 0 C captures** (`tests/dslogic_replay.rs`). Opening the device (startup
  trace) and all 11 capture scenarios match the C driver step for step, including every
  `DSL_setting` byte. The scenarios are 1, 5 and 20 MHz with 16, 8 and 4 channels, plus two trigger
  waits stopped through WAIT_UPLOAD. The delivered data equals the captured bulk data, exactly
  `limit` samples. Mutation checks: a changed mode bit and a wrong trim are caught. A changed
  trigger-position cap and the second stop stage are not reached by these captures; the second
  stop stage is covered live below.
- **Trigger masks against C** (`dev/dsview-oracle/tests/trigger_diff.rs`): 2000 random triggers set
  through the C `ds_trigger_*` API (16 advanced stages, the simple stage, 32 probes), all six masks
  for every stage, both probe halves and half/quarter rate, about 1.6 million values. All equal;
  C's `struct ds_trigger` also reads as `dsview_core::Trigger`.
- **Live, DSLogic Plus (PID 0x0034, firmware 2.2), C and Rust in the same staged DSView:**
  - usbmon recordings of the startup and 12 scenarios with `DSVIEW_IMPL=c` and `rust`. The 12 are
    the Phase 0 set plus a new user stop in the middle of a 10 s stream.
  - `usb-diff` reports the same protocol for all 13, including the stop's second stage (FORCE_RDY,
    drain, STOP, status read).
  - The capture results (samples, duration) are identical. Only the stop latency differs (51 vs
    72 ms) and, with it, how many transfers arrive before the stop (53 vs 56).
- **Rust driver alone** (`cargo run --example dslogic`):
  - internal test mode, 16.7 million samples at 100 MHz, checked against the counter pattern:
    0 errors;
  - buffer mode, 10 million samples at 100 MHz;
  - a stop during a stream;
  - **1000 start/stop cycles** at random rates and stop times. All ended in 9 minutes, using 3.6 s
    of CPU (no busy waiting).
- Every demo, file and decoder check still passes. `drv-diff` was rerun after the shim changes,
  and the control transcript is identical with everything in Rust (the DSLogic attached and scanned
  by the Rust driver).

**Replug tests (2026-10-18, DSLogic Plus, staged DSView headless, usbmon recording):**
- **Cold start.** The device was plugged in while DSView ran with `DSVIEW_IMPL=rust`. The Rust
  driver found it through lib_main's attach event, DSView activated it, and the driver configured
  the cold FPGA (530,620 bytes). A 5 M-sample capture followed.
  - The same after a restart with the device freshly replugged, and with the C driver after a
    replug.
  - `usb-dump --dev` of the two cold opens plus captures: C and Rust send the same steps. The
    exceptions are F22's unset header offsets, and the kernel's SET_CONFIGURATION, which was
    recorded only on the side whose capture started before the plug-in.
- **Firmware upload: not testable with this device.** This DSLogic Plus boots its FX2 firmware
  from its own flash, so after a power cycle it comes up as "USB-based DSL Instrument v2"
  (firmware 2.2), and neither driver uploads anything.
- **Unplug during a stream capture, with Rust and with C:**
  - the outstanding transfers fail (a transfer error, then stalls);
  - the driver ends the capture (SR_DF_END) and keeps the data so far;
  - the final FORCE_RDY fails with "device disconnected";
  - the collect thread ends, lib_main handles the detach, and the device leaves the list;
  - DSView stays responsive and asks to save the data.

  Both drivers take the same steps in the same order; neither hangs.
- **Replug after that.** The device is found again at its new address. DSView activates it only
  after its "Save captured data?" dialog is closed, in C as well. That dialog cannot be closed over
  the control API, so the capture after the replug ran after a restart (above).
- Live stop in the middle of a stream, repeated with the 3e build: `usb-diff` reports the same
  protocol (53 vs 55 transfers before the stop).

**Open for 3d:**
- the firmware upload path (needs a DSLogic that loads its firmware from the host, e.g. an
  original DSLogic or a Pro);
- the three nightly hardware runs the plan asks for.

**Left for later in Phase 3:**
- Making the demo/file drivers the default (after one release of A/B, with Phase 1/2), then
  deleting `hardware/demo` and `session_driver.c`.

### 3e DSCope and MSO-E8: done (default stays C until a hardware confirmation)

**Shared DSL device** (`crates/dsview-drivers/src/dsl/device.rs`, = `dsl.c`):
- The DSLogic driver's shared half moved here, as `Dsl` with a `Family`.
- It gained the DSO and DAQ data paths:
  - transfer sizing per mode;
  - the `DSL_setting` divider and counters for DSO/DAQ;
  - `get_measure()` (the status block behind each DSO frame, read with C's index arithmetic);
  - instant mode's tail measurement;
  - the HMCAD1511 ADC power-down at the final stop.
- `dslogic.rs` and `dscope.rs` hold what `dslogic.c` and `dscope.c` add.

**DSCope driver** (`dscope.rs`; also serves the Hermelin MSO-E8):
- the 64-bit DSO commands (`dso_cmd_gen`, `dso_offset` with both offset schemes and comb
  compensation);
- open: the stored calibration (`dso_load_eep`, the COMB table on cold FPGAs), the HMCAD1511 set-up,
  `dso_init`;
- every config key, including ZERO/ZERO_DEFAULT/ZERO_LOAD/ZERO_SET (writing the calibration back),
  PROBE_VGAIN/PREOFF/COMB_COMP, the bandwidth limit, and the E8's attenuator and logic mode;
- the auto calibration (`dso_zero`: warm-up, offset-scale fix, per-range offsets, the comb stage,
  and the HMCAD1511 fine-gain loops) with its state per device (`ZeroCal`);
- start (ADC branch set-up, fine gains, trigger position, per-channel commands) and the progress
  polls.
- C behaviour is kept down to its numeric conversions: x86 truncation and masked shifts, through
  `c_i32`, `c_u16`, … in `dsl/mod.rs`. Departures: F24 and the table in DEVIATIONS.md.
- C seam: `rs_dscope_driver_info` (`DSVIEW_IMPL_DRV_DSCOPE=rust`); `rs_driver.c` shares one
  scan function between the two USB drivers.

**Simulated device** (`crates/dsview-usb/src/sim.rs`):
- `DslSim` speaks the DSL firmware protocol (registers, NVM, DSO command port, arm block, trigger
  header, logic/DSO/DAQ data).
- DSO frames carry a status block whose levels follow the offset commands, plus a settable
  front-end bias. A perfect device lets the calibration run to its end; a biased one keeps it
  correcting.
- Bulk IN completes in ticks. C ticks once per `libusb_handle_events*()`, Rust once per
  `bulk_in_wait` with a timeout, which gives both drivers the same batches.
- The oracle links `csrc/fake_libusb.c` instead of libusb, so the C drivers run on the same
  simulated device. `session.c`'s `g_poll` there lets the virtual clock run.
- The Rust drivers get the device through `dsview_ffi::drv::set_usb_backend` (`SimBackend`).

**Checks:**
- **`drv-diff` USB scenarios.** About a third of the generated scenarios pick a DSCope or E8
  profile, with or without stored calibration, a configured or cold FPGA, and a front-end bias.
  They then run random DSO/DAQ/logic settings, calibration captures, ZERO_SET/ZERO_LOAD round
  trips and captures, and compare every USB request either driver sent, besides all answers and
  packets.
  - Captures end after a fixed number of data packets or completed transfers, so both stop at the
    same point.
  - **3000 scenarios, all identical:** 4.2 million answers and USB requests, 215,000 packets.
    About 1060 of them were DSCope/E8 scenarios, over all 12 profiles. The scenarios cover
    calibrations running through all stages, biased devices that keep correcting, EEPROM write and
    read back, instant mode, DAQ streams, E8 logic captures, and captures whose DSO frames both
    drivers drop.
  - The C driver crashed or hung in 164 runs, all in the demo driver (F18, killed after the
    per-side time limit when it hangs), none on the simulated USB device.
- Bugs found along the way:
  - in the Rust port: an overflow panic on the E8's `bits = 0` channels;
  - in the shim: channel values the driver changes during a capture were not copied back, and the
    stop that C's session loop issues when a capture ends together with a stop request was missing;
  - in C: F24.
- **DSLogic after the restructuring** (live, DSLogic Plus):
  - the replay tests of the Phase 0 captures are byte-identical as before;
  - internal test mode, 16.7 million samples, 0 counter errors;
  - 300 start/stop cycles at random stop times, all ended;
  - a user stop in the middle of a stream through DSView: `usb-diff` reports the same protocol for
    C and Rust;
  - the control transcript is identical with `DSVIEW_IMPL=c` and `rust`.

**Open:**
- A DSCope or MSO-E8 owner runs the Rust driver once (`DSVIEW_IMPL_DRV_DSCOPE=rust`) before it
  becomes the default (plan, Phase 3 exit).
- 3d's firmware-upload and nightly hardware steps.

## Phase 4 (session + control API): status

### 4a acquisition library: done

**`crates/dsview-session`, `Library`** (= `lib_main.c` and the part of `session.c` the drivers
use):
- the device list (demo devices, then DSLogic, then DSCope, then capture files as they are opened);
- activation, with C's fallback to the previous device;
- `remove`, `release`, `close_all`, `reload`, the last error;
- the collect thread: start, the dummy-source loop, the stop (including the second stop C's session
  issues when a USB capture ends together with a stop request), and the end events;
- hotplug on its own thread, with C's 100 ms ticks and the 500 ms reconnect merge;
- the delayed destruction of a detached active device;
- events posted on threads of their own, as C does.

It uses the Rust drivers for every kind of device.
- `dsview-usb` gains hotplug (`UsbBackend::watch`; the nusb backend runs `nusb::watch_devices`), so
  this needs no libusb at all.
- The simulator can be unplugged and plugged in again, and can hold its data (a capture waiting for
  its trigger).

**Seam:**
- `dsview-ffi::acq` (`dsv_lib_*`) and `libsigrok4DSL/ds_api_rs.c` implement the `ds_*` API on the
  library. With `DSVIEW_IMPL_ACQ=rust` (or `DSVIEW_IMPL=rust`), `lib_main.c`'s entry points hand
  over to them, and `sr_session_stop()` asks the Rust capture to stop.
- The GUI still sees a `struct sr_dev_inst` with an `sr_channel` list per device. `rs_driver.c`
  makes these copies; the library calls back to copy channels in before it uses a device and out
  after it changed one, to forward packets and events, and when a device is gone.
- The config, status, mode and channel calls work on the active device's copy, as `lib_main.c`
  does on its `sr_dev_inst`.

**Checks:**
- `drv-diff` now runs the Rust side entirely on the Rust library: demo, file and simulated
  DSCope/E8 scenarios through the `ds_*` API. C library + C drivers against Rust library + Rust
  drivers, **1500 scenarios, all identical**: 2.1 million answers, 108,000 packets.
  - 70 of them hit the known C demo crashes (F18), where Rust matched up to the crash.
  - Found on the way: a stop requested from a capture's first packet was lost (the flag was
    registered after the thread started).
- Hotplug, against the simulated DSLogic Plus (`tests/hotplug.rs`):
  - a 150 ms loss of contact: the same device at its new address, opened again, no detach event;
  - a real detach of the active device: out of the list after 500 ms, the active one kept;
  - a new device: the attach event and a new entry;
  - unplugging during a capture: the capture ends, END is sent, the capture ends with
    COLLECT_TASK_END_BY_DETACHED.
- The Qt GUI (headless DSView) on the Rust library:
  - the control-API transcript is identical to the C library's;
  - with the DSLogic Plus, the startup and a user stop in the middle of a stream give the same
    USB protocol as C (`usb-diff`; 52 vs 53 transfers before the stop).

- **Replug with the Rust library through DSView** (2026-10-18, DSLogic Plus):
  - Unplugging during a stream capture: nusb's hotplug stream reported it, and the capture ended
    with SR_DF_END, keeping its 20.7 million samples. The collect thread stopped, the device was
    closed and left the list 500 ms later (delayed destruction of the active device). DSView stayed
    idle and asked to save.
  - Plugging it back in: the attach event, a rescan, the device listed at its new address.
    Activation waits for the save dialog, as with C.
  - After a restart, the freshly powered device opened with its FPGA configured cold (530,620
    bytes), and a 10 M-sample capture followed.

### 4b capture session, control server, `dsview-cli serve`: done

**`dsview-session::Session`**: DSView's GUI capture logic without the GUI. It covers
`SigSession` with the parts of `DeviceAgent`, `SamplingBar` and `TriggerDock` that drive it:
- **Devices:** device selection (fresh signals per device), mode switch, signals per mode, and
  `reload()`, which keeps the names and triggers of the signals that remain.
- **Profiles (`load_device_config`/`save_config`):**
  - Hardware settings are kept per device and mode in the Qt GUI's profile directory
    (`$XDG_DATA_HOME/DreamSourceLab/DSView/<driver><mode>.dsc`, the same files the GUI uses).
  - Without a saved profile the default from `res/` is used.
  - The demo oscilloscope loads its shipped profile.
- **Sampling bar:** the rate and depth selectors. The depth list starts at `hw_depth / rate` and
  steps down 5-2-1. `commit_settings` rounds up to 1024 samples (1000000 → 1000448). DSO
  horizontal resolution comes from the timebase.
- **Trigger:** the simple trigger per logic signal, with the dock's position range (1..90, or
  less when the depth is short) put into `ds_trigger` before each capture.
- **Capture:** single, instant, repeat and loop.
  - Repeat mode captures into a second buffer while the first is shown and decoded (C's two
    `SessionData`).
  - Loop mode streams into the logic store's ring.
- **Datafeed:** into the logic/DSO/analog stores, with the trigger position, the error states and
  the DSO status block.
- **Decoding:** a decode queue on its own thread; the decoders run when a logic capture ends.

**`dsview-session::decode::DecoderStack`**, the `pv/data/decoderstack.cpp` part:
- rows and their titles;
- annotations routed by (decoder, class);
- text interning (`AnnotationResTable`);
- the `{$}` number with the hex default;
- progress, error messages, the chunking over the store's leaves.

**`dsview-session::store`**:
- the decoder JSON of `.dsl`/`.demo` files (`StoreSession::load_decoders` with its option typing,
  `gen_decoders_json`);
- a capture file's session (`load_config_from_json`) and generating it (`gen_config_json`);
- the demo patterns' decoders;
- saving `.dsl` (logic and DSO);
- CSV/VCD export of logic data, CSV export of DSO and analog data.

**`dsview-control`**:
- `server`: the socket of `controlserver.cpp`. Same rules: path from `DSVIEW_MCP_SOCKET` or the
  XDG/home default, a 107-byte limit, the directory owner-only, never taking over a socket another
  instance listens on, one JSON-RPC 2.0 request per line, 1 MiB per line, the same error codes and
  messages. Numbers are written the way Qt writes its doubles.
- `api`: all 31 methods of `controlapi.cpp`, with the same parameters, checks, messages and answers.
  Methods run one at a time, as on Qt's GUI thread. Captures, decoding and saving wait outside that
  lock.
- `measure`: the DSO measurements of `DsoSignal::get_measure`. They come from the device's status
  block when it is valid, else from the software fallback over the samples.

**`dsview-cli`**:
- `serve --mcp`: DSView without Qt, behind the same socket. `dsview-mcp` drives it like the GUI.
- `soak`: long loop or repeat captures, with the resident memory logged every minute.

**Checks:**
- **Control transcript** (`testdata/control/demo-scenario.json`, 38 steps, all 31 methods):
  - `dsview-cli serve` passes `cargo xtask control check`.
  - The Qt GUI on the Rust core (`DSVIEW_IMPL=rust`) produces the same transcript, every step
    identical. That includes the four DSO steps, which the check compares only by shape: same
    waveform millivolts, same measurement strings.
  - The Qt GUI on the C core differs only in the order of `list_decoders`: the decoder load order
    (F25, below).
  - The transcript was re-recorded from the Qt GUI for this. The old one had the decoder and row
    order of an earlier install of the decoder directory.
- **Every annotation, C GUI against Rust:**
  - The Qt GUI on the C core (`DSVIEW_IMPL=c`: SigSession, DecoderStack and the C decoder engine)
    against `dsview-cli serve`. Both captured the demo, then dumped all four decoders' annotations
    through `get_annotations`.
  - All 8,379 annotations are identical: start, end, row title and text. So are the decoder
    states (stack, progress, counts) and the row sets.
- `crates/dsview-control/tests/transcript.rs` runs the same transcript in-process over a real
  socket: the repository's demo files and decoders, no USB.
- `crates/dsview-session/tests/session.rs` covers:
  - the depth rounding;
  - a single capture with the four protocol.demo decoders (405/517/5192/2265 annotations, as in
    the Qt GUI);
  - repeat mode until stopped;
  - loop mode into the ring;
  - a DSO single shot.
- **DSLogic Plus through `dsview-cli serve`:**
  - a 10 MHz capture of 1000448 samples (16 channels);
  - the device's default profile and the depth rounding in stream mode, step for step the same as
    the Qt GUI's (2,000,000 → 1,000,448 at 5 MHz, 50,000,000 → 40,000,512 at 20 MHz);
  - settings kept across a restart in the profile.
- **`dsview-mcp` against `dsview-cli serve`:** the MCP tools run unchanged: device selection, a
  capture with the demo decoders, annotations, measurements. With `DSVIEW_BIN=dsview-cli`,
  `dsview-mcp` starts the headless DSView itself (`dsview-cli serve --mcp`).
- `dsview-mcp` takes the socket path from `dsview-control` (the `api` feature off, so no session and
  no Python in it).
- **DSO by hand:** a continuous capture until `stop_capture`, V/div stepping, coupling, saving as
  `.dsl`, CSV export, and reopening the saved file.
- **Analog by hand:** a capture stopped after 3 s, then its CSV export (1,000,448 rows).
- **Soak** (`dsview-cli soak`; RSS sampled every 5 s after a 5-minute warm-up; the gate is the
  least-squares trend over the run):
  - **1 h loop mode** on the demo's random pattern: one capture into the ring for the whole hour.
    - RSS mean 78 MB, trend **−3.1 %** over 55 minutes (661 samples).
    - The RSS alternates between two levels 32 MiB apart, one row of 2 MiB leaves for 16
      channels, depending on where the ring stands.
  - **1 h repeat mode** with the four protocol.demo decoders: 36,832 captures, each decoded.
    - RSS mean 101 MB, single samples 66-195 MB, trend **−6.4 %**.
  - **15 min loop mode on the DSLogic Plus** (stream mode, 1 MHz, 16 channels): RSS 61.5 MB at
    the start, +0.12 % at the end.

## Phase 5 (Rust GUI): status

### M6.0 prerequisites: done

**DSO envelope in `dsview-store`** (F26):
- Ported: 256× per level, ten levels, `enable_envelope`, `get_envelope_section`, and the
  `samplerate_tog` rebuild, with C's quirks.
- The C++ `DsoSnapshot` hands over to it through `dsv_dso_enable_envelope` /
  `dsv_dso_envelope_section`.
- `store_ad_diff` compares envelope sections against the C++ class: 3000 cases, all identical.
  A deliberately wrong max was caught at once.

**Annotation index in `DecoderStack`:** range queries look only at starts in
`(start − longest, end]`, in arrival order as before. Checked against the scan on 3000 annotations;
the demo transcript is unchanged.

**`SessionEvent`:**
- `Session::subscribe()` reports device and mode changes, capture start/end, new data, decode
  progress/done, and errors.
- Data and decode events are limited to 30 Hz.

**`ViewHooks`** for `dsview-control::api`:
- a GUI's open dialog refuses the mutating methods, with Qt's message;
- `show_region` and `set_cursors` reach the view.

**`dsview-viewmodel`** (new):
- `viewport`, ported from `View`: scale, integer pixel offset, limits, zoom around a point,
  `show_region`, auto-scroll, index↔pixel;
- `logic`, ported from `LogicSignal::paint_mid_align`: toggles or per-pixel pulses → lines, and
  sample dots.

**`dsview-dsp`** (new):
- the DSO measurements (moved from `dsview-control`);
- `spectrum`, from `SpectrumStack`: the five windows with Qt's `PI = 3.1415`, and the power
  spectrum on realfft.

**`dsview-gui`** (new, binary `dsview`):
- a preview window showing the demo capture through the viewmodel, with wheel zoom and drag pan;
- `--bench SECONDS` logs frame times.
- Results on this VM over xrdp are in PLAN.md §5.1. The decision is glow, logic traces as
  rectangle meshes without feathering, 13 frames/s at 1080p. Presenting an empty window alone
  allows 17.5.

### M6a viewer: core done

**`dsview-viewmodel`** (18 tests): each module was ported from the Qt code path.
- `ruler`: the tick algorithm and time, sample and frequency formats.
- `measure`: the pulse under the mouse, edge counts, edge snapping (`View::Unknown_Str` is
  `########`).
- `cursors`: numbered by position, Qt's hue table, drag with snapping to an edge within 10 px.
- `layout`: row height fitting, view-index order.
- `input`: wheel mapping with the `verticalScrollIsZoom` swap, Ctrl+Shift vertical zoom,
  horizontal pan, kinetic scroll after a swipe, right-drag rubber-band zoom.

**`dsview` (Rust GUI):**
- Opens `.dsl`/`.demo` files (File menu, native dialog through the xdg portal) or a file given on
  the command line.
- Saves `.dsl`, exports CSV/VCD.
- Logic traces with Qt's default colours, channel dividers, sample dots.
- The ruler with time labels relative to the trigger, and the hover arrow.
- The header: colour swatch (colour picker), name (double-click renames), the five trigger
  squares, drag to reorder (view indexes).
- Zoom at the pointer, pan, kinetic scroll, rubber band, right double-click for full/previous view,
  PgUp/PgDn, ←/→.
- Cursors:
  - left double-click adds one, snapped to an edge within 10 px;
  - drag on the trace or the ruler label;
  - right-click on the label deletes it.
- Mouse measurement in the status bar (width, period, frequency, duty).
- Clicks: on an edge, a jump measurement to the next edge clicked; elsewhere, edge counting to
  the pointer.
- **Control API:** `dsview --mcp` passes `cargo xtask control check` (38 steps). `show_region` and
  `set_cursors` drive the view; an open file dialog refuses mutating calls with Qt's message.
- **Snapshot:** `tests/snapshot.rs` renders the protocol.demo view off-screen (egui_kittest, wgpu)
  and compares it with `tests/snapshots/protocol_demo_view.png`.

**Still to come in M6a's area:**
- cursor time labels in the ruler (Qt prints the time next to the number);
- the ruler's cursor-selector strips;
- the Y-scale badge;
- a vertical scroll bar (the wheel scrolls already).

### M6b logic capture: done
- **Sampling bar:**
  - device list, work mode, channel enable (8 per row);
  - duration list (the depth steps, "(RLE)" or "/ div" as in Qt) @ rate list;
  - collect mode Single/Repeat (with interval)/Loop;
  - Start/Stop (S), Single (I);
  - capture progress, "Waiting for trigger".
- **Simple trigger:** the header's trigger squares, put into `ds_trigger` at the start.
- **While streaming:** the view follows the data at up to 30 Hz (`SessionEvent::DataAppended`,
  `is_realtime_refresh`).
- **Hotplug:**
  - "The current device was disconnected." and a switch to the default device;
  - a new device asks "Switch to it?".
- **Profiles:** as in 4b (the Qt GUI's profile directory).
- **Hardware matrix through the Rust GUI** (`dsview --mcp`, `cargo xtask usb record` with the
  DSLogic Plus):
  - all 11 baseline scenarios give the same USB protocol as the C driver under Qt (`usb-diff`);
  - stopping a stream midway gives the same protocol, with 56 vs 52 transfers before the stop
    (timing).
- **GUI soak** (`dsview --soak 60`, the demo's random pattern in loop mode with the window
  open): RSS trend +0.15 % over 55 minutes (gate 1 %); the ring's two levels (190 and 228 MB)
  alternate as in the 4b soaks.

### M6c decoders: done
- **Decoder traces in the view:**
  - rows with annotations, coloured as in Qt (row colours, HSL offsets per row and annotation
    type);
  - hexagons, instants, the best-fitting text form;
  - "Zoom in for details" below the detail threshold;
  - row headings, errors, "Unshown" decoders;
  - placed among the channels by view index.
- **Protocol dock (D):**
  - **Decoder search:** stacked decoders bring their base decoders, as Qt does
    (`get_output_protocol_by_id`).
  - **Options dialog:** channel binding (required marked, matched by name), option values as
    lists or text. The same dialog edits an existing stack in place ("Settings").
  - **Decoder list:** progress, decode again, remove.
  - **Annotation table:** virtual, Qt's model (one column per row shown in the list, bits and
    warnings hidden).
  - **Search and navigation:** search in the chosen column with match count and previous/next;
    a click jumps the view to the annotation.
  - **CSV export:** in `ProtocolExp`'s format, tested on the demo.
- **Live decoding:**
  - a single stream capture (demo, files, DSLogic stream) is decoded while it runs, as `SigSession`
    does;
  - the decoders wait for data and end with the capture;
  - constant leaves stay until the decoder has passed them.
  - All 8,379 demo annotations remain identical to the C GUI's.
- **Snapshots:** the protocol.demo view with the decoder rows and the protocol dock
  (`tests/snapshots`).

### M6d DSO and analog: done (no oscilloscope hardware here)
- **DSO traces** (`dsview-viewmodel::dso`, a port of `DsoSignal`):
  - a polyline up to 2 samples per pixel, with dots from 10 pixels per sample;
  - one bar per pixel column up to 256;
  - the store's envelope above.
  - Coordinates follow the C++ float/int types, so shapes match pixel for pixel; unit tests
    cover each path, the grid and the header geometry.
- **The view:**
  - grid with the zoom marker; split bands (View menu);
  - zero lines (drag the label in the header) and trigger levels (drag the square);
  - the trigger's sub-sample offset;
  - wheel zoom through the timebase list (`WheelAccumulator`);
  - the value under the mouse;
  - the X measurement (double click, two clicks: width, period, frequency, duty);
  - "Out off range!" on hardware.
- **Header per channel:** V/div dial (click or wheel on its right half; the trace stretches while
  stopped), probe ×1/×10/×100, EN/DIS, AC/DC, AUTO (hardware).
- **Auto-set** (`dsview-viewmodel::autoset`): Qt's state machine. Timebase and V/div steps, the
  MSO-E8 attenuator, the clip back-off, three frames' lock after each step, a 10 s timeout. Unit
  tests only: AUTO shows on hardware, and the DSLogic Plus here has no DSO mode.
- **Persistence:** faded ghosts of the frames before, remapped on zoom/pan (setting in the View
  menu; off by default as in Qt).
- **Analog (DAQ):** rows with the value rule, polyline or envelope over the loop ring, zero drag,
  value under the mouse.
- **Docks and panels:**
  - DSO trigger dock (T): position, source with the auto channel, slope, hold-off, sensitivity;
  - measurement bar: ten slots, channel and type chooser, the twenty measurements shared with
    the control API (`dso_measurements`).
- **Functions menu:**
  - **FFT:** a pane under the traces, linear or dBV, zoom/pan, rulers, hover; options dialog.
  - **Math:** 11 operators, own dial and zero line, envelope.
  - **Lissajous:** replaces the traces while on.
  - **Reference waveforms:** the frozen calibration mapped onto the current V/div.
  - **Histogram** (values, periods, jitter) and **channel-to-channel** measure (delay, skew,
    phase).
- **`dsview-dsp` against the C++** (`dev/dsview-oracle`, `tests/dsp_diff.rs`):
  - **Power spectrum on FFTW:** worst error 4.6e-16 of the peak over 200 spectra (every
    window, length and interval; gate 1e-9).
  - **Math results and their envelope levels:** bit-identical over 220 cases.
  - **Dial defaults and values:** identical in 3564 cases.
  - **Sensitivity:** mutating π back to its true value, or the filter's divisor, fails the tests.
- **GUI tests:** 14 egui_kittest tests. DSO, analog, FFT pane, math, Lissajous, references and the
  analysis dialogs are snapshots; the trigger dock and measurement bar too. Interaction tests
  cover the probe factor, the dial, wheel zoom, the X measurement, and persistence (a new frame
  leaves a ghost, a V/div step drops it).
- **Not here:**
  - the trigger-wait animation (M6b's item, with the capture status);
  - calibration and its start-up prompt (M6e);
  - shortcuts 0/1 and ↑/↓ for the DSO channel (M6f);
  - application options for line width and persistence (M6f; persistence is in the View menu
    meanwhile).

### M6e docks and dialogs: done; the Rust GUI is the default on Linux
- **Triggers:**
  - advanced (16 stages: X/0/1/R/F/C, and/or, invert, counter, contiguous) and serial trigger
    in the session (`commit_trigger`) and the trigger dock (T);
  - the oracle compares 3000 random dock states with C's `ds_trigger_*` (all identical).
- **Docks:**
  - measure dock (M): mouse-measurement switch, cursor distances, edges between cursors, the
    cursor list with go-to and delete;
  - search dock (R): pattern dialog, next/previous in a thread with Cancel, the search cursor
    (draggable).
- **Dialogs:**
  - device options generated from the driver's option list, channel mode and channels;
  - manual calibration (VGAIN/VOFF/VCOMB) and auto calibration with the start-up prompt;
  - save/export with a cursor range, original or compressed CSV, and a progress bar with Cancel
    (a cancelled save discards the file; F29 on ranged `.dsl` files);
  - protocol export (CSV/TXT, choice of rows) and the protocol list options (map zoom fit/fixed,
    protocol, rows listed);
  - application options, all applied: font size, line widths, decoder font width (F29),
    trigger position in the middle, profile in the title, swap back buffer, don't ask to save;
  - File menu: profiles (load, store, default) and screenshot.
- **Decoders (M6c items):**
  - per-protocol number format;
  - decode region;
  - decoder and row show buttons;
  - the protocol list's mark and the type-2xx marks on bound channels.
- **The viewer gaps (M6a/b items):**
  - ruler cursor labels with time and close box, "T" and "S" labels;
  - the cursor selector and go-to strips;
  - Y-scale badge;
  - header multi-select and 10 px grid drag;
  - pinch zoom;
  - mouse-measure arrows and popup;
  - hover snapping;
  - the capture progress and trigger-wait screen;
  - "Save captured data?" on device change, detach, new device and close.
- **Profiles:** `<driver><mode>.ses.dsc`, the file Qt DSView uses (before this the Rust session
  wrote `<driver><mode>.dsc`); older per-language files are read when it is missing.
- **Tests:**
  - 33 egui_kittest snapshot and interaction tests;
  - 2 window tests (`tests/app_flow.rs`) on a simulated DSLogic that drive `App` itself (the
    store question on a device change and on close);
  - session tests for profile names, store progress and cancel, protocol export cancel.
  - The kittest accessibility check found and got labels for the decoder options and sampling
    bar widgets.
- **MCP suite (exit):** `cargo xtask mcp suite target/debug/dsview`: 30 tools, 37 steps green;
  identical to `dsview-cli` and to Qt DSView on the Rust core in all 37 steps.
- **DSLogic Plus, through the Rust GUI:**
  - a trigger on channel 0 with nothing connected shows Qt's wait screen ("Waiting for Trigger!
    0% Captured", blinking dots, from the device's status);
  - a 1 s capture at 1 MHz fills the view.
  - That check found the first-view scale bug (F29), now fixed.
- **Default flip:**
  - CMake option `DSVIEW_RUST_GUI` (on by default on Linux) builds `crates/dsview-gui` with
    cargo and installs it as `bin/DSView`;
  - the Qt GUI installs as `bin/DSView-qt` with a "DSView (Qt)" desktop entry, for one release
    (PLAN §1.2);
  - without cargo the Qt GUI stays `DSView`.
  - `build.yml`'s deb/AppImage jobs have no Rust toolchain and stay Qt (DEVIATIONS, departures).
- **Not here (M6f):** the logo bar, about, help, i18n, themes and icons (with the display menu's
  theme choice), shortcuts O, 0/1, ↑/↓, Esc and the debounce, settings import and window state,
  `-s`, AccessKit in the binary (needs `accesskit_winit`, not in the offline registry), dock
  layout per mode, log options.
