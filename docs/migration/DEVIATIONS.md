# Deviations log

This file records every known way the Rust port behaves differently from the C/C++ original, and every
place where the implementation departs from `PLAN.md`. Each entry says why, and gives the evidence.
Golden files come from the C oracle (`dev/dsview-oracle`), so any intentional difference must be
listed here before a golden is blessed.

## Findings about the original C code

### F1. The decoder engine's output depends on chunk boundaries
**Observed (Phase 0):**
- `oracle-decode DSView/demo/logic/protocol.demo` with DSView's default chunking (16384 samples,
  `DecoderStack::MaxChunkSize`) differs from the same run with `--max-chunk 1000`.
- The difference is 3 annotations: UART bit annotations at samples 80108 and 97108, and a CAN-FD bit
  annotation at 18003. With 1000-sample chunks each moves by **+1 sample**.
- With `--max-chunk 77`, 42 lines differ. With `--max-chunk 4096` the output is identical.

**Cause:** the constant-chunk skip shortcut in `libsigrokdecode4DSL/instance.c` (around lines 944-1000)
evaluates conditions differently at chunk edges.

**Consequences:**
- Goldens are recorded **only** with DSView's exact chunking: 16384-sample chunks, clipped at 2^24-sample
  leaf boundaries. `oracle_decode.c` reproduces that chunking.
- The Rust engine's default mode `Quirks::C_COMPAT` must reproduce this behaviour bit for bit.
- `Quirks::FIXED` will make output independent of chunking. Every annotation it changes must be listed
  under "Planned deviations".

### F2. Stopping a capture is asynchronous on real hardware
**Observed on a DSLogic Plus (PID 0x0034, Phase 0 dry run):**
- `stop_capture` while waiting for a trigger returns before the capture has ended.
- DSView sends `FORCE_RDY`, the FPGA flushes 9,216 buffered samples, and only then does the session report
  that it is idle (about 50 ms).
- Calling `select_device` right after `stop_capture` fails with "DSView is capturing or saving".

**Consequence:** the Rust session must expose stop as an explicit state, Stopping → Done, and callers must
wait for it.

### F3. Fractional samplerates do not survive a C round trip
**Observed (Phase 1a, code reading plus property tests):**
- `sr_si_string_u64` writes a non-integral rate as `<rate/unit>.<rate%unit>`, where the remainder is in Hz
  and unpadded. 1.05 MHz becomes "1.50000 MHz", and 1.5 MHz becomes "1.500000 MHz".
- `sr_parse_sizestring` stops at the dot and ignores the error it returns, so DSView loads such a file at
  **2 Hz** or **1 Hz**.

**Rust behaviour:**
- `dsview-formats` decodes the writer's actual format, so it recovers every rate the C writer can produce.
  This is property-tested over 0..10 GHz.
- The Rust writer never emits a dot (it writes "1050 kHz"), so both readers agree on Rust-written files.

### F4. Per-channel header keys are positional
- `vDivN`, `enableN`, `couplingN` and similar keys address the *N-th channel in the list*, not the channel with index N.
- `triggerN` addresses the channel with *index* N.
- The C writer numbers `trigger` keys with a running counter (`probecnt`), so with disabled logic channels a
  trigger can be restored on the wrong channel. `dsview-formats` reproduces the reader's addressing exactly.

### F5. CSV export writes decimal commas under many locales
**Observed (Phase 1a, `export_diff.rs::c_csv_uses_locale_decimal_comma`):**
- With `LC_NUMERIC=de_AT.utf8`, `output/csv.c` writes DSO and analog values as `-0,28000` inside a
  comma-separated file.
- Qt sets the process locale from the environment, so this happens in DSView for every user with a
  decimal-comma locale (most of continental Europe).
- Only the time column has a comma-to-dot workaround.

**Rust behaviour:** the exporter always writes `.`.

### F6. An empty logic packet crashes the CSV and VCD exporters
**Observed (Phase 1a, `export_diff.rs::c_crashes_on_empty_logic_packet`, ignored by default):**
- `for (i = 0; i <= length - unitsize; ...)` underflows when `length < unitsize`, and the process gets SIGSEGV.

**Rust behaviour:** produces no output for an empty packet.

### F7. Saving ignores write errors and is not atomic
- `ZipMaker::AddFromBuffer` always returns true, so a full disk silently produces a truncated `.dsl`.
- Saving writes over the target in place, so a cancelled or failed save destroys the previous file.

**Rust behaviour:** `DslZipWriter` reports every error and checks the header. It writes to a temporary
file, verifies it opens, and renames it into place. `ZipMaker::Discard` keeps the previous file when Rust
writes.

### F8. Sample counts are printed with an unpadded remainder
- `sr_si_string_u64` prints 131072 samples as "131.72 k Samples" in CSV headers.
- **Kept byte for byte**: it is cosmetic, and changing it would break the export diff.
- To be fixed after the Rust export becomes the default.

## Planned behaviour deviations (Rust intentionally differs)

| Area | C behaviour | Rust behaviour | Phase |
|---|---|---|---|
| DSCope EEPROM calibration | Reads and writes bytes 30-31 past the 30-byte `cmd_zero_info` struct (stack overflow) | Bounded 32-byte layout; calibration values for gain levels 2/3 are persisted | 3e |
| Decoder `wait()` pin tuple | One tuple object, mutated in place | A fresh tuple per call | 2 |
| Decoder options from files | Applied as stored | Path/exec options (`objdump`, `elffile`, `mapping`) reset to defaults until the user confirms | 2 |
| USB stop/abort | No transfer cancellation, infinite timeouts, unbounded busy-polls | FORCE_RDY → grace period → cancel all transfers; every wait has a deadline | 3 |
| `.dsl` samplerate | Fractional rates misread (F3); an unparsable string silently loads as 0 or garbage | Decodes the C writer's format; an unparsable string is an error | 1a ✔ |
| `.dsl` misaligned logic block | Stream silently ends at that block | Load error ("logic block is not 8-byte aligned") | 1a ✔ |
| `.dsl` version 1, data not a multiple of 128 bytes | Last partial 64-sample block converted from a stale buffer | Partial block dropped | 1a ✔ |
| `.dsl` file rejection | Hardened C loader rejects bad headers when the session loads | Rejected already when the file is opened (`sr_new_virtual_device`), with the reason in the log | 1a ✔ |
| Datafeed packets from files | `struct sr_datafeed_*` fields the driver does not set are left uninitialised | All fields zeroed | 1a ✔ |
| Header values written by Rust | Probe names written raw; a backslash makes GKeyFile reject the name | GKeyFile escaping (`\\`, `\n`, leading `\s`) | 1a ✔ |
| Zip entry bytes | minizip deflate | `zip` crate deflate at the same level (1); entry *contents* identical, compressed bytes differ | 1a ✔ |
| CSV/VCD export, empty logic packet | Crash (F6) | No output | 1a ✔ |
| CSV export numbers | Locale decimal comma (F5) | Always `.` | 1a ✔ |
| CSV export, 32+ logic channels | Undefined behaviour (`1 << i` on `int`, mask sign extension) | Correct n-bit mask | 1a ✔ |
| CSV export, analog with more than 8 channels | Stack overflow (`int enalbe_channel_flags[8]`) | Works | 1a ✔ |
| CSV export, DSO channel index ≥ enabled count | Reads past the frame | Reads 0 | 1a ✔ |
| Saving `.dsl` | Write errors ignored; in-place overwrite (F7) | Errors reported; atomic temp file plus rename; previous file kept on cancel | 1a ✔ |
| Saving `.dsl`, entry names | Any | Only DSView's names; the header must parse | 1a ✔ |

## Departures from PLAN.md

| Plan said | Done instead | Why |
|---|---|---|
| Corrosion for the CMake ↔ cargo build | A `dsview_ffi` custom target that runs `cargo build --locked` (option `DSVIEW_RUST_CORE`, OFF by default) | Corrosion is fetched from GitHub at configure time, the kind of unpinned download the security review flagged. The custom target uses only the committed `Cargo.lock` |
| cbindgen-generated header | Done in Phase 1a: `cargo xtask gen-header`, checked in CI | — |
| `.dsc` profiles in Phase 1a | Deferred to Phase 4/5 | Profiles are read and written only by the Qt GUI (`MainWindow`, Qt JSON), so a C seam has no user; the Rust model is built with the Rust session/GUI |
| srzip/gnuplot export in Rust | srzip stays C until it is ported; gnuplot is dropped in Phase 6 | csv and vcd cover the export dialog defaults; srzip (sigrok `.sr`) needs a sigrok-compatible writer of its own |
| Goldens as zstd JSONL | gzip (`.jsonl.gz`, via `flate2`) | Pure Rust, no C build dependency; 60 KB for the protocol.demo decode golden |
| Phase 0 performance baselines (`get_display_edges`, ingest, decode throughput) | Deferred to the start of Phase 1b / Phase 2 | `get_display_edges` and ingest need `oracle-store` (LogicSnapshot built against Qt Core), which is Phase 1b's oracle. Decode throughput needs large synthetic captures from `dsview-siggen`; the only corpus capture (131k samples) is dominated by Python start-up |
| USB traces recorded in Phase 0 | Recorder and scenarios done and validated on the DSLogic in dry-run mode; the pcapng recording itself is pending | Needs root once: usbmon module plus dumpcap capture rights (see `tools/usb/README.md`) |
