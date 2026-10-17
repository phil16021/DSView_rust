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

### F9. Logic packets that are not whole 64-sample groups corrupt the capture
**Observed (Phase 1b, `store_diff.rs`, ASan):** `LogicSnapshot::append_cross_payload` assumes every
LA_CROSS packet ends on a 64-sample group boundary (one u64 per channel). When it does not:
- If the last channel's run does not reach the packet end, the group counter does not advance. The
  packet's samples are dropped from the count, and the next packet's leftover bytes overwrite earlier
  data. In loop mode this stalled a 1.1G-sample stream at 0.6G samples.
- It reads up to 7 bytes past the packet, and after a channel resumes at a saved read position, more
  than that (ASan heap-buffer-overflow at `logicsnapshot.cpp`, `*write_ptr++ = *read_ptr`).

**Reachability:** none known. The DSLogic driver sizes captures in whole groups
(`actual_bytes = samples / 64 * channels * 8`), and the `.dsl` loader and demo driver send whole groups.

**Rust behaviour:** identical results for whole-group streams (the differential tests use only those).
For other streams it reads zeros past the packet and never touches memory outside its leaves
(`crates/dsview-store/tests/robustness.rs`).

### F10. A capture into an uncleared LogicSnapshot reuses stale state
`first_payload` has a leaf-reuse path for "same channels, same limit". That path keeps
`_loop_offset`, `_byte_fraction`/`_ch_fraction` and `_dest_ptr` from the previous capture:
- a stale `_loop_offset` misaligns the write position, and the leaf write runs off its 2 MB block
  (ASan heap-buffer-overflow);
- a stale `_dest_ptr` points into a freed leaf (ASan heap-use-after-free).

**Reachability:** none. `SigSession` clears the store before every capture. After `clear()` the channel
list is empty, so the reuse path is never taken.

**Rust behaviour:** same logic, but memory-safe. The differential tests clear between captures, as
DSView does.

### F11. Logic captures whose length is not a multiple of 64 are shifted, and lose data when saved
**Observed (Phase 1b), in the shipping C++ DSView and in the Rust store (kept for parity):**
- The last packet carries whole 64-sample groups, so it overruns the sample limit.
- `append_cross_payload` stores the overrun as `_loop_offset` even outside loop mode. Every read then
  adds it, so the capture is shown shifted and the zero padding appears at the end.
- Example: a 1000-sample `.dsl` with a rising edge at sample 500. DSView shows the edge at 476 and a
  phantom falling edge at 976.
- Saving that capture writes 960 samples: the first 24 samples are lost, the data is shifted, and the
  tail is cut. Reopening shows the shifted edges permanently.

**Reachability:** any logic capture with a sample count that is not a multiple of 64. This covers `.dsl`
files from other tools or edited headers, demo/file sessions with such limits, and possibly some
hardware limits (the DSLogic rounds its limits to whole groups).

**Rust behaviour:** identical for now; the A/B and the differential tests depend on it.
**Fix after the Rust store becomes the default:** keep the overrun out of `loop_offset` in single
mode, and have `capture_ended` trim to `total_sample_count`.

### F12. The analog envelope reads one entry past its ring
**Observed (Phase 1b, `store_ad_diff.rs`):** `AnalogSnapshot::append_payload_to_envelope_levels` builds
envelope level *n* from level *n−1*. After taking an entry it advances without the wrap check:

```c
EnvelopeSample sub_sample = *src_ptr++;
```

When a 16-entry group starts at the last entry of the lower ring, the next read is one entry past the
`malloc`ed array. That is a 2-byte heap over-read, folded into the min/max. It happens once the sample
ring wraps and a level's entry count is not a multiple of 16; the drawn envelope can then show a spike
from unrelated heap bytes.

Level 0 has the same pattern on the sample ring. One sample row past the ring lands in the 8 spare
bytes `first_payload` allocates, which `malloc` leaves uninitialised. With rows wider than 8 bytes,
for example 4 channels of 32-bit samples (found by fuzzing), the read goes past the allocation.

**Rust behaviour:**
- Levels 1 and up: the out-of-range entry is skipped and the group wraps. The oracle fills the
  allocator slack after each C++ ring with neutral entries (min 0xff, max 0), which makes the C++
  result equal the Rust one. Where there is no slack, the C++ result is undefined and the levels
  above are not compared.
- Level 0: bytes past the ring read as 0. The Rust spare bytes are zero, and so are the oracle's
  after `osana_zero`.

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
| Logic store, packets not of whole 64-sample groups (F9) | Mis-assembled; reads/writes out of bounds | Same assembly; past-the-packet bytes read as 0; memory-safe | 1b ✔ |
| Logic store, capture into an uncleared store (F10) | Stale offsets; heap overflow / use-after-free | Same logic, memory-safe (DSView always clears first) | 1b ✔ |
| Logic store `_dest_ptr` into a freed leaf | Dangling pointer kept | Write position dropped (only reachable through F9/F10 input) | 1b ✔ |
| Analog envelope over-read (F12) | One entry past the ring folded into min/max | Entry skipped | 1b ✔ |
| DSO envelope, `cal_vrms`, `cal_vmean` | Implemented, never called | Not ported; under `DSVIEW_IMPL_STORE=rust`, `enable_envelope` is a no-op and the two return 0 | 1b ✔ |
| Analog data/DSO frames/envelopes before they are written | `malloc`: uninitialised | Zeroed | 1b ✔ |

## Departures from PLAN.md

| Plan said | Done instead | Why |
|---|---|---|
| Corrosion for the CMake ↔ cargo build | A `dsview_ffi` custom target that runs `cargo build --locked` (option `DSVIEW_RUST_CORE`, OFF by default) | Corrosion is fetched from GitHub at configure time, the kind of unpinned download the security review flagged. The custom target uses only the committed `Cargo.lock` |
| cbindgen-generated header | Done in Phase 1a: `cargo xtask gen-header`, checked in CI | — |
| `.dsc` profiles in Phase 1a | Deferred to Phase 4/5 | Profiles are read and written only by the Qt GUI (`MainWindow`, Qt JSON), so a C seam has no user; the Rust model is built with the Rust session/GUI |
| srzip/gnuplot export in Rust | srzip stays C until it is ported; gnuplot is dropped in Phase 6 | csv and vcd cover the export dialog defaults; srzip (sigrok `.sr`) needs a sigrok-compatible writer of its own |
| Goldens as zstd JSONL | gzip (`.jsonl.gz`, via `flate2`) | Pure Rust, no C build dependency; 60 KB for the protocol.demo decode golden |
| Phase 0 performance baselines (`get_display_edges`, ingest, decode throughput) | Deferred to the start of Phase 1b / Phase 2 | `get_display_edges` and ingest need `oracle-store` (LogicSnapshot built against Qt Core), which is Phase 1b's oracle. Decode throughput needs large synthetic captures from `dsview-siggen`; the only corpus capture (131k samples) is dominated by Python start-up |
| Phase 1b gate "ingest ≥ 1 GB/s per core" | Measured 766 MiB/s (Rust) vs 777 MiB/s (C++) on the development VM; the gate is moved to the Phase 4 capture path | Even a bare strided copy into fresh 2 MiB blocks reaches only 0.74–0.98 GiB/s on this VM, and a plain memcpy into fresh memory about 1.4 GiB/s. 1 GB/s needs a different leaf layout (a transposing copy, pooled pre-faulted leaves) that breaks pointer compatibility with the C++ GUI. The fastest DSLogic streams about 0.4 GB/s |
| Lock-free store readers verified with `loom` (Phase 1b) | The FFI wraps each store in a mutex, like the C++ classes | The C++ GUI locks around every call anyway; lock-free readers matter for the Rust GUI and capture path (Phase 4/5) |
| Annotation store in `dsview-store` (Phase 1b) | Moved to Phase 2 | Annotations are produced and owned by the decoder engine |
| Decoder pin identity in the store tests | The differential tests track DecoderStack's pinned leaf by position, not address | DecoderStack compares leaf addresses; glibc may give a new leaf a freed leaf's address, so the C++ behaviour itself depends on the allocator |
| USB traces in the repository | Recorded 2026-10-17 (11 scenarios plus startup). The pcapng files stay out of git; `testdata/usb/dslogic/manifest.json` pins their sha256 | 16 MB of binary traces; the plan's separate test-data repository is not set up yet |
