# Migration status

| Phase | State | Notes |
|---|---|---|
| 0 Infrastructure, oracle, baselines | **done, one item partly deferred** | Performance baselines: `display_edges` and the aggregate decode rate were measured in 1b/2; per-decoder throughput and 1G samples are follow-on (PLAN §2). USB traces recorded 2026-10-17 |
| 1 Formats + store | **done**: 1a formats behind `DSVIEW_IMPL_FORMATS`, 1b sample stores behind `DSVIEW_IMPL_STORE`; Rust becomes the default after one release of A/B | See "Phase 1a" and "Phase 1b" below |
| 2 Decoder engine | **done**: Rust/PyO3 engine behind `DSVIEW_IMPL_DECODE`; Rust becomes the default after one release of A/B | See "Phase 2" below |
| 3 Drivers | **done except hardware steps**: 3a demo, 3b file, 3c USB layer, 3d DSLogic (replug tests passed 2026-10-18), 3e DSCope/MSO-E8 (`DSVIEW_IMPL_DRV_DEMO` / `_FILE` / `_DSLOGIC` / `_DSCOPE`). Open: a firmware upload from the host, the nightly runs, one DSCope/E8 hardware confirmation | See "Phase 3" below |
| 4 Session + control API | **done**: 4a acquisition library in Rust behind `DSVIEW_IMPL_ACQ`; 4b capture session, control server, `dsview-cli serve --mcp` (transcript identical to the Qt GUI, 1 h soaks passed) | See "Phase 4" below |
| 5 Rust GUI | **done**: M6.0-M6g; release R1 merged with M7a as DSViewR 1.0.0 (2026-10-19) | PARITY.md 52 of 52; AccessKit live over AT-SPI |
| 6 Removal + packaging | **M7a done 2026-10-19**: the Qt GUI and all C/C++ code removed (tag `c-final` keeps them), cargo-deb and AppImage packages of DSViewR 1.0.0 checked on Ubuntu 22.04/24.04/26.04 and Fedora 44. Open: M7b post-parity fixes; merge, tag and release (user) | See "M7a" below |

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
      tests"). It was checked by behaviour, not recorded as pcapng: a recording needs someone to
      pull the cable during `usb record`. That is on the R1 manual checklist;
    - firmware upload cannot be tested with this device (follow-on);
    - the test-counter mode is checked through the Rust driver by `dsview-cli verify-counter` and
      `hwmatrix` (M6g step 4). A C trace of it is dropped: Qt's control API cannot select the
      operation mode, and the Rust check needs no reference.

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

### M6f polish: done 2026-10-19
**Step 1, settings and the `DSView.conf` import (done 2026-10-18):**
- **Model:** `AppOptions` (in `crates/dsview-gui/src/options.rs`) now holds all of Qt's `AppConfig`:
  - every `[Application]` key;
  - `frame`: style, language, maximised, the normal window rectangle, docks per work mode;
  - `history`: the directories per file dialog, `showDocuments`, `showDriverHint`.
  - It is stored as JSON in `dsview-rs.json`.
- **First-run import:**
  - Without `dsview-rs.json`, the Qt `DSView.conf` next to it is read once and saved as JSON.
  - The reader is `dsview_viewmodel::qsettings`, a QSettings-INI reader: groups, `a\b` keys,
    `%XX`, quoted values with escapes.
  - `_loadApp`'s clamping is applied: font size 8-16 (and the middle for version-1 files), trace
    height 0.1-20, line widths 1-4, persistence 0-10000.
  - The language comes from the locale when none is stored.
  - Checked on the real `DSView.conf` of this machine, in a copy.
- **Window:** the last normal rectangle and the maximised state are restored. A window that shows
  less than 70 × 70 px of the screen moves to its middle (Qt's check). The rectangle is saved when
  the window closes.
- **Persisted view state:** trace height factor and DSO split.
- **File dialogs:** each starts in the directory its kind used last: open, save, export, sessions,
  screenshot and protocol export, as Qt's `UserHistory`.
- **Saving is opt-in.** Only settings that were loaded are written, so tests and benchmarks never
  touch the user's files.
- **Bug fixed from M6e:** "No" to "Save captured data?" while closing now closes the window, as Qt's
  `able_to_close` does.
- **Found while testing:**
  - `XDG_CONFIG_HOME` is set in this environment, so a HOME override does not isolate a DSView
    instance.
  - This session's MCP suite runs had written the real `DSView.conf` (Qt) and the DSLogic profile
    (on a device switch).
  - `cargo xtask mcp suite` now gives the instance its own `XDG_CONFIG_HOME`/`XDG_DATA_HOME` with a
    seeded `DSView.conf`. A suite run leaves the real directories unchanged (checked).
- **Tests:**
  - the QSettings reader (4 tests);
  - the import with clamping, the locale language and the JSON round trip;
  - option application;
  - `app_flow`: "No" sends the window's Close.

**Step 2, log (done 2026-10-18):**
- **`crates/dsview-gui/src/log.rs`** ports `pv/log.cpp` over xlog:
  - messages up to the level (0-5) go to stderr and, once enabled, to `~/DSView.log` as
    `DSView: <message>`;
  - the file opens once, appending or overwriting;
  - it can be switched off and on;
  - Clear empties an open file and deletes a closed one.
- **Start-up as `main.cpp`:**
  - `-s/--storelog` raises the level to debug and appends to the file;
  - a saved "Save To File" opens it in the saved mode, and the saved level wins when it is higher;
  - `-l3`/`--loglevel=3` are accepted too;
  - a second file argument ends with Qt's "Only one file can be openened." and exit code 1.
- **Help > Log Options:** level, Save To File, Append mode, Open (system viewer, `open` crate) and
  Clear (confirmed).
- **App id:** the Wayland app id and X11 class are now `dsview`, as Qt's
  `setDesktopFileName("dsview")`. The Rust GUI's desktop entry matches it and no longer lists
  `Qt;`.
- **Tests:** the log file, levels and clear (with a redirected path), and the dialog through the
  window.

**Step 3, Help menu and messages (done 2026-10-18):**
- **Help menu** (Qt's logo bar, last in the menu bar):
  - About: version and architecture, website, copyright, fork and thanks, with working links;
  - Manual: `ug<language>.pdf` in the data directory, the English guide when the language has none;
  - Bug Report and Update open their GitHub pages;
  - Log Options;
  - the Language submenu follows with i18n (step 6).
- **Opening files and links:** through the `open` crate. egui's hyperlinks would need eframe's
  `links` feature.
- **`msgbox`:** `MsgBox::Show` ("Message", OK) and `MsgBox::Confirm` ("Confirm", Yes/No) as
  centred, non-blocking windows. These now use them:
  - the app's messages;
  - "Save captured data?";
  - the new-device question, now Qt's "To switch the new device?";
  - the log's Clear.
- **Tests:** the manual lookup, and About through the window.

**Step 4, shortcuts, and docks per work mode from step 5 (done 2026-10-19):**
- **One key path, as `MainWindow::eventFilter`:**
  - the table is `dsview_viewmodel::input::shortcut`;
  - `KeyGate` drops keys with Ctrl, Shift or Alt and any key less than 200 ms after the last one
    taken;
  - nothing reacts while a text field has the keyboard.
- **Keys:**
  - S, I: run/stop, instant;
  - T, D, M, R: the docks;
  - O: device options;
  - PgUp/PgDn: one view width;
  - ←/→: zoom, a timebase step in DSO mode;
  - 0/1: the active DSO V/div dial, only on an enabled channel;
  - ↑/↓: that dial.
  - The time view no longer reads keys itself.
- **Docks per work mode (`TrigBar::reload`):**
  - logic offers trigger, protocol, measure and search; analog only measure; DSO trigger and
    measure;
  - T/D/M/R and the View menu touch only the offered docks;
  - each mode reopens the docks it was left with, saved in the settings.
- **Esc** closes every dialog: Cancel buttons and windows with a close button.
- **Tests:**
  - the key table, the gate and the dock table;
  - a window test on the simulated DSLogic and the demo oscilloscope: Ctrl ignored, debounce, docks
    per mode, 0/↑/↓ on the V/div, O.

**Step 5, start-up dialogs (done 2026-10-19):**
- **The hardware driver note** (`show_driver_hint_once`): shown when no
  `60-dreamsourcelab.rules` is installed in `/usr/lib`, `/lib` or `/etc/udev/rules.d`. With a
  copy of the rule in the data directory it gives the `sudo cp` command, otherwise the rule line.
- **The document hint** (`show_doc`): `showDoc<language>.png` from the data directory, or the one
  built in from Qt's resources (German shows the English one). Buttons: "Not Show Again",
  "Ignore", "Open" (the user guide).
- **Both:** "Not Show Again" is saved in the settings. They appear only for loaded settings, so not
  in tests or benchmarks.
- **Tests:** the hint texts, and the sequence through the window.

**Step 6, i18n (done 2026-10-19):**
- **`crates/dsview-gui/src/i18n.rs`** reads `lang/<cn|en|de>`:
  - the toolbar, msg and dlg pages;
  - for Chinese also the device texts (`dsl_*.json`) and the decoder texts (`dec/*.json` by
    `idn`).
- **Lookup:** `tr()` finds a GUI text through Qt's English text of the same id, compared without
  `&` accelerators, a trailing ellipsis or colon, and case. The ellipsis or colon of the Rust text
  is kept. Texts Qt does not have stay English.
- **Applied to:**
  - about 230 GUI texts (buttons, labels, menus, window titles, combos, hints);
  - the message boxes, which translate their text;
  - the device options (DSL page) and the decoder dialog (channel and option descriptions by
    `idn`, with Qt's "Translate param names" switch for Chinese).
  - German has no device or decoder texts, as in Qt (`is_lang_en()`).
- **Labels aligned to Qt's wording** where both name the same thing:
  - File menu: Load Session, Save Session, Load Default Session, Capture;
  - Function menu;
  - "Repetitive";
  - the docks: Trigger, Decode, Measure, Search, with their keys;
  - the decoder channel rows: "name (description) *".
- **Help > Language:** English / 中文 / Deutsch, switched at run time and saved. The older
  per-language profile `<driver><mode>.ses<N>.dsc` of the current language is preferred.
- **Fonts:** a system CJK font is added as the last fallback (Droid Sans Fallback, Noto Sans CJK SC
  face, or WenQuanYi). The language menu's 中文 needs it in every language.
- **Tests:**
  - accelerator and ending rules, and the three languages through local stores;
  - `tests/i18n_flow.rs`, in its own binary: switching to Chinese and back through the menu, with a
    snapshot; skipped without a CJK font;
  - the per-language profile choice.

**Step 7, themes and icons (done 2026-10-19):**
- **`theme.rs`:** dark, light, latte and frappé.
  - egui visuals in each theme's colours;
  - a `Palette` of view colours by role (background, header, ruler, deep plot background, text,
    ticks, dim, muted, faint, dividers, accent), threaded through the logic, DSO, analog, FFT, math
    and Lissajous views;
  - the dark palette keeps the colours the views had, so all 33 snapshots are unchanged.
- **Display menu (Qt's):** Themes (saved as `style`) and Options…
- **`icons.rs`:** Qt's dark and light SVG sets (`GetIconPath`: dark for dark and frappé),
  rasterised with resvg/usvg 0.44 at the drawn size and cached as textures. Used on the toolbar
  (options, start/stop, single) and the File, Function, Display and Help menu entries.
- **Tests:**
  - every icon rasterises and draws something, in both sets;
  - the theme styles round-trip;
  - `tests/theme_flow.rs`, in its own binary: Display > Themes > Latte through the window, with a
    snapshot of the demo capture.
  - The shortcut test is now deterministic: both keys in one frame, since kittest gives each queued
    key its own frame.
- **Font licence (decided 2026-10-19):** `cargo deny check licenses` failed on egui's built-in
  Ubuntu font (`epaint_default_fonts`: OFL-1.1 AND Ubuntu-font-1.0), present since M6.0. The user
  chose to allow `Ubuntu-font-1.0` in `deny.toml`; `cargo deny check licenses bans sources` passes.

**Step 8, accessibility (done 2026-10-19):**
- **Audit:** `app_flow::accessibility_of_the_dialogs` opens every dialog through the window, and
  kittest's accessibility check fails on any widget without a name. The dialogs covered:
  - logic: Application Options, Channels, Device Options, Search Options (from the search dock) and
    the decoder options (from the picker);
  - DSO: FFT, Math, Lissajous, Reference, Histogram, Channel-to-channel measure and Device Options.
- **Fixed by it:**
  - the FFT and Math option rows, now labelled by their row text;
  - the search fields that showed only a hint, now named text inputs.
- **The window tests load the decoders**, so the decoder picker has entries there.
- **Screen readers:** eframe's `accesskit` feature is on (`accesskit_winit` 0.34 and its
  AT-SPI side, zbus in pure Rust, fetched once with the user's consent 2026-10-19; no existing
  locked version changed).
  - Checked live on a private session bus with AT-SPI enabled: `dsview` appears as an application.
  - The menus, toolbar buttons, combos and measurement slots carry their names.
  - The trace area's painted parts show as unnamed elements.
- **PARITY.md is at 52 of 52.** The command-line row is ticked: `-l`, `-v`, `-s`, `--mcp` and the
  file argument have been in place since step 2.

### M6g hardening: done 2026-10-19
**Step 1, runtime (done 2026-10-19):**
- **Bundled Python:**
  - `dsview` and `dsview-cli` carry the RUNPATH `$ORIGIN/../lib/dsview`;
  - `dsview_decode::python` finds `<prefix>/lib/dsview/python3.X/encodings` and starts the
    interpreter through `PyConfig` with that home and search path (stdlib, `lib-dynload`).
    This replaces the Qt packages' `PYTHONHOME`/`PYTHONPATH` wrapper. A set `PYTHONHOME`
    still wins.
  - `tools/package/bundle-python.sh PREFIX BIN [PYTHON]` copies libpython and the standard
    library into that layout. It checks that the binary links the same version.
  - Checked: a staged `usr/` with `dsview-cli` and pyenv's 3.12 bundle (62 MB), started with
    `env -i` and no `LD_LIBRARY_PATH`. It loads the decoders, and `/proc/<pid>/maps` shows
    libpython and every `lib-dynload` module from the bundle, nothing from pyenv.
- **Device errors (`SigSession::set_device`):**
  - The GUI shows Qt's messages for a failed switch: firmware file missing, device busy,
    firmware version, USB I/O error, no driver.
  - `set_device` returned true for every code, because it checked for codes 1000-1099, which do
    not exist (the codes are 10-14). Fixed. The control API's `select_device` now reports
    "could not open the device" as Qt's does.
  - A missing FPGA bitstream is reported as a missing firmware file (F31).
  - Tests: a cold simulated DSLogic without its bitstream; the message table.
- **Decoder options from files:**
  - `objdump`, `elffile` (`arm_itm`, `arm_etmv3`) and `mapping` (`numbers_and_state`) from a
    `.dsl` or a loaded `.dsc` are held at their defaults.
  - The GUI asks per decoder ("The file sets decoder options that run a program or read a file
    …"). Yes reloads the trace in its place with them and decodes it again.
  - DSView's own profiles and the demos load as before.
  - Tested in `session.rs`.
- **DSO voltages in the control API:** the V/div of the capture, recorded when it starts, as
  Qt's `set_measure_voltage_factor`. Changing V/div afterwards no longer changes `measure_dso`
  or `read_waveform`. The test fails without the fix.
- **One version:**
  - `[workspace.package] version = "1.6.1"`, which every crate takes;
  - the GUI and CLI use `CARGO_PKG_VERSION`;
  - CMake reads `DS_VERSION_*` from `Cargo.toml` (checked with `cmake -P`).
- **Data directory:** `<prefix>/share/dsview` (the packages' layout), else Qt's
  `<prefix>/share/DSView`, else `/usr/local/share/DSView`. GUI and CLI now share this lookup in
  `dsview_session::default_data_dir`.
- **Crash reports:** a panic in `dsview` or `dsview-cli` writes the message, thread, place and
  backtrace to `$XDG_STATE_HOME/dsview/crash/<app>-<time>-<pid>.txt`, then prints as before.
- **Firmware:** the 14 FX2 firmware files the tree lacks are listed in F31. The log already names
  a missing one.

**Step 2, srzip export (done 2026-10-19):**
- **`dsview_formats::srzip::SrzipWriter`** ports `output/srzip.c`:
  - `version` ("2");
  - `metadata` as GKeyFile text: sigrok version 0.2.0, capturefile, total probes, samplerate,
    `probe<n>` of the enabled logic channels, and `unitsize` at the first chunk;
  - one `logic-1-<n>` per logic packet.
  - The archive is created by the first logic packet; there is no file without logic data.
- **Export:** `store::export_range_with` sends the same packets to a text module or to srzip.
  - The GUI offers it in logic mode (`getSuportedExportFormats`); a `.srzip` path selects it.
  - The control API's `export_data` and the MCP tool take `srzip` for logic data.
- **GKeyFile escaping** is now GLib's (`g_key_file_parse_string_as_value`), shared by srzip and
  the `.dsl` header. The oracle found the difference for a tab inside a name.
- **Checked:**
  - `export_diff::srzip_matches_c`: C's module (through the new `oracle_export_file`) and the
    Rust writer write the same entries in the same order. That covers random enabled sets,
    sample rates, packets and names with spaces, tabs, newlines, backslashes and UTF-8; 2000
    cases pass.
  - `session::srzip_export_holds_the_capture`: every sample of a demo capture.
  - `sigrok-cli -i demo.srzip` shows the 16 named channels at 25 MHz. Its 131072 samples equal
    DSView's CSV export at every run boundary.
- **Gnuplot is dropped** (DEVIATIONS).

**Step 3, `dsview-cli` commands (done 2026-10-19):**
- **New one-shot commands** next to `serve` and `soak`. They run the control API in process,
  with its argument checks and error texts, and exit with 1 on an error:
  - `devices` and `decoders` (`--json` for the API's JSON);
  - `info [--device D]`: the device's `get_config`;
  - `capture [--device D] [--mode] [--samplerate] [--samples] [--channels 0-7,9] [--timeout]
    --out FILE`: `.dsl` saves, `.csv`/`.vcd`/`.srzip` export;
  - `export FILE.dsl OUT`;
  - `decode FILE.dsl --decoder ID[:channel=N,option=value,...]`. It accepts several decoders
    and prints `start-end row: text` lines, or JSON. A decoder id may be written without its
    `0:` prefix.
  - `verify-counter`: the DSLogic's internal test mode, checked as a binary counter over the
    enabled channels. The operation mode is restored afterwards, so the profile keeps it.
  - `selftest --out DIR`, for DSCope and MSO-E8 owners: `selftest.json` (versions, devices,
    each hardware device's settings, a capture and the save) with `capture-<n>.dsl` and the
    full driver and decoder log `dsview.log`.
- **`--device D`:** an index or a part of a name. Without it, the first hardware device.
- **On the DSLogic Plus:**
  - `capture --samplerate 10000000 --samples 1000000 --channels 0-7 --out hw.dsl` takes 1 s;
  - `verify-counter` checked 16 channels × 16 777 216 samples with 0 breaks;
  - `selftest` wrote its bundle in 0.7 s.
  - All of it ran with their own XDG directories, and the user's profile stayed unchanged.
- **Tests:** `tests/oneshot.rs` runs the binary on the demo: devices, capture, export to srzip,
  UART decode, and the error paths. Plus the channel list parser.

**Step 4, the DSLogic hardware matrix (done 2026-10-19):**
- **`dsview-cli hwmatrix [--cycles N] [--quick] [--out REPORT.json]`** runs PLAN §3 item 6 through
  the session and the control API, as the GUI drives them. It puts the device's settings back
  afterwards.
- **On the DSLogic Plus** (firmware 2.2, own XDG directories): **132 of 132 passed in 480 s**.
  - **Test counter:** 1, 4, 8 and 16 channels × 4 M samples, 0 breaks.
    - The internal test mode always runs at 100 MHz. It takes the channel and sample count, but
      not the rate, so "every rate" is covered by the next two rows instead.
  - **Buffer mode:** 54 captures, every rate of the three channel modes (16 ch to 100 MHz, 8 ch to
    200 MHz, 4 ch to 400 MHz). Each reached the sample count the device took.
  - **Stream mode:** 62 captures, every rate of the four channel modes (16 ch to 20 MHz, 12 to
    25, 6 to 50, 3 to 100 MHz).
  - **Triggers on the counter**, each at 0, 50 and 100 %:
    - simple (ch15 rising): ch15 rises exactly at the reported trigger sample;
    - advanced, two stages (ch15 rising, then ch14 rising): fires at 0xC000 (49152) with ch14
      rising there;
    - serial (start ch15 rising, stop ch15 falling, clock ch0, data ch1, 4 bits): fires 7 samples
      after the start, at the fourth clock edge.
    - With an all-X start condition the FPGA never fires the serial trigger.
    - On the counter, the trigger position cannot move the event, because it comes 32768 samples
      after the start. So 50 % and 100 % trigger at the same sample.
  - **Stops:** while the trigger waits (an open input) and in the middle of a 10 MHz stream. Both
    ended about 100 ms after the stop.
  - **1000 start/stop cycles** at random rates, buffer or stream, run to the end or stopped after
    0-600 ms: all ended, 450 s.
- **The 10-minute stream:** `dsview-cli soak --mode loop --device dslogic --minutes 10` (release
  build). One endless loop capture ran 10 minutes, and the resident set stayed at 49 056 KiB
  throughout: trend +0.00 % over 61 samples.

**Step 5, tests and CI (done 2026-10-19):**
- **Fuzzing:**
  - Eight new targets: `usb_header` and `dso_status_block` (the DSLogic and DSCope drivers on a
    simulated device whose bulk IN data comes from the fuzzer), `pcapng_reader`,
    `jsonrpc_request`, `srzip`, `dsc_json`, `decoders_json` (on one demo session with every
    decoder) and `demo`.
  - Each ran 30-60 s here. The `pcapng_reader` target found a panic on blocks too short for
    their type; fixed, with a regression test, and 13 M runs since are clean.
  - The old `stores` target no longer compiled (the DSO store's `samplerate_tog`); fixed. The
    CI fuzz smoke job would have failed on it.
  - `dsview-usb`'s nusb backend is now a default feature: nusb 0.2.8's own `cfg(fuzzing)` code
    does not compile, and the fuzz build does not need real devices.
  - The USB targets capture 4096 samples, so one input reaches many headers and frames. The
    DSCope target went from 8 to about 10 000 runs in 30 s.
- **CI (`rust.yml`):**
  - `e2e-mcp`: the MCP suite on `dsview-cli` plus the one-shot command test;
  - fuzz smoke over every target (`cargo fuzz list`);
  - the test job on Ubuntu 22.04 and 24.04 (Python 3.10 and 3.12);
  - the portability job checks every crate but the oracle on Linux and macOS;
  - `libfftw3-dev` in `ORACLE_DEPS`.
- **`fuzz-nightly.yml`:** every target for an hour per night, with the corpus cached, which reaches
  24 h on the third night. Crashes are uploaded. Leak detection is off, because the JSON targets
  keep their interpreter on purpose.
- **Property tests:** `dsview-viewmodel/tests/viewport_props.rs`, for any rate, length, width and
  zoom path:
  - index→pixel→index round-trips within one sample, and keeps order;
  - zoom stays inside the scale and offset limits;
  - zoom keeps the sample under the pointer, within the integer offset's pixel (F27).
- **Chunking invariance:** `dsview-decode/tests/chunking.rs`, UART and I²C in chunks of 2 to 4096
  samples. It found F32: libsigrokdecode's chunk-edge shift, the same in C. Covered up to that one
  sample.
- **Miri** on the store's `unsafe` code (leaf blocks, raw block pointers):
  `single_channel_roundtrip_and_edges` and `leaf_words_match_cpp_leaf_block_space` are clean
  (473 s). The full suite does not finish in 30 min under Miri, because of its proptests. A CI
  `miri` job runs the two tests.
- **`cargo xtask golden bless`** writes the goldens from the Rust side: the reader summary and
  `rust-decode`. No C code runs. Its use is for intentional changes, once they are in
  DEVIATIONS.
- **M6c's exit checks:**
  - **100k+ rows:** `dsview-gui/tests/protocol_large.rs` decodes a 4 M-sample clock with `timing`
    into 262 142 rows.
    - With the protocol table shown, a frame takes 1.1 ms in release and 22 ms in debug.
    - The dock now caches the sorted columns per result set (trace, listed rows, results run and
      count, length, format). Collecting and sorting them took 9 ms on every frame before.
    - `DecoderStack::generation` counts clears, so a new run with the same count is not mistaken
      for the old one.
  - **Table and export against Qt:** not automated. Qt's protocol export exists only in its
    dialog, and Qt's control API has no method for it. The rows come from decoder results that
    `decode-diff` shows identical, and the writer follows `ProtocolExp::save_proc`. A
    side-by-side export on `protocol.demo` is on the R1 manual A/B checklist.

**Step 6, documentation (done 2026-10-19):**
- The Phase 0 note on the unplug traces now says what was checked (behaviour, both drivers) and
  that the pcapng needs a manual unplug: it is on the R1 checklist.
- DEVIATIONS:
  - F31 (runtime) and F32 (decoder chunk edges);
  - rows for the missing bitstream, srzip chunk names and GLib escaping;
  - decoder options from files ✔;
  - the USB stop/abort row was already ticked.
- PARITY: the export row lists srzip.
- PLAN: §5.6 marked done; R1 gets the Python layout and a manual A/B checklist; §6 points at R1.


## Release R1 (the A/B release): local part done 2026-10-19
**Defaults:**
- `dsview-ffi` has a feature `rust-default`. With it, every `DSVIEW_IMPL_*` subsystem without a
  variable runs Rust; `DSVIEW_IMPL=c` (or `DSVIEW_IMPL_<NAME>=c`) still selects C.
- The CMake build of `DSView-qt` enables the feature, and on Linux `DSVIEW_RUST_CORE` is now on
  by default (off with a warning when cargo is missing). The oracle builds `dsview-ffi` without
  it, so the C side of every differential check stays C.
- `cargo xtask control ab` first starts `DSView-qt` with no `DSVIEW_IMPL*` set and requires
  "rust" for all eight subsystems of `version`, then runs its C/Rust probes. Passed on the local
  build; all corpus captures read and export the same with C and Rust.
- DSCope/E8: PLAN 3e kept their C drivers as the default until a hardware confirmation. Inside
  `DSView-qt`, `DSVIEW_IMPL_ACQ=rust` runs every device on its Rust driver anyway (DEVIATIONS,
  4a), and the user's decision for R1 is Rust everywhere. The Qt desktop entry has an action
  "DSView (Qt, C core)" (`env DSVIEW_IMPL=c DSView-qt`) as the fallback for those owners.

**Packages (`build.yml`):**
- The Linux job gets the toolchain from `rust-toolchain.toml` (`rustup show`). CMake builds the
  Rust GUI, `dsview-cli` (now installed) and `DSView-qt` on the Rust core.
- The packaging steps moved into scripts:
  - `tools/package/deb.sh DEB [PYTHON]`:
    - bundles Python into `/usr/lib/dsview` (`bundle-python.sh`);
    - gives `DSView-qt` the same RUNPATH and the old `PYTHONHOME` wrapper;
    - drops the libpython dependency.
  - `tools/package/appdir.sh APPDIR [PYTHON]`:
    - the same bundle;
    - drops the bundled libxkbcommon;
    - writes an AppRun that starts the Rust GUI, or the Qt GUI with `--qt`.
  - `tools/package/check-deb.sh DIR IMAGE…` and `check-appimage.sh APPIMAGE IMAGE…`: the
    clean-container checks the job now runs.
- Deb metadata: the Rust GUI's dlopen'ed libraries as Depends (GL/EGL, X11, xcb, xkbcommon) and
  Recommends (Wayland, Vulkan, D-Bus).
- The Windows installer and the release job read the version from `Cargo.toml`. Before, they
  read it from CMakeLists.txt, which has no literal version since M6g. The installer step would
  have failed, and the release title would have had an empty version.
- The AppImage drops the `dsview-qt.desktop` entry (it is used through `--qt`).

**Checked locally:** the Linux job ran unchanged inside `ubuntu:22.04` (Python 3.10, the
runner's), with linuxdeploy and appimagetool:
- **.deb** (25.6 MB), on clean `ubuntu:22.04`, `24.04` and `26.04` (`check-deb.sh`):
  - `dsview-cli` lists the decoders, captures the demo and decodes 71 I²C annotations;
  - both GUIs run (Rust under Xvfb, Qt offscreen);
  - on 26.04 (no Python 3.10 there):
    - the Rust GUI's `version` reports rust for all eight subsystems, and it lists 157 decoders;
    - `DSVIEW_IMPL=c DSView-qt` reports c for all eight, with 157 decoders;
    - the desktop action is installed;
    - with the DSLogic Plus passed into the container, the packaged `dsview-cli verify-counter`
      counts 16 channels × 16 777 216 samples with 0 breaks, and `capture` writes a `.dsl`.
- **AppImage** (58 MB), on clean `ubuntu:22.04`, `24.04` and `26.04` with the desktop libraries
  AppImage expects from the host:
  - both GUIs answer on the control socket on the Rust core and list the decoders.
  - Found and fixed: the bundled libxkbcommon 1.4 (from 22.04) logged "unrecognized keysym" for
    26.04's Compose file.
  - Not a regression: Qt's `libusb-1.0.so.0`, like GL, comes from the host (AppImage
    excludelist), as with the Qt AppImage before.

**First GitHub CI runs (2026-10-19, PR #1 on github.com/phil16021/DSView_rust):**
- `Build` was green at once.
- `Rust` found what local runs had not:
  - GCC 11 and Qt 6.2 on Ubuntu 22.04 could not build the oracle;
  - LeakSanitizer flagged the embedded interpreter;
  - snapshots differed by 1-3 anti-aliased pixels;
  - the zbus "unmaintained" notices;
  - the C decoder load order in the A/B check;
  - real defects: F33 (the analog envelope writes past its ring), F34 (`ds_lib_exit` leaves a
    dangling device, so `oracle-dsl` crashed) and V/div 0 in the demo driver (fuzzer, F21).
- All of these were fixed before the C code was removed.

**R1 changed course (user, 2026-10-19):** no A/B package. The Qt GUI and the C code were removed
right away (M7a below), and the product became DSViewR 1.0.0. The manual A/B checklist
(protocol export against Qt's dialog, Qt's first start) no longer applies. The unplug pcapng
stays a manual hardware step.

## M7a removal and packaging, as DSViewR 1.0.0: done 2026-10-19
**Removed** (commit `3f86aa6f`, about 140k lines):
- `DSView/` (Qt GUI), `libsigrok4DSL/`, the C engine of `libsigrokdecode4DSL/` and `common/`;
- CMake, the Windows installer and cross-compile files;
- `crates/dsview-ffi`, `dev/dsview-oracle` (all C-vs-Rust checks), `tools/gen` and
  `xtask control ab`/`gen-header`.
- Tag `c-final` (`a6cf7a7d`) is the last tree with them.

**Kept and moved:**
- **`assets/`**, the data directory in the source tree, installed as `share/dsview`:
  - `res`, `demo`, `lang` and `decoders` (the 342 Python decoders);
  - the icons the GUI embeds, the udev rule, `dsviewr.desktop`, the manuals and the debian
    scripts.
  - A cargo build finds `assets/` through `dsview_session::data_dir_for`.
- **The Rust decoder runner** (`rust-decode` with `capture`, `rust_run` and `watchdog`) is now in
  `dsview-testkit`. `cargo xtask golden check` compares the Rust reader and engine with the
  stored C goldens (all pass), plus a Rust write/read-back check.
- **Frozen from C:**
  - the config key numbers (`crates/dsview-core/tests/config_keys.txt`);
  - `dsl/tables.rs`, now edited directly.

**DSViewR 1.0.0** (user's decisions):
- **Names:** `DSViewR` and `dsviewr-cli`, with the window title, app id, About, log file and CLI
  texts to match.
- **Help menu:** Bug Report and Update open github.com/phil16021/DSView_rust. About credits
  phil16021, DreamSourceLab and Schildkroet.
- **Unchanged:** the internal crate names, and the settings and profile paths.

**Packaging:**
- **cargo-deb**, with the metadata in `crates/dsview-gui/Cargo.toml`:
  - the package `dsviewr`, which replaces and conflicts with `dsview`;
  - the dlopen'ed libraries as Depends/Recommends;
  - the udev rule in `/usr/lib/udev/rules.d` with postinst/postrm.
- **`tools/package/`:**
  - `deb.sh` bundles Python;
  - `stage.sh` and `appdir.sh` build the AppImage (no Qt plugin; 25 MB, before 58 MB);
  - `check-deb.sh` and `check-appimage.sh` run the container checks.
- **CI:**
  - `build.yml` is Linux only: cargo-deb, the AppImage, the container checks and the release job.
  - `rust.yml` runs without C, Qt, glib or FFTW: lint, tests on 22.04/24.04, the golden check,
    fuzz, e2e-mcp, Miri, supply chain and portability.

**Checked locally:**
- **Workspace:** clippy, all 230 tests, the golden check and the fuzz targets compile.
- **Packages:** the Linux job replayed in `ubuntu:22.04`.
  - The `.deb` (21 MB) on clean Ubuntu 22.04, 24.04 and 26.04: `dsviewr-cli` decodes the demo
    (71 I²C annotations), and DSViewR runs under Xvfb.
  - The AppImage on the same three and on **Fedora 44**: DSViewR answers on its control socket
    and lists the decoders.
- **DSLogic Plus from the installed `.deb`** (26.04 container):
  - `verify-counter`: 16 channels × 16 777 216 samples, 0 breaks;
  - `hwmatrix --quick --cycles 20`: 37 of 37.
- **Screenshots** for the README (`docs/screenshots/`), taken headless under Xvfb over the
  control socket.

**Open:**
- the merge of `philipp` into `master`, the tag `v1.0.0` and the release (user; draft notes in
  `RELEASE-R1.md`);
- M7b, the post-parity fixes;
- the wgpu default renderer (PLAN, follow-on TODO).

## Post-1.0: bigger trigger squares in the logic header (2026-10-27)

- **Change** (`crates/dsview-gui/src/timeview.rs`):
  - the five trigger squares (rising, high, falling, low, edge) are 16 px at a 19 px pitch (were
    10 px at 11 px);
  - glyphs have a 3 px inset and a 1.5 px stroke;
  - geometry comes from one `trig_rect`, shared by hit-testing and painting.
- **Header width:** the logic header is 190 px (`LOGIC_HEADER_WIDTH`) so channel names keep their
  room. The DSO, analog, FFT and math headers stay at `HEADER_WIDTH` (150 px).
- **Tests:**
  - the new `snapshot::logic_trigger_squares` clicks a square (set), again (clear), and the corner
    of the edge square (replace);
  - the logic-view snapshots were updated after review: `protocol_demo_view`,
    `ruler_cursor_strips`, `mouse_measure_popup`, `yscale_badge`, `theme_latte`;
  - `cargo test -p dsview-gui` passes, and `cargo clippy -p dsview-gui --all-targets -D warnings`
    is clean.
- **Plan:** the open points are gathered in [M7b-PLAN.md](M7b-PLAN.md).

## M7b: F11 fixed, captures of any length (2026-10-27)

- **Problem** (DEVIATIONS F11): a logic capture whose length is not a multiple of 64 was shown
  shifted by the last group's overrun and saved cut. File sessions also had their length rounded up
  to 1024, which showed padding past the data.
- **Fix:**
  - `dsview-store`: single mode keeps `loop_offset` at 0, `capture_ended` trims and pads the last
    word with the last sample and rebuilds that leaf's mip-map, and the last block is whole 64-sample
    words;
  - `dsview-session`: `commit_settings` leaves a file device's rate and length alone, and the
    ranged `.dsl` save rounds to whole words.
- **Tests** (both failed before the fix):
  - three store tests, one of them a proptest: any length 1..5000, 1/2/3/16 channels, packets of
    1..20 groups, every sample and every edge;
  - `session::unaligned_dsl_round_trip`;
  - the `measure.rs` test now uses 1000 samples.
- **Checked:**
  - workspace: 235 tests pass, and clippy `-D warnings` is clean;
  - `xtask golden check` ok;
  - `xtask control check` 38 steps;
  - `xtask mcp suite` 30 tools, 37 steps.
- **Not run here:** the cargo-fuzz smoke (no cargo-fuzz or nightly toolchain); CI runs it.

## M7b: F31 fixed, DSO V/div after or during a capture (2026-10-27)

- **Problem** (DEVIATIONS F31):
  - while a capture ran, a V/div change left voltages in the V/div of the start, so after a dial
    step the demo's Vpp halved when V/div doubled (GUI and control API);
  - when stopped, only the header dial rescaled the trace. V/div from Device Options, auto-set or
    the control socket changed the grid but not the trace.
- **Fix:**
  - `Session::set_config` updates `cap_vdiv` for a DSO channel's V/div while a capture runs, and
    leaves it alone when stopped;
  - the DSO view takes the data's V/div from there and derives the trace's stop scale from it; its
    own step counting is gone.
- **Tests:**
  - `dsview-control` `transcript::dso_vdiv_changed_while_running` (Vpp 3940 mV before, 1970 mV
    after without the fix);
  - `dsview-gui` `dsoview::tests::stopped_trace_follows_vdiv_from_any_path` (scale 1 instead of
    0.5 without it);
  - the existing `dso_voltages_keep_the_capture_vdiv` still passes.
- **Checked:**
  - workspace tests, clippy `-D warnings`, golden check, control transcript (38 steps), MCP suite
    (30 tools, 37 steps);
  - the GUI snapshots are unchanged.
- **Seen in passing:** `hotplug::detach_during_capture` failed 2 times in 46 runs with this change
  and 0 in 21 without. Its 3 s waits are timing-sensitive and it does not use `Session`, so this
  looks like an existing flake; to be watched.

## M7b: F8, F19, F21, F25, F28 fixed (2026-10-27)

Each fix has a test that failed before it.

| ID | Fix | Test |
|---|---|---|
| F21 | The demo's protocol playback reads each enabled channel's own `L-<index>`. With channel 3 disabled, channel 4 showed channel 3's data | `demo::protocol_playback_skips_disabled_channels` |
| F19 | The analog demo's pattern buffer is at least one 206-byte cycle. A capture under 2048 samples sent only END, where C spun at 100 % CPU | `demo::short_analog_capture_delivers_data` |
| F8 | `si_string_u64` pads the remainder: "131.072 k Samples", "1.05 MHz" (was "131.72 k", "1.50000 MHz"). Affects the CSV header, the srzip metadata and the GUI rate labels; `.dsl` is unchanged | `cfmt::tests::si_strings` |
| F25 | DSO channel 1's hardware burst width uses its cycle time, not its fall time | `measure::tests::hardware_burst_width_is_the_same_for_both_channels` |
| F28 | Auto-set no longer sets the trigger level twice in the pass that ends auto-V | `autoset::tests::steps_until_it_fits` |

**Checked:**
- workspace: 240 tests pass, and clippy `-D warnings` is clean;
- golden check ok;
- control transcript: 38 steps;
- MCP suite: 30 tools, 37 steps.

## M7b: decoder engine fixes F13, F14, OUTPUT_LOGIC, F32 (2026-10-27)

Each fix has a test that failed before it.

| Item | Fix | Test |
|---|---|---|
| F13 | `wait()` returns a fresh pin tuple; a kept result no longer makes the next `wait()` fail (the `pjdl` and `ieee488` pattern) | `tests/keep_pins.rs` |
| F14 | A negative `skip` is `skip: 0`; a count too large for 64 bits fails at once. No error is left pending, so F17 is moot | `tests/pending_error.rs` |
| `OUTPUT_LOGIC` | `srd.OUTPUT_LOGIC` (4), checked and passed to a callback if registered. `pca9571` and `tca6408a` start | `tests/logic_output.rs` |
| F32 | The step past a match on a chunk's last sample no longer repeats on the next chunk | `tests/chunking.rs`, exact for chunks of 1 to 4096 |

**F32 on the demo** (`rust-decode protocol.demo --max-chunk N` against the default chunking):

| Chunk size | Lines that differed before | After |
|---|---|---|
| 64 | 43 | 0 |
| 77 | 42 | 0 |
| 1000 | 3 | 0 |

**`Quirks::FIXED`:** not built. The engine is chunk-independent, and DSView's chunking still gives
the stored golden.

**Checked:**
- workspace: 244 tests pass, and clippy `-D warnings` is clean;
- golden check ok, decoder golden unchanged;
- control transcript: 38 steps;
- MCP suite: 30 tools, 37 steps.

**Not covered:** no PJDL, GPIB, FlexRay or PCA9571/TCA6408A capture is in the tree, so those
decoders are covered by the engine tests only.
