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

### F13. Decoders that keep the tuple wait() returned fail on their next wait()
**Observed (Phase 2, `decode-diff`, `pjdl`):** `Decoder_wait` returns one tuple per instance and refills it
with `PyTuple_SetItem` on every call. CPython refuses that while anyone else holds the tuple (refcount
above 1) and sets a SystemError. `wait()` then returns the stale tuple with the exception set, and Python
raises `SystemError: <method 'wait' of 'sigrokdecode.Decoder' objects> returned a result with an
exception set`.
- Decoders written as `pins = self.wait(...)` therefore stop at their second `wait()`: `pjdl` and `ieee488`.
- Decoders that unpack immediately, `(a, b) = self.wait(...)`, are unaffected.

**Rust behaviour:** identical (same tuple, same `PyTuple_SetItem`), so the outputs match. A fresh tuple per
call would fix these decoders; that change is planned for after the Rust engine becomes the default.

### F14. A negative or oversized `skip` count makes wait() fail later
**Observed (Phase 2, `flexray`):** `create_term_list` reads the count with `PyLong_AsUnsignedLongLong`
and does not check for an error. A negative count becomes 2^64−1 and leaves an OverflowError pending.
`wait()` keeps scanning with the other conditions, and when one matches it returns with the exception
set, so Python raises the same SystemError as in F13. `flexray` computes `pos - self.samplenum`, which can
be negative.

**Rust behaviour:** identical (the error is left pending the same way).

### F15. Decoder strings are cut at the first NUL character
**Observed (Phase 2, `amulet_ascii`):** annotation texts, metadata strings and condition values go through
`g_strdup(PyBytes_AsString(...))`, which stops at a NUL. `amulet_ascii` formats a received 0 byte with
`'%c'`, so its texts end there.

**Rust behaviour:** identical.

### F16. Some decoders never return on unusual input or options
**Observed (Phase 2, stack timeouts in `decode-diff`, identical in both engines):**
- `gpib` floods zero-length annotations at sample 0 forever.
- `onewire_link` loops forever and grows memory with a small "min height level pulse time".
- `ltc26x7` also never returns.
- `midi` never returned once in 1000 runs.

In DSView the decode never finishes, and in the case of `onewire_link` memory grows until the process
is killed (seen at about 4 GB). These are bugs in the Python decoders, not in either engine.

Other decoder failures seen identically in both engines:
- `lfast`: NameError, `bMoreMatch` is not defined.
- `ir_irmp`: needs `libirmp.so`, which is not built.
- `pca9571` and `tca6408a`: need `OUTPUT_LOGIC`, which DSView's engine does not have.
- `ir_irmp`'s `__del__` raises AttributeError at exit.

### F17. Whether a pending wait() error survives depends on CPython's type cache
**Observed (Phase 2, `decode-diff` seed 100354, `flexray`):** after a match, `Decoder_wait` stores
`self.samplenum` and `self.matched` while the F14 OverflowError is still pending.
- On a type-attribute-cache miss, CPython's `_PyType_Lookup` clears any live exception (3.12
  `typeobject.c`: "We may end up clearing live exceptions below"). `wait()` then returns normally and
  the decoder carries on.
- Whether the lookup misses depends on cache slots indexed by object addresses and shared by all
  threads, so the outcome can change from run to run.
- In C the slot is almost always hot: C passes an interned name the decoder has just read, and C gave
  the same output in 200 of 200 runs. A decoder that modifies its class between waits forces the miss
  in C too.
- C does lose it occasionally. In the exit batch, seed 100223 `flexray` carried on in C at a point
  where it stopped with the SystemError in 29 later C replays (5 quiet, 24 under load) and in Rust.
- The first Rust port passed a fresh string on every store. Under load it lost the error in about 9%
  of runs.
- `decode-diff` therefore runs a differing seed again, up to twice. A difference that does not come
  back is reported as `FLAKY` and names the engine whose output changed; it fails only if Rust's
  output changed.

**Rust behaviour:** the pending error is held aside while the two attributes are stored and then
restored, so `wait()` always fails as C does in practice. The names are interned as in C. Regression
test: `crates/dsview-decode/tests/pending_error.rs`, which forces the cache miss.

### F18. The demo driver reads past its buffers when a packet is larger than the data behind it
**Observed (Phase 3, `drv-diff`; every C crash in the batches is one of these):**
- Analog (`receive_data_analog`): a pattern is expanded into a buffer of about
  `0.1 × sample limit` bytes, random data fills 1 MiB, and each packet takes `samplerate / 100`
  bytes from it. When a packet
  is longer than the buffer, the wrap-around `memcpy` reads past it. Once the read position is beyond
  the buffer, `data_buf_len - analog_read_pos` underflows and the next copy is gigabytes long
  (SIGSEGV at `demo.c:2489`). Smaller overruns read heap garbage into the packet without crashing.
- Logic random data (`receive_data_logic`): packets of 2 MiB or more overrun the 2 MiB random
  buffer. At exactly 2 MiB, `rand() % 0` raises SIGFPE; above it, the source offset is a wild
  value (SIGSEGV).
- DSO instant mode: the last packet points `packet_len` bytes past the end of the frame buffer and
  hands those bytes to the GUI.

The GUI's lists do not reach the analog and logic cases: the shortest DAQ duration is 200 ms, and
each logic channel mode caps the samplerate. They are reached through `config_set` with other values
(the control API, a profile). The DSO case happens on every instant capture.

**Rust behaviour:** the same packets wherever C's are defined. Bytes C would read from beyond its
buffers are 0, and the offsets are computed without underflow, so nothing crashes. `drv-diff`
requires Rust to match C up to the crash or the undefined read, and to finish the scenario.

### F19. The analog demo delivers no data for captures under 2048 samples
**Observed (Phase 3, `drv-diff`):** for a pattern (not "random") the buffer size is
`103/2048 × limit × 2` rounded down to whole 206-byte cycles. Under 2048 samples this is 0,
`g_try_malloc0(0)` returns NULL, and `receive_data_analog` fails before sending anything. It fails
the same way on every later callback, so the session loop spins at 100% CPU with no data until the
user stops it. The GUI can choose such a limit: at 10 Hz its list includes 200 s, which is 2000
samples.

**Rust behaviour:** the same (no data, an error in the log, spinning until stopped), so the capture
looks the same. To be fixed after the default flip.

### F20. Driver packets carry uninitialised fields
**Observed (Phase 3, `drv-diff`):** the demo driver and the session (file) driver fill the
`sr_datafeed_*` structs on the stack and leave fields unset. The GUI then reads stack garbage:
- DSO packets (demo, and file playback of version 3 DSO captures): `samplerate_tog`, `trig_flag`
  and `trig_ch`. The GUI uses all three: `samplerate_tog` resets the DSO envelope, and the trigger
  fields mark the trigger.
- Analog packets (demo): `unit_pitch`. `AnalogSnapshot::append_data` decimates by it when it is
  above 1, so the analog trace can depend on stack contents and read past the packet.
- Logic packets: `unitsize`, `data_error` and `error_pattern` (unused by the GUI). In the file
  driver's version-3 path also `index` and `order`.
- The file driver's SR_DF_TRIGGER (`struct ds_trigger_pos`): every field except `real_pos`.

**Rust behaviour:** all of them are 0. The oracle builds `demo.c` and `session_driver.c` with
`-ftrivial-auto-var-init=zero`, so the C reference is what C with zeroed structs sends.

### F21. Smaller defects in the demo and file drivers
Found while porting, identical in Rust unless noted:
- **Demo logic playback ignores which channels are disabled.** `protocol.demo` is read from `L-0` …
  `L-<n-1>` for the n *enabled* channels. With channel 3 disabled, channels 4–15 show the data of
  channels 3–14, and channel 15's data is lost. Same in Rust, until the flip.
- **Dangling `map_unit`.** `config_set(SR_CONF_PROBE_MAP_UNIT)` keeps the pointer into the GVariant,
  which `sr_config_set` frees right after. Rust copies the string; the shim hands out an interned
  copy.
- **NULL channel crash.** Channel keys (`PROBE_EN`, `PROBE_VDIV`, …) with no channel dereference
  NULL in both drivers. Rust returns SR_ERR_ARG. The GUI always passes a channel.
- **Two SR_DF_END packets per demo DSO capture.** The source callback sends one when it stops, and
  `dev_acquisition_stop` sends another. Same in Rust.
- **Pattern files.** Any name containing ".demo" counts, losing its last five characters. The
  directory test checks the bare name against the current directory.
- **Version 1 captures and live channel changes** (a Phase 1a gap, now fixed). C unpacks version-1
  logic data for the channels enabled when the capture starts. The Rust reader used the flags stored
  in the file, so after disabling a channel the packets differed. `DslReader::set_channel_enabled`
  now takes the live flags, in the Rust file driver and in `session_driver.c`'s Rust path.

### F22. Command headers carry stack contents on the wire
**Observed (Phase 3, usbmon traces of the C driver):** `struct ctl_header.offset` is never set for
some commands. It goes to the device as whatever the stack held:
- CTL_WR START, STOP and BULK_WR (the BULK_WR before arming showed `0x9897` in every trace);
- CTL_RD_PRE FW_VERSION and HW_STATUS (`0xa1ad` once while opening).

DSLogic firmware ignores the field for these commands. This fork already zeroes it for PROG_B, SYS,
LED, INTRDY and WORDWIDE, after MSO-E8 firmware wrote INTRDY's byte into its EEPROM through a garbage
offset.

**Rust behaviour:** 0. The replay device and `usb-diff` do not compare these two bytes.

### F23. More of the DSLogic driver's C defects
- **Unbounded waits.** Arming polls HW_STATUS for SYS_CLR with `while(1)`; FPGA configuration
  does the same for INIT_B, GPIF_DONE and FPGA_DONE. The data transfers have no timeout, and
  stopping waits until the device has drained them. A device that stops answering hangs the
  collect thread, and `ds_stop_collect()` blocks with it.
- **NULL dereference.** `config_list(SR_CONF_PROBE_VDIV)` walks `dev_caps.vdivs`, which is NULL
  for every DSLogic profile. The GUI asks only for DSO and DAQ channels, so this does not happen in
  practice.

### F24. Defects of the DSCope driver (also serves the MSO-E8)
Found while porting `dscope.c` and the DSO paths of `dsl.c` (Phase 3e). Most were confirmed by
running the C driver against the simulated device.
- **Calibration block overflow.** `dso_load_eep()` and `SR_CONF_ZERO_SET` treat the 30-byte
  `struct cmd_zero_info` as 32 bytes: the layout has a 2-byte `digi_fgain` that the struct lacks.
  - Loading reads the last two combined fine gains from the stack behind the struct.
  - Saving writes those two bytes past it (stack corruption). It still sends only 30 bytes, so
    those two gains are never stored.
- **NULL dereferences.**
  - `config_get(SR_CONF_PROBE_VGAIN_RANGE)` reads `ch->vdiv` without checking `ch`; the dump in
    `drv-diff` crashed C on it.
  - Selecting a device mode the profile does not offer (e.g. DAQ on an MSO-E8): C stores the mode,
    finds no channel mode for it and deletes every channel. The next mode change dereferences the
    empty list (`sdi->channels->data`). For DSO it also divides by zero (`dso_depth / num_probes`).
  - An MSO-E8 has no firmware or bitstream names (NULL). If it ever reports its FPGA unconfigured
    or comes up without firmware, C builds the path with `strlen(NULL)`.
- **MSO-E8 range mismatch.** `vdivs_dso_e8[]` offers 218 mV/div; its `vga_defaults[]` entry says
  182. No lookup by V/div finds that entry. The default gain and offset are 0, and
  `PROBE_VGAIN_DEFAULT` returns SR_OK without a value.
- **Calibration state shared between devices.** `dso_zero()` keeps its progress in function statics
  (`warm_done`, `trans_fix_done`, `mid_zero_done`, the margins, the saved V/div). Two DSCopes
  calibrating at once share one state, and a calibration left unfinished carries into the next
  device's.
- **Wrong channel in the calibration.** When the offset scale is fixed (non-PREOFF devices), C
  sends channel 0's offset command whichever channel's `vpos_trans` changed.
- **Undefined shifts.** Each are `int` shifts that x86 happens to mask.
  - Channels that appear in the MSO-E8's logic mode have `bits = 0`, and the DSO offset command
    computes `1 << (bits - 1)`.
  - The DSO trigger-value command shifts each channel's value by `8 * (index + 1)` bits: 32 and more
    for the E8's eight logic channels.
  - Values of 128 and above at channel 2 sign-extend into the command's upper half.
- **Uninitialised reads.** In DSO and DAQ mode `receive_transfer()` adds the uninitialised
  `logic.length` to `num_bytes`. Nothing reads `num_bytes` in those modes.
- **Dead code.**
  - `dso_tune()`: nothing sets `devc->tune`.
  - The DSO and DAQ branches of `dslogic.c`: no DSLogic profile offers those modes.
- **Re-opening an open device** claims the interface again and repeats the FPGA and security checks.
  Ported as is.

## Planned behaviour deviations (Rust intentionally differs)

| Area | C behaviour | Rust behaviour | Phase |
|---|---|---|---|
| DSCope EEPROM calibration (F24) | Reads and writes 2 bytes past the 30-byte `cmd_zero_info` struct; the last two combined fine gains come from the stack and are never stored | Same 30 bytes on the wire; the two gains that do not fit read as 1.0 (code 0) | 3e ✔ |
| DSCope calibration progress (F24) | Function statics shared by all devices | Per device (`ZeroCal`) | 3e ✔ |
| DSCope `PROBE_VGAIN_RANGE` without a channel | NULL dereference | SR_ERR | 3e ✔ |
| DSCope device mode the profile does not offer | All channels deleted, later NULL dereference or division by zero | Same state (no channels), no crash; a mode value outside 0-2 is SR_ERR_ARG | 3e ✔ |
| MSO-E8 without firmware/bitstream name | `strlen(NULL)` crash | Error: firmware missing | 3e ✔ |
| DSCope `dso_tune()` | Dead code | Not ported | 3e ✔ |
| DSCope channel values during a capture (`hw_offset`, calibration V/div and offsets) | Written into `sdi->channels` by the driver | Copied into the C channel list before each packet is forwarded and after each poll | 3e ✔ |
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
| Decoder engine teardown | `wait()` returns NULL without an exception (Python: SystemError, which `except Exception` in a decoder can swallow) | Raises `sigrokdecode.Terminate`, a BaseException | 2 ✔ |
| Malformed `wait()` conditions | NULL without an exception ("returned NULL without setting an exception") | SystemError naming the problem | 2 ✔ |
| `metadata()` raising | The exception stays pending on the calling thread | Discarded | 2 ✔ |
| `srd_exit` | `Py_Finalize()` | Python is not finalized (PyO3 does not support it); the process exits anyway | 2 ✔ |
| Decoder directory that cannot be listed | Tried as a zip archive (zipimport) | Logged and skipped | 2 ✔ |
| Python bytecode | `__pycache__` written into the decoder directories | `sys.dont_write_bytecode` | 2 ✔ |
| Demo driver, buffer overruns (F18) | Reads past the analog pattern, the logic random buffer, the DSO frame; crashes on some inputs | Same packets where C's are defined; zeros for the rest; no crash | 3a ✔ |
| Demo/file driver packet fields (F20) | Left uninitialised | 0 | 3a/3b ✔ |
| Demo driver, channel keys without a channel | NULL dereference | SR_ERR_ARG | 3a ✔ |
| Demo/file `map_unit` | Pointer into a freed GVariant | Copied string (interned in the shim) | 3a/3b ✔ |
| Demo random data and timing | `rand()` and the wall clock | Same algorithm on `dsview_drivers::sys` (process-wide RNG and clock, replaceable in tests) | 3a ✔ |
| DSLogic command header offsets (F22) | Stack contents | 0 | 3d ✔ |
| DSLogic status polls (arm, FPGA configuration) | Loop until the bit is set | At most 200 reads, then an error | 3d ✔ |
| DSLogic stop | Waits for the device to drain, without a time limit | Waits 3 s, then cancels the transfers and ends the capture | 3d ✔ |
| DSLogic `PROBE_VDIV` list without a range table | NULL dereference | Empty list | 3d ✔ |
| DSLogic profile check | Reads manufacturer/product over USB (GET_DESCRIPTOR) | Takes them from the OS device list (sysfs on Linux), so no USB traffic while scanning | 3d ✔ |
| Library: a device that reconnects within 500 ms | Gets a new `ds_device_handle` (its new `libusb_device`) | Keeps its handle; only its bus address changes | 4a ✔ |
| Library: USB events | libusb hotplug callbacks; the hotplug thread also pumps libusb events, so it can complete the drivers' transfers alongside the collect thread | nusb's hotplug stream on a thread of its own; transfers are completed only by their driver | 4a ✔ |
| Library with `DSVIEW_IMPL_ACQ=rust` | — | Every device runs on its Rust driver (`DSVIEW_IMPL_DRV_*` apply to the C library only) | 4a ✔ |
| DSLogic DSO and DAQ modes | Code in `dslogic.c`, unreachable: no DSLogic profile offers them (F24) | Not ported; setting such a mode fails | 3e ✔ |
| DSLogic/DSCope common key without its channel (`PROBE_VDIV` etc. with `ch` NULL) | SR_ERR_NA (the driver's own switch is tried after `dsl_config_get()` fails) | Same (3d answered SR_ERR) | 3e ✔ |
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
| Phase 2: fresh pin tuple per `wait()` | Kept C's refilled tuple (F13) | The exit gate is identical output on all decoders; the fix waits for the default flip |
| Phase 2: `Quirks::FIXED` mode | Not implemented | Nothing needs it before the default flip |
| Phase 2: `OUTPUT_LOGIC` for `pca9571`/`tca6408a` | Not added | It would change their output from C's (an error); added later with the fixes |
| Phase 2: decoder options from files reset (`objdump`, `elffile`, `mapping`) | Moved to the GUI work (Phase 4/5) | The Qt GUI applies saved options (StoreSession); the engine cannot tell where an option came from |
| Phase 2: decoder corpus from sigrok-dumps and `dsview-siggen` | I2C, SPI and UART generators inside `decode-diff`, plus noise signals and protocol.demo | Raised decoder coverage from about 80 to 120 of 153 without new test data; sigrok-dumps comes with the CLI (Phase 4) |
| Phase 2: one interpreter per build, tested on 3.10 and 3.12 | Both engines use the same interpreter (`PYO3_PYTHON`, else `python3`); the oracle no longer uses pkg-config `python3-embed` | A single process needs a single libpython; CI uses the runner's Python |
| USB traces in the repository | Recorded 2026-10-17 (11 scenarios plus startup). The pcapng files stay out of git; `testdata/usb/dslogic/manifest.json` pins their sha256 | 16 MB of binary traces; the plan's separate test-data repository is not set up yet |
| Phase 3a: demo random mode checked "structurally only" | Checked packet for packet | Both drivers run on one virtual clock and one random sequence (`dev/dsview-oracle/src/drv.rs`; `demo.c` is built with `g_timer_*`, `g_usleep`, `rand`, `srand` mapped onto it), so random and time-based output is deterministic |
| Phase 3: `compat/rs_driver.c` | `libsigrok4DSL/hardware/rust/rs_driver.c`, next to the other shims (`output/rs_output.c`, `rs_decode.c`) | Same place as the code it stands in for |
| Phase 3: `dsview-usb-fake` for the demo/file drivers | The real libsigrok4DSL `ds_*` API in the oracle, with the C and the Rust driver selected by `DSVIEW_IMPL_DRV_*` | The demo and file drivers do no USB; the fake is for 3c/3d |
| Oracle C build | Now `NDEBUG` (as DSView's Release build) and `DSVIEW_RUST_CORE` (the shims are linked) | The driver oracle needs the shipped app's behaviour (`sr_config_get` asserts are off there) and the Rust drivers behind the same API. The goldens are unchanged |
| Phase 3c: `dsview-usb-fake` (a libusb with simulated devices for C and Rust) | Replay of the Phase 0 C captures against the Rust driver, plus usbmon recordings of both drivers on the real DSLogic compared with `usb-diff` | The captures pin the C driver's real traffic, including the device's answers and data; the hardware is here. The fake (and the simulator) come with the DSCope, which is not |
| Phase 3c: recording backend (`DSVIEW_USB_RECORD`) | usbmon (`dumpcap`) records any driver; `usb-dump` and `usb-diff` read the files | usbmon needs no driver support and records C and Rust alike |
| Phase 3: Rust USB through the C library's hotplug | The Rust driver enumerates with nusb; the shim keys each device by lib_main's `libusb_device`, so the C hotplug, reconnect and duplicate checks keep working | lib_main's hotplug thread stays until the session moves to Rust (Phase 4) |
| Phase 3e: `dsview-usb-fake` as a cdylib exporting `libusb_*` | `dev/dsview-oracle/csrc/fake_libusb.c`, linked into the oracle instead of libusb, over `dsview_usb::sim` | The oracle already runs the C library through its `ds_*` API; linking the fake there needs no `LD_PRELOAD` and gives C and Rust one simulated device |
| Phase 3e: separate `DsLogicSim`, `DsCopeSim`, `E8Sim` | One `DslSim`, configured from the profile (USB identity, header size, FPGA state, NVM, front-end bias) | The firmware protocol is the same for all three; what differs is the data, and the DSO status block covers the DSCope and the E8 |
| Phase 3e: EEPROM layout "deliberately fixed" to 32 bytes | The 30 bytes C reads and writes; the two gains that do not fit are 1.0 | Writing 32 bytes would change the NVM layout under C, which still reads 30; the wire format stays compatible both ways |
| Phase 3e: `ZeroCalState`/`TuneState` | `ZeroCal` per device; no tune state | `dso_tune()` is dead code (F24) |
| Phase 3: DSCope Rust default | Still opt-in (`DSVIEW_IMPL_DRV_DSCOPE=rust`) | As planned: the default flips after a hardware confirmation |
| Phase 4: `compat/ds_api_rs.c`, `DeviceManager` | `libsigrok4DSL/ds_api_rs.c` over `dsview_session::Library` | Same place as the other shims; the library is lib_main.c's device manager and capture thread in one |
| Phase 4: CaptureSession state machine, DecodeScheduler | Moved to 4b (with the control server and `dsview-cli serve`) | They replace Qt's SigSession, which stays for the Qt GUI; 4a replaces what is under it |
