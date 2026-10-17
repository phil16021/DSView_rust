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

## Planned behaviour deviations (Rust intentionally differs)

| Area | C behaviour | Rust behaviour | Phase |
|---|---|---|---|
| DSCope EEPROM calibration | Reads and writes bytes 30-31 past the 30-byte `cmd_zero_info` struct (stack overflow) | Bounded 32-byte layout; calibration values for gain levels 2/3 are persisted | 3e |
| Decoder `wait()` pin tuple | One tuple object, mutated in place | A fresh tuple per call | 2 |
| Decoder options from files | Applied as stored | Path/exec options (`objdump`, `elffile`, `mapping`) reset to defaults until the user confirms | 2 |
| USB stop/abort | No transfer cancellation, infinite timeouts, unbounded busy-polls | FORCE_RDY → grace period → cancel all transfers; every wait has a deadline | 3 |

## Departures from PLAN.md

| Plan said | Done instead | Why |
|---|---|---|
| Corrosion for the CMake ↔ cargo build | A `dsview_ffi` custom target that runs `cargo build --locked` (option `DSVIEW_RUST_CORE`, OFF by default) | Corrosion is fetched from GitHub at configure time, the kind of unpinned download the security review flagged. The custom target uses only the committed `Cargo.lock` |
| cbindgen-generated header | `crates/dsview-ffi/include/dsview_ffi.h` written by hand | The ABI is one function so far; cbindgen (not installed) comes in with the first real API in Phase 1 |
| Goldens as zstd JSONL | gzip (`.jsonl.gz`, via `flate2`) | Pure Rust, no C build dependency; 60 KB for the protocol.demo decode golden |
| Phase 0 performance baselines (`get_display_edges`, ingest, decode throughput) | Deferred to the start of Phase 1b / Phase 2 | `get_display_edges` and ingest need `oracle-store` (LogicSnapshot built against Qt Core), which is Phase 1b's oracle. Decode throughput needs large synthetic captures from `dsview-siggen`; the only corpus capture (131k samples) is dominated by Python start-up |
| USB traces recorded in Phase 0 | Recorder and scenarios done and validated on the DSLogic in dry-run mode; the pcapng recording itself is pending | Needs root once: usbmon module plus dumpcap capture rights (see `tools/usb/README.md`) |
