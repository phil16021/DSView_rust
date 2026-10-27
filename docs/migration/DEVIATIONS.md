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
- **Resolved 2026-10-27 (M7b, F32)** without a second mode:
  - the one engine's output no longer depends on chunking;
  - DSView's own chunking still gives the stored golden line for line, so nothing needed listing
    or re-blessing;
  - `Quirks::FIXED` is not built.

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
- **Fixed 2026-10-27 (M7b).** `cfmt::si_string_u64` pads the remainder as a decimal fraction and
  drops trailing zeros:
  - 131072 Samples is "131.072 k Samples";
  - 1.05 MHz is "1.05 MHz", where C wrote "1.50000 MHz".
  - This covers the CSV header, the srzip metadata (read by sigrok-cli and PulseView) and the GUI's
    rate labels. `.dsl` headers have their own writer, which never writes a fraction.

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

**Rust behaviour: fixed 2026-10-27 (M7b).**
- **Store** (`dsview-store/src/logic.rs`):
  - single mode keeps the overrun out of `loop_offset`;
  - `capture_ended` trims to `total_sample_count`: the overrun bits of the last 64-sample group
    repeat the last sample, so there is no phantom edge, the rest of the leaf is zeroed, and the
    leaf's mip-map is rebuilt;
  - loop mode is unchanged.
- **Saved files:** the last `.dsl` block is cut at whole 64-sample words (the format's unit), and
  the header carries the exact length.
- **File sessions:** `commit_settings` no longer rounds a file's length up to 1024. C never runs it
  for a file, whose rate and count selectors are disabled.
- **Tests:**
  - `logic::tests::{unaligned_capture_is_not_shifted, any_length_reads_back}` (a proptest over
    lengths, channel counts and packet sizes);
  - `session::unaligned_dsl_round_trip` (open, save, ranged save, reopen a 1001-sample `.dsl`).
- **Departure from DSView:** DSView still shows such captures shifted and saves them cut.

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

**Rust behaviour: fixed 2026-10-27 (M7b).** `wait()` returns a fresh tuple each time, so a kept result
no longer blocks the next call.
- Test: `tests/keep_pins.rs`, with a test decoder written like `pjdl`; before the fix it failed with
  exactly this SystemError.
- No PJDL or GPIB capture is in the tree, so `pjdl` and `ieee488` themselves are only covered by
  that mechanism test.

### F14. A negative or oversized `skip` count makes wait() fail later
**Observed (Phase 2, `flexray`):** `create_term_list` reads the count with `PyLong_AsUnsignedLongLong`
and does not check for an error. A negative count becomes 2^64−1 and leaves an OverflowError pending.
`wait()` keeps scanning with the other conditions, and when one matches it returns with the exception
set, so Python raises the same SystemError as in F13. `flexray` computes `pos - self.samplenum`, which can
be negative.

**Rust behaviour: fixed 2026-10-27 (M7b).**
- A negative count is `skip: 0`: the sample asked for is already behind, and the decoder carries on.
- A count that does not fit 64 bits fails that `wait()` at once with "Failed to get number of
  samples to skip."
- No error is left pending any more, so F17 cannot arise.
- Tests: `tests/pending_error.rs` (both cases).

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

**Fixed 2026-10-27 (M7b)** in the shipped decoders; each change is marked `DSViewR F16` and is to be
sent upstream.

| Decoder | Cause | Fix |
|---|---|---|
| `gpib` | With `sample_total` set, the skip count reaches 0 at that sample, and `skip: 0` returns the same sample again, forever | Drop the skip condition once the sample is reached |
| `ltc26x7` | An I²C address below the chip's range gives a negative `addr`. `convert_ternary_str`'s `divmod(n, 3)` stays at -1, so it loops while its list grows | Addresses outside 0..26 are annotated "Unknown address"; the conversion guards n ≤ 0 |
| `onewire_link` | Not reproduced | — |
| `midi` | Not reproduced | — |

- **Tests:** `dsview-decode/tests/decoder_hangs.rs`, one minimal input per decoder. Each hung before
  the fix; a watchdog aborts the test after 20 s.
- **`onewire_link` and `midi`:** not reproduced:
  - the decoder soak hung on neither in 200 runs (30 600 stacks);
  - `onewire_link` also ran on the demo's signals with the option at 2, 0.5, 0 and -1 µs;
  - the Phase 2 `onewire_link` case predates the engine fixes;
  - the nightly soak keeps watching both.
- **Decoder soak** (`cargo xtask decoders soak`, `dev/dsview-testkit/src/bin/decoder-soak.rs`, the
  former `decode-diff` generator without the C side), 200 runs from seed 100000:

  | | Hangs | Notes |
  |---|---|---|
  | Before the fixes | 34 of 30 600 stacks (`gpib` 18, `ltc26x7` 16) | `lfast` failed in 199 runs and `sipi`, stacked on it, in 200 |
  | After | 0 | |

Other decoder failures seen identically in both engines:
- `lfast`: NameError, `bMoreMatch` is not defined.
  - **Fixed 2026-10-27:** the timeout check is `self.matched & 0b10`. The old text,
    `& 0b10 == 0b10`, also tested bit 0 by precedence.
  - Test: `decoder_hangs::lfast_runs_past_its_first_edges`.
- `ir_irmp`: needs `libirmp.so`, which is not built.
  - **Decision 2026-10-27 (user):** DSViewR ships no C, so the library is not bundled.
  - The decoder now fails with "IRMP library not installed; ir_irmp needs libirmp.so (…)".
  - It works for anyone who installs libirmp.
  - Test: `decoder_hangs::ir_irmp_without_its_library_says_so`.
- `pca9571` and `tca6408a`: need `OUTPUT_LOGIC`, which DSView's engine does not have.
  - **Added 2026-10-27 (M7b):** `srd.OUTPUT_LOGIC` (4, as libsigrokdecode). Puts are checked
    (`[channel, bytes]`, the channel one of `logic_output_channels`) and go to an `OUTPUT_LOGIC`
    callback if there is one.
  - DSViewR shows no logic outputs; the decoders' annotations now work.
  - Tests: `tests/logic_output.rs`. Without the constant, `pca9571` failed in `start()` with the
    AttributeError.
- `ir_irmp`'s `__del__` raises AttributeError at exit.
  - **Fixed 2026-10-27:** `getattr(self, '_inst', None)`; after a failed `LoadLibrary()` the
    attribute does not exist.
  - Test: `decoder_hangs::irmp_library_del_after_failed_load_is_quiet`.

### F17. Whether a pending wait() error survives depends on CPython's type cache
**Moot since 2026-10-27:** the F14 fix leaves no error pending.

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

**Rust behaviour:** fixed 2026-10-27 (M7b).
- The buffer is at least one whole cycle (206 bytes), so a short analog capture delivers its
  samples and ends.
- Test: `demo::short_analog_capture_delivers_data`; before the fix the capture produced only END.

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
  channels 3–14, and channel 15's data is lost.
  - **Fixed in Rust 2026-10-27 (M7b):** each enabled channel plays its own `L-<index>`.
  - Test: `demo::protocol_playback_skips_disabled_channels`.
- **Dangling `map_unit`.** `config_set(SR_CONF_PROBE_MAP_UNIT)` keeps the pointer into the GVariant,
  which `sr_config_set` frees right after. Rust copies the string; the shim hands out an interned
  copy.
- **NULL channel crash.** Channel keys (`PROBE_EN`, `PROBE_VDIV`, …) with no channel dereference
  NULL in both drivers. Rust returns SR_ERR_ARG. The GUI always passes a channel.
- **V/div 0** (found by the `demo` fuzz target, R1). C stores it and divides by it when it scales
  the DSO data. Rust refuses it with SR_ERR_ARG; the GUI only offers the listed V/div values.
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
  The Rust driver uses the open handle (no second claim) and repeats the checks.
  - **Kept (M7b review, 2026-10-27):** the repeated checks only read the device's state, and the
    DSCope/MSO-E8 open path cannot be tested on hardware here, so it stays as it is.

### F25. GUI session and decoder-stack defects (Phase 4b)
Found while porting `SigSession`, `DecoderStack`, `SamplingBar` and `DsoSignal` for
`dsview-cli serve`.
- **Decoder order depends on the install.** `srd_decoder_load_all()` loads in directory order
  (`g_dir_read_name`). On ext4 that order follows a per-directory hash, so two installs of the
  same decoders list them differently.
  - It also changes the row order of decoder stacks. `DecoderStack` keys its rows by
    `srd_decoder` address, and the decoders are allocated in load order.
  - On this machine the control transcript recorded in Phase 0 no longer matched the Qt GUI after
    a reinstall, with C or Rust decoding (`list_decoders`, and the rows of the I²C + EEPROM stack).
- **Decoder text used as a printf format.** For an annotation with a number,
  `Annotation::annotations()` replaces `{$}` with `%s` and passes the decoder's text to `sprintf`
  as the format. Any other `%` in the text is a conversion with no argument: `%s` reads a stray
  pointer, `%n` writes through one. Decoders build their texts from the decoded data, so the
  data can reach the format string.
- **Interning by concatenated text.** `AnnotationResTable` keys an annotation by all of its texts
  run together. `["ab", "c"]` and `["a", "bc"]` share one entry, so the second shows the first one's
  texts.
- **Dropped annotations are counted.** An annotation of a class that no row lists is counted in
  `get_result_count()`, then dropped with only a log line.
- **Decode progress.**
  - It divides by the last sample index, so a one-sample capture divides by zero (SIGFPE).
  - Progress can briefly exceed 100.
  - When there is no data, it stays at 0 with `_is_decoding` set.
- **Burst width of DSO channel 1** (`DsoSignal::paint_mid`). It is computed from
  `ch1_cyc_flen + ch1_cyc_llen`; channel 0 uses `ch0_cyc_tlen + ch0_cyc_llen`. A copy-paste
  slip: channel 1's burst width is a fall time plus a tail.
  - **Fixed 2026-10-27 (M7b):** both channels use the cycle time plus the tail
    (`dsview-dsp/src/measure.rs`).
  - Test: `measure::tests::hardware_burst_width_is_the_same_for_both_channels`; before the fix
    channel 1 gave 55 against channel 0's 1005.
- **View indexes past the end.** `load_channel_view_indexs` assigns the saved positions to the
  signals by position. When the file lists fewer channels than there are signals, it reads past
  the vector.
- **Uninitialised software depth on non-x86.** `update_sample_count_selector` sets `sw_depth` only
  under `__x86_64__`/`__i386`. On aarch64 a stream or RLE capture computes its depth list from
  garbage.
- **Capture progress with no sample limit.** `get_capture_status` divides by the limit, so a limit
  of 0 gives an infinite double cast to `int`.
- **First probe by pointer order.** `Decoder::first_probe_index()` takes the first entry of a map
  keyed by `srd_channel*`. Which bound channel's signal supplies the capture therefore depends on
  heap addresses.

### F26. The DSO envelope (and a gap in the Rust store)
- **A Phase 1b mistake.**
  - The Rust DSO store was built without the envelope, on the claim that nothing enables it.
  - `DsoSignal::paint_mid` does enable it: from 256 samples per pixel it draws the frame from
    `get_envelope_section`.
  - With `DSVIEW_IMPL_STORE=rust`, deep frames (more than about 256k samples on a 1000 px trace)
    were drawn empty when zoomed out.
  - No test saw it: the demo oscilloscope's 10,000 samples never reach that zoom. Fixed in M6.0;
    `store_ad_diff` now compares envelope sections.
- **Stale entries after a shorter frame.** `append_payload_to_envelope_levels` computes only from
  the previous length onwards.
  - When a non-instant frame is shorter than the one before (and `samplerate_tog` is not set),
    nothing is recomputed, and the shorter frame shows the previous frame's envelope. A longer
    frame keeps the old envelope up to the previous length.
  - **Fixed 2026-10-27 (M7b):** a frame that replaces the previous one gets its envelope computed
    whole; only instant captures, which append, extend it.
  - Test: `dso::tests::envelope_follows_each_frame` (fails before the fix: the shorter frame showed
    the previous frame's values).
- **One short channel ends the update for all.** If a channel has fewer than 256 samples, the
  function returns before the other channels and before marking the envelope as built.
  - **Fixed 2026-10-27:** all channels have the frame's length, so this means a frame shorter than
    one entry. Every level is now emptied, so no level keeps an older frame's length; the same for
    the levels above the last one a frame fills.
- **The section index is a storage position.** `get_envelope_section(probe_index)` indexes
  channels by storage position. `DsoSignal` passes `index % enabled channels`, which matches only
  while the enabled channels are numbered from 0.
  - **Fixed 2026-10-27:** the DSO view looks the position up (`DsoStore::channel_order`). With
    channels 1 and 3 of four enabled, both used to draw channel 3's envelope.
  - Test: `dso::tests::envelope_by_storage_position`.

### F27. Quirks of the Qt time view (ported as they are)
The Rust GUI's viewmodel reproduces these for parity; they are candidates for fixing after M6e.
- **Integer view offset.** `View::_x_offset` is a whole pixel count, so each zoom step loses the
  sub-pixel position and repeated zooming drifts away from the point under the mouse.
  - **Fixed 2026-10-27 (M7b):** zooming carries the fraction it rounds off (`Viewport::x_frac`) to
    the next zoom. Drawing still uses whole pixels.
  - Before the fix, 4–6 zoom-ins moved the sample under the pointer by up to about 5 px.
  - Test: `viewport_props::zooming_in_does_not_drift`.
- **Kinetic scroll stops abruptly.** The swipe's strength is an integer number of pixels per tick;
  the damping rounds it to 0 after a few ticks instead of easing out.
  - **Fixed 2026-10-27:** the strength is fractional and damped geometrically, with a sub-pixel
    carry.
  - A 30 px/tick swipe now travels about 620 px over more than 60 ticks; it travelled 340 px in
    25 ticks.
  - Test: `input::tests::swipe_eases_out`.
- **Cursor numbers are list positions.** Deleting a cursor renumbers (and recolours) every cursor
  after it.
  - **Fixed 2026-10-27 (M7b):** a cursor keeps its number and colour until it is deleted, and a new
    cursor takes the lowest free number.
  - Shown on the ruler label, line marker, go-to strip and measure dock.
  - The measure dock's distance and edge rows keep pointing at the same cursors.
  - Tests: `cursors::tests::numbers_are_stable`; snapshot `ruler_cursor_strips`, where the surviving
    cursor stays "3".
- **Ruler labels.** The tick spacing is a multiple of 10 samples that leaves room for a label, not
  a 1-2-5 sequence (`ScaleUnits` is declared and unused), so labels are not round numbers.
  - **Fixed 2026-10-27:** major ticks are 1, 2 or 5 × 10ⁿ apart, in seconds, or in samples when the
    ruler counts samples; still at least 10 samples and room for a label.
  - The logic ruler reads 1.20, 1.40 … ms (was 1.27, 1.38 …).
  - The DSO ruler now lines up with the time/div grid: 0.50 µs ticks on 0.5 µs divisions, where it
    had 0.40 µs.
  - Test: `ruler::tests::major_ticks_follow_1_2_5`; snapshots with a ruler updated.

### F28. Oscilloscope view findings (M6d)
- **The Y measurement is unreachable.** `Viewport::onDsoMouseRelease` starts `DSO_YM` only when
  `_hover_hit` is set, and `paintMeasure` only ever sets it to false. The painting code exists;
  no user can reach it. Not ported.
- **Stale math envelope levels.** `MathStack::append_to_envelope_level(true)` rebuilds level 0
  but the higher levels only from their previous length on, unless the length is unchanged. When
  a frame's length changes, the upper levels keep entries of the previous frame. Rust rebuilds
  every level per frame (identical when the length is unchanged, the normal case).
- **Auto-set writes the trigger level twice.** When auto-V ends inside `auto_set`, `autoV_end`
  sets the level and the same pass sets it again from the same values.
  - **Fixed 2026-10-27 (M7b):** the second write is skipped once auto-V has ended. The value was
    the same, so nothing visible changes.
  - Test: `autoset::tests::steps_until_it_fits`.
- **The demo's last DSO sample.** In instant mode the C demo's last packet reads past its buffer
  (F18); the Rust demo pads with zeros. The histogram and the Lissajous figure show that sample
  (code 0, +5 V on the demo's range) as an outlier.

### F29. Docks and dialogs findings (M6e)
- **The region dialog is never opened.** `pv/dialogs/regionoptions.cpp` is built and included by
  `mainwindow.cpp`, but nothing creates it. Not ported; the decoder options carry the decode
  region (M6c).
- **A ranged `.dsl` save writes the whole capture's length.** With a start cursor inside a block,
  `StoreSession::save_logic` writes `total samples` of the full capture and a first block that
  starts mid-block; a range over several blocks does not load back as written. Rust repacks the
  range into whole blocks, writes the range's length and moves the trigger position by the
  range's start.
- **Ctrl while dragging header labels.** `Header::mouseMoveEvent` moves logic traces only while
  Ctrl is *not* held; Ctrl at the press only adds the trace to the selection.
  - **Fixed 2026-10-27 (M7b):** the selection drags with Ctrl held too.
  - Found on the way: a plain click, with no movement, snapped the traces to the 10 px grid,
    renumbered them and cleared the selection. A move now starts only once the pointer moves.
  - Test: `snapshot::header_drag_and_multi_select`, which drags with Ctrl held.
- **"Don't ask to save" is not asked everywhere.** `SamplingBar::on_device_selected` offers to
  save without looking at `dontAskSaveOnExit`; a detach, a new device and closing the window do.
  - **Fixed 2026-10-27:** choosing a device honours it too.
  - Test: `app_flow::device_switch_respects_dont_ask`.
- **Hover snapping ignores the trigger.** `View::eventFilter` snaps the hover point to multiples
  of the smallest ruler period counted from sample 0, while the ruler counts from the trigger.
  - **Fixed 2026-10-27:** the hover point snaps to the ruler's minor ticks, counted from the
    trigger.
  - Test: `ruler::hover_tests::hover_snaps_to_ticks_counted_from_the_trigger`.
- **The first view of a hardware capture.** Qt starts at `_scale(10)` (10 s per pixel), which the
  first limit update clamps to the whole capture. The Rust view started at 10 ns per pixel, so
  a first DSLogic capture opened zoomed far in (found on the DSLogic Plus); fixed.

### F30. Settings findings (M6f)
- **The protocol-format cap never applies.** `AppConfig::SetProtocolFormat` trims the list only
  `while (size < 15)` inside `if (size > 15)`, which never runs. The list grows without limit.
  Rust keeps a map without a cap, which is the same in effect.
- **Log Options opens a new log file in overwrite mode.** On OK, `on_action_setting_log` calls
  `dsv_log_enalbe_logfile(false)` whatever "Append mode" says. The setting only takes effect at
  the next start.
  - **Fixed 2026-10-27:** the file opens in the chosen mode (`log::apply_options`).
  - Test: `log::tests::file_levels_and_clear`.
- **The manual for German does not exist.** `MainWindow::openDoc` opens `ug<language>.pdf`, but
  only `ug25.pdf` and `ug31.pdf` ship, so German opens nothing. Rust falls back to the English
  guide.
- **The window position can be off every screen.** The saved `x`/`left` comes from whichever
  monitor the window was last on; `ReadSettings` moves it back when it shows less than 70 × 70 px
  of the screen. Ported.
  - **Open (2026-10-27):** with several monitors the check can misfire. egui reports only the
    current monitor's size, not its position, so a window correctly on a second monitor is judged
    as if that monitor started at (0, 0), and may be moved to the first.
  - The fix needs the monitor layout from winit; tracked as a follow-up.

### F31. Runtime findings (M6g)
- **A missing FPGA bitstream shows as a USB error.** `dsl_fpga_config` sets
  `SR_ERR_FIRMWARE_NOT_EXIST` when it cannot open the file, but `dsl_dev_open` then overwrites it
  with `SR_ERR_DEVICE_USB_IO_ERROR`. The user reads "USB IO error!" for a file missing on disk.
  Rust checks the file first and keeps "firmware not exist" (row below).
- **The FX2 firmware files of 14 models are not in the tree:**
  - DSLogicBasic, DSLogicPlus, DSLogicU2Basic, DSLogicU2Pro16, DSLogicU3Pro16, DSLogicU3Pro32;
  - DSCopeB20, DSCopeC20, DSCopeC20B, DSCopeC20P, DSCopeU2B100, DSCopeU2B20, DSCopeU2P20,
    DSCopeU3P100.
  - Devices that boot their firmware from flash do not need them; the DSLogic Plus here is one.
  - A device that needs one stays unready, and the log names the file
    ("Unable to open firmware file … for reading"), in C and in Rust.
- **`update_dso_data_scale` is never called.** The snapshot's data scale is set only with the
  sample rate, so a V/div changed after a capture stretches the trace and keeps its voltages.
  The control API now does the same (row below).
  - **Fixed 2026-10-27 (M7b).** The session's `cap_vdiv` is the one record of the V/div the data
    was taken at:
    - while a capture runs, a V/div change becomes it, since the frames that follow are in the new
      V/div. Before, the GUI and the control API kept the V/div of the start, so after a dial step
      during a run, DSO voltages were off by the ratio (the demo's Vpp halved when V/div doubled);
    - a stopped capture keeps it.
  - **Trace scale:** the view derives the trace's stop scale from it (`cap_vdiv / V/div`) instead
    of counting dial steps. A V/div set from Device Options, auto-set or the control socket now
    rescales a stopped trace too; before, only the header dial did.
  - **Tests:** `transcript::dso_vdiv_changed_while_running` and
    `dsoview::tests::stopped_trace_follows_vdiv_from_any_path`.

### F32. Decoder chunk edges (M6g)
- **libsigrokdecode's results depend on the chunks.** The new chunking-invariance test found:
  - An annotation can move by one sample when its decoder's `wait()` ends on the first sample of
    a chunk. On `protocol.demo` with 64-sample chunks, 43 of 9337 output lines move: CAN-FD and
    UART, start and/or end by one.
  - Chunks of a single sample decode nothing.
- **The C engine does the same:** `oracle-decode` and `rust-decode` agree line for line at every
  chunk size tried (whole, 64, 1). So the Rust engine keeps C's behaviour, as Phase 2 required.
- DSView's own chunks (the decoder thread's blocks) are far larger than one sample, so the empty
  case never happens in the application. The ±1 shift does.
- **Fixed 2026-10-27 (M7b).**
  - **Cause:** after a match on a chunk's last sample, the next `wait()` used up the chunk with the
    step past the match, but kept the "matched" flag. With the next chunk it stepped again and
    skipped that chunk's first sample.
  - **Fix:** the flag is cleared once the step is taken (`instance.rs`, `find_match_in`).
  - **Tests:** `tests/chunking.rs` requires identical output for chunks of 1, 2, 3, … 4096
    samples.
  - **`rust-decode protocol.demo`:** with chunks of 64, 77 and 1000 samples, 43, 42 and 3 lines
    used to differ from the default chunking; now none do. The default output equals the stored
    golden.

### F33. The analog envelope writes one entry past its ring (R1, first CI run)
- **Found by** the `oracle-asan` CI job: it had never run before the branch was pushed. One
  channel, 20 samples total, packets of 4, 12 and 3 samples.
- **The defect:** `AnalogSnapshot::append_payload_to_envelope_levels` continues level 0 at
  `e0.samples + prev_length`.
  - When the sample ring ends in its last partial 16-sample group, `prev_length` equals the
    ring's entry count `e0.count`. That needs a total that is not a multiple of 16, e.g.
    1000 samples.
  - A packet that completes no new group then takes the "wrapped" branch. It computes
    `total / 16` entries and writes the first one at `e0.samples[e0.count]`, before the wrap
    check. That is a 2-byte heap write past the `malloc`ed ring.
- **Rust behaviour:** the store computes the same entries and drops the one that has no slot
  (`dsview-store/src/analog.rs`). The visible envelope is the same as C++'s.
- **Tests:**
  - Unsanitized, `store_ad_diff` keeps feeding such inputs and compares the results.
  - Under ASan (`ORACLE_SANITIZE=1`, `cfg(oracle_sanitize)`) it uses whole 16-sample totals,
    and no packets past the ring (F12). The C++ code stays memory-clean on everything else.

### F34. `ds_lib_exit()` leaves a dangling active device (R1, first CI run)
- **Found by** the golden check on the GitHub runner: `oracle-dsl` crashed there, and valgrind
  and ASan show the same read locally.
- **The defect:** `ds_lib_exit()` destroys every device, including the active one. It does not
  clear `lib_ctx.actived_device_instance`. After a second `ds_lib_init()` in the same process,
  the next `ds_active_device()` reads the freed instance (`lib_main.c:495`).
- **Reach:** DSView starts the library once per process, so the application never hits it. The
  oracle did: it ran the library twice per file.
- **Rust:** `dsview_session::Library` owns its devices, so there is nothing to dangle.
- The oracle now starts the library once per process and opens the next file as DSView does
  (`csrc/oracle_dsl.c`). The goldens are unchanged.

### F35. Decoders that raise on unusual data (decoder soak, 2026-10-27)
The first decoder soak (200 runs from seed 100000) found decoders that stop with a Python exception
on data or options they did not expect. Each stops only its own stack. Not fixed yet; tracked as a
follow-up.

| Decoder | Runs | Exception |
|---|---|---|
| `st7735` | 61 | `KeyError` for an unknown command byte (`META[cmd]`) |
| `spdif` | 77 | `ValueError` from `int(bits, 2)` on a bit string with digits other than 0/1 |
| `fsi` | 18 | `AttributeError`: `direction` read before it is set |
| `i2s` | 7 | `struct.error`: a sample outside `'<I'` |
| `seven_segment` | 31 | `IndexError` |
| `smart_qspi` | 4 | `IndexError` |

Expected configuration errors from random channel maps and options are not listed: SPI without
MISO or MOSI, `ac97`, `ieee488`, `graycode`'s contiguous channels, and `rgb_led_ws281x`'s colour
orders.

## Planned behaviour deviations (Rust intentionally differs)

| Area | C behaviour | Rust behaviour | Phase |
|---|---|---|---|
| DSCope EEPROM calibration (F24) | Reads and writes 2 bytes past the 30-byte `cmd_zero_info` struct; the last two combined fine gains come from the stack and are never stored | Same 30 bytes on the wire; the two gains that do not fit read as 1.0 (code 0) | 3e ✔ |
| DSCope calibration progress (F24) | Function statics shared by all devices | Per device (`ZeroCal`) | 3e ✔ |
| DSCope `PROBE_VGAIN_RANGE` without a channel | NULL dereference | SR_ERR | 3e ✔ |
| DSCope device mode the profile does not offer | All channels deleted, later NULL dereference or division by zero | Same state (no channels), no crash; a mode value outside 0-2 is SR_ERR_ARG | 3e ✔ |
| MSO-E8 without firmware/bitstream name | `strlen(NULL)` crash | Error: firmware missing | 3e ✔ |
| DSCope `dso_tune()` | Dead code | Not ported | 3e ✔ |
| A capture whose device is unplugged (M7b) | `COLLECT_TASK_END_BY_DETACHED` only when the hotplug thread saw the unplug while the capture still ran; when the failing transfers ended it first, a normal end (no "device detached" message) | A capture that ends on its own while its USB device is off the bus ends by detach; the flag is cleared when a capture starts. Test: `hotplug::detach_during_capture` (failed 21 of 120 stressed runs without it, 0 of 200 with it) | M7b ✔ |
| DSCope channel values during a capture (`hw_offset`, calibration V/div and offsets) | Written into `sdi->channels` by the driver | Copied into the C channel list before each packet is forwarded and after each poll | 3e ✔ |
| Decoder `wait()` pin tuple | One tuple object, mutated in place | A fresh tuple per call | 2 |
| Decoder options from files | Applied as stored | Path/exec options (`objdump`, `elffile`, `mapping`) from a file the user opens (`.dsl`, a loaded `.dsc`) start at their defaults; the GUI asks before setting them. DSView's own profiles and the shipped demos are trusted; a headless session keeps the defaults | M6g ✔ |
| Missing FPGA bitstream (F31) | Reported as a USB I/O error | Reported as "Firmware file is not exist", the code C meant | M6g ✔ |
| srzip chunk names past 32767 chunks | `logic-1-%d` of `(short)` index: wraps to negative numbers (more than 268 M samples at 8192 per chunk) | Counts on | M6g ✔ |
| srzip and GKeyFile value escaping | GLib escapes leading spaces and tabs only | The same, now also for `.dsl` headers (spaces were escaped only as the first character, tabs everywhere) | M6g ✔ |
| USB stop/abort | No transfer cancellation, infinite timeouts, unbounded busy-polls | FORCE_RDY → grace period → cancel all transfers; every wait has a deadline | 3 ✔ |
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
| Analog envelope over-write (F33) | Level 0 writes one entry past the ring when the ring ends in a partial 16-sample group | That entry is dropped; same envelope | 1b ✔ (recorded R1) |
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
| Decoder load order (F25) | Directory order | Name order, so the decoder list and stack row order are the same on every install | 4b ✔ |
| Decoder stack row order | By `srd_decoder` address (in practice the load order) | By load order | 4b ✔ |
| Annotation text with a number (F25) | Decoder text used as a `sprintf` format | `{$}` and the first `%s` take the number, `%%` is `%`; any other `%` is kept as written | 4b ✔ |
| Decode progress, one-sample capture (F25) | Division by zero | Progress 100 | 4b ✔ |
| Decoder's capture signal (F25) | First bound channel by pointer order | First bound channel in declaration order (required, then optional) | 4b ✔ |
| View indexes from a session with fewer channels (F25) | Reads past the vector | Extra signals keep theirs | 4b ✔ |
| Single capture in stream mode (demo, files, DSLogic stream) | Decoders start with the capture and decode while it runs | Same since Phase 5 (live decoding, STATUS "Phase 5"); in 4b the decoders ran when the capture ended | 5 ✔ |
| DSO voltages in `measure_dso`/`read_waveform` | `DsoSignal` scales by the view height, the dial and the snapshot's measure factor and data scale | Per code `vdiv × probe factor × 10 / (ref_max − ref_min)` with the V/div of the capture, which is what Qt's factors reduce to (the data scale is the view height over the code range); every value in the demo transcript is identical | 4b ✔, capture V/div M6g ✔ |
| DSO data lock, feed timer, trigger-check timer | Throttle the display; poll the hardware's trigger flag every 200 ms | Not needed without a display; `triggered` comes from the first data, as with demo and file devices in C | 4b ✔ |
| Control methods in `dsview-cli serve` | Run on the GUI thread; waiting replies answered from the event loop | One method at a time under a lock; captures, decoding and saving wait outside it | 4b ✔ |
| DSO envelope | Built when `DsoSignal` draws at ≥ 256 samples per pixel | Ported in M6.0 (until then the Rust store had none, and the Qt GUI drew deep DSO frames empty with `DSVIEW_IMPL_STORE=rust`, F26); same entries, compared against the C++ class | M6.0 ✔ |
| DSO `cal_vrms`, `cal_vmean` | Implemented, never called | Not ported | 1b ✔ |
| DSO Y measurement (F28) | Unreachable (`_hover_hit` never set) | Not ported | M6d ✔ |
| Math envelope after a frame of another length (F28) | Upper levels keep the previous frame's entries | All levels rebuilt | M6d ✔ |
| DSO persistence | A pixmap of the old frames, faded in coarse steps (×0.92 at a time) | The old frames' shapes kept and drawn with alpha exp(−age / time); same remapping on zoom/pan and the same reset on vertical changes | M6d ✔ |
| DSO voltages in the view | `get_voltage` from the snapshot's measure factor and data scale, set at `set_cur_snap_samplerate` | The same factors, taken when a capture starts (the view does not change them between frames) | M6d ✔ |
| Analog data/DSO frames/envelopes before they are written | `malloc`: uninitialised | Zeroed | 1b ✔ |
| Ranged `.dsl` save (F29) | Full capture length; first block starts mid-block | Whole blocks of the range, its length, trigger position moved by the start | M6e ✔ |
| Region dialog (F29) | Built, never opened | Not ported | M6e ✔ |
| Settings storage | `QSettings` INI (`DSView.conf`); `windowState` as Qt's dock blob | JSON (`dsview-rs.json`), the Qt file imported once on the first start; `windowState` is not imported (egui has no equivalent) | M6f ✔ |
| Text lookup | `L_S(page, id, default)` at each call site | The GUI's English texts are looked up through Qt's English text of the same id (`lang/en`), without accelerators, ellipsis or colon; texts Qt does not have stay English. Device option and decoder texts by text and `idn` as in Qt | M6f ✔ |
| CJK glyphs | Qt's font fallback | A system CJK font (Droid Sans Fallback, Noto Sans CJK SC, WenQuanYi) as egui's last fallback, else a Noto Sans CJK SC subset built into the binary (M7b: the translation's characters, the 3755 common hanzi of GB 2312 level 1, CJK punctuation; 810 KB, OFL) | M6f ✔ |
| Dark theme colours | Trace area rgb(38,38,38), widgets from `dark.qss` | The Rust GUI's dark colours since M6a (trace area 24, header 34, ruler 30); light, latte and frappé take the qss palettes | M6f ✔ |
| Icons | Every bar button and many widgets (212 SVGs incl. widget decorations) | The bar buttons and menu entries (27 icons, dark and light sets), rasterised with resvg 0.44; egui draws its own checkboxes, arrows and scrollbars | M6f ✔ |
| Esc in dialogs | Closes the focused dialog | Closes every open dialog window (egui has no focused window) | M6f ✔ |
| Shortcuts while a dialog is open | The filter sits on the view, bars and docks, so keys in a dialog are not shortcuts | Shortcuts work unless a text field has the keyboard | M6f ✔ |
| Screen check of the restored window | All screens' geometry | The current monitor's size (egui knows no others before the window exists) | M6f ✔ |
| Decoder dynamic font width | `QFont::setStretch` between the minimum and maximum percent | A smaller font down to the minimum percent; never wider than the font (egui has no stretch, and a larger font would not fit the row) | M6e ✔ |
| Y-scale badge | Reset glyph U+27F2 | A painted arrow (no egui font has the glyph) | M6e ✔ |
| Older per-language profile `<driver><mode>.ses<N>.dsc` | The file of the configured language | The same since M6f (the first such file by name only where no language is known, e.g. `dsview-cli`) | M6f ✔ |
| Saving with a range | The range dialog first, with a default file name and a "path" button | The file dialog first, then the range dialog | M6e ✔ |
| Protocol export progress | `QProgressDialog` over the export thread | A window with a progress bar and Cancel; the lines written are kept, as in Qt | M6e ✔ |

## Departures from PLAN.md

| Plan said | Done instead | Why |
|---|---|---|
| Corrosion for the CMake ↔ cargo build | A `dsview_ffi` custom target that runs `cargo build --locked` (option `DSVIEW_RUST_CORE`; on by default on Linux since R1) | Corrosion is fetched from GitHub at configure time, the kind of unpinned download the security review flagged. The custom target uses only the committed `Cargo.lock` |
| cbindgen-generated header | Done in Phase 1a: `cargo xtask gen-header`, checked in CI | — |
| `.dsc` profiles in Phase 1a | Deferred to Phase 4/5 | Profiles are read and written only by the Qt GUI (`MainWindow`, Qt JSON), so a C seam has no user; the Rust model is built with the Rust session/GUI |
| srzip/gnuplot export in Rust | srzip ported in M6g (`dsview_formats::srzip`); gnuplot dropped | srzip's entries match C's (proptest against the oracle) and sigrok-cli reads them |
| Goldens as zstd JSONL | gzip (`.jsonl.gz`, via `flate2`) | Pure Rust, no C build dependency; 60 KB for the protocol.demo decode golden |
| Phase 0 performance baselines (`get_display_edges`, ingest, decode throughput) | Deferred to the start of Phase 1b / Phase 2 | `get_display_edges` and ingest need `oracle-store` (LogicSnapshot built against Qt Core), which is Phase 1b's oracle. Decode throughput needs large synthetic captures from `dsview-siggen`; the only corpus capture (131k samples) is dominated by Python start-up |
| Phase 1b gate "ingest ≥ 1 GB/s per core" | **Met in M7b B4 (2026-10-27):** 1.1–1.3 GiB/s for a first capture and 1.2 GiB/s for a repeated one, 16 ch × 128M samples on the development VM (was 632–702 and 991 MiB/s) | The first capture was bound by first-touch page faults. Leaves now come from a pool (`logic::leaf_pool`, up to 64 idle leaves kept) that a `leaf-prefault` thread fills with touched leaves from the first packet on (up to 1024 leaves, 2 GiB); the level-1 mip-map is computed in 64-word blocks. The transposing copy was tried and dropped: no measurable gain over the strided copy. The leaf layout is unchanged, so decoders and saving read leaves as before |
| Lock-free store readers verified with `loom` (Phase 1b) | The FFI wraps each store in a mutex, like the C++ classes | The C++ GUI locks around every call anyway; lock-free readers matter for the Rust GUI and capture path (Phase 4/5) |
| Annotation store in `dsview-store` (Phase 1b) | Moved to Phase 2 | Annotations are produced and owned by the decoder engine |
| Decoder pin identity in the store tests | The differential tests track DecoderStack's pinned leaf by position, not address | DecoderStack compares leaf addresses; glibc may give a new leaf a freed leaf's address, so the C++ behaviour itself depends on the allocator |
| Phase 2: fresh pin tuple per `wait()` | Kept C's refilled tuple (F13) until M7b; fixed 2026-10-27 | The exit gate is identical output on all decoders; the fix waited for the default flip |
| Phase 2: `Quirks::FIXED` mode | Not built: the F32 fix makes the one engine chunk-independent (2026-10-27) | DSView's chunking gives the golden unchanged, so a second mode has nothing to do |
| Phase 2: `OUTPUT_LOGIC` for `pca9571`/`tca6408a` | Added 2026-10-27 (M7b) | It changed their output from C's (an error), so it waited for the fixes |
| Phase 2: decoder options from files reset (`objdump`, `elffile`, `mapping`) | Moved to the GUI work (Phase 4/5) | The Qt GUI applies saved options (StoreSession); the engine cannot tell where an option came from |
| Phase 2: decoder corpus from sigrok-dumps and `dsview-siggen` | I2C, SPI and UART generators inside `decode-diff`, plus noise signals and protocol.demo | Raised decoder coverage from about 80 to 120 of 153 without new test data; sigrok-dumps comes with the CLI (Phase 4) |
| Phase 2: one interpreter per build, tested on 3.10 and 3.12 | Both engines use the same interpreter (`PYO3_PYTHON`, else `python3`); the oracle no longer uses pkg-config `python3-embed` | A single process needs a single libpython; CI uses the runner's Python |
| USB traces in the repository | Recorded 2026-10-17 (11 scenarios plus startup). The pcapng files stay out of git; `testdata/usb/dslogic/manifest.json` pins their sha256 | 16 MB of binary traces; the plan's separate test-data repository is not set up yet |
| Phase 3a: demo random mode checked "structurally only" | Checked packet for packet | Both drivers run on one virtual clock and one random sequence (`dev/dsview-oracle/src/drv.rs`; `demo.c` is built with `g_timer_*`, `g_usleep`, `rand`, `srand` mapped onto it), so random and time-based output is deterministic |
| `compat/` with the shims | The shims sit next to the C code they replace (`libsigrok4DSL/ds_api_rs.c`, `hardware/rust/rs_driver.c`, `output/rs_output.c`, `libsigrokdecode4DSL/rs_decode.c`) | Each builds with its own C library; they are deleted with it in Phase 6 |
| `compat/impl_loader.c`: dlopen of per-subsystem `.so` files | One binary; `dsview-ffi/src/switch.rs` reads `DSVIEW_IMPL*` at run time | Same A/B power, no symbol-clash risk, no extra build artifacts |
| `dev/dsview-usb-fake` and `dev/dsview-siggen` crates | The fake libusb is `dev/dsview-oracle/csrc/fake_libusb.c`; the generators live in `decode-diff` | They have no other users |
| egui_dock, catppuccin-egui, egui_extras SVG icons in the GUI | Fixed panels; themes as hand-written `Visuals` and icons through resvg 0.44 (M6f) | Not in the offline registry in a version that fits egui 0.37; the docks need no tabbing |
| `rtrb` ingest queue, mmap spill, `loom` (§1.4) | **Ingest queue built in M7b B4** with `std::sync::mpsc` (packet granularity; `rtrb` gives nothing there): the collect thread hands packets and events to a per-session `ingest` thread, in order, and waits only once 256 MiB are queued (counted by `Session::ingest_waits`, at most 2 s, then logged). No mmap spill: a **memory guard** refuses a logic capture that does not fit into `MemAvailable` (less 256 MiB headroom, plus the buffer it replaces) with a message in the GUI and a `start_capture` error over the control API. No `loom` | DSView ingested on the USB thread, so a GUI frame or decoder holding the store lock stalled the transfers. A capture larger than RAM was OOM-killed before; the guard turns that into a clear refusal. A spill to disk remains a follow-on if large captures are needed |
| `masks.toml` for the masked USB bytes (Phase 3) | The masks are in the comparing code (F22) | One place, next to the comparison |
| R1: `switch.rs` defaults to Rust | The default is a build feature (`rust-default`) that only the CMake build of `DSView-qt` sets | A run-time default would also flip the oracle, which links `dsview-ffi`: every C-vs-Rust check would compare Rust with Rust |
| M7a: oracle buildable from a `c-final` worktree (`xtask oracle-src`), C-vs-Rust checks in a manual archive workflow | The oracle, `dsview-ffi` and every C-vs-Rust check were deleted with the C code; tag `c-final` holds them (check it out to compare again). The Rust decoder runner (`rust-decode`) moved to `dsview-testkit`; `xtask golden check` compares Rust alone with the stored C goldens | The user's choice (2026-10-19): archive by tag. A worktree build would keep Qt, glib and FFTW in the toolchain for a check that no longer gates anything |
| M7a: Windows installer files kept for the follow-on port | Deleted with the rest of the legacy tree (`installer/`, `cross-compile-windows/`, icons) | The user asked to remove all legacy code; the Windows port will need a new installer for the Rust binary anyway |
| R1 and M7a as separate steps (one A/B release with the Qt GUI, then the removal) | Done in one go: DSViewR 1.0.0 ships the Rust GUI only; the Qt GUI and its "C core" fallback never went out in a package | The user's decision (2026-10-19) |
| Product name DSView | DSViewR 1.0.0 (`DSViewR`, `dsviewr-cli`, package `dsviewr` replacing `dsview`); data in `share/dsview`; settings and profiles stay where DSView kept them | The user's decision (2026-10-19); internal crate names stay `dsview-*` |
| Windows and macOS (portable design, PLAN context) | Linux only after Phase 6; release R1 is the last Qt Windows build | Decision 2026-10-18; the ports are follow-on work |
| Packaging against the system Python | The deb and AppImage bundle libpython and the stdlib in `lib/dsview`; the Rust binaries find them through their RUNPATH and `dsview_decode::python` (no wrapper script); `DSView-qt` keeps the `PYTHONHOME` wrapper (deb) or AppRun branch (`--qt`) | Decision 2026-10-18: one package for every distribution release; layout built by `tools/package/bundle-python.sh` (M6g) |
| gnuplot export (Qt's export list) | Dropped; srzip is ported (M6g) | Decision 2026-10-18 |
| M6e: Rust GUI default | CMake option `DSVIEW_RUST_GUI` (on by default on Linux): the Rust GUI installs as `DSView`, the Qt GUI as `DSView-qt` with its own desktop entry, for one release; without cargo the Qt GUI stays `DSView` | Until R1, `build.yml` (deb/AppImage) had no Rust toolchain, so its packages stayed Qt; since R1 they carry the Rust GUI, `DSView-qt` and `dsview-cli`. Windows and macOS keep Qt (PLAN: Linux first) |
| M6e: AccessKit in the `dsview` binary | Widgets are labelled and checked by the kittest accessibility check, but eframe is built without its `accesskit` feature | It needs `accesskit_winit`, which is not in the offline cargo registry; added with the M6f audit |
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
| Phase 3: DSCope Rust default only after a hardware confirmation | R1 makes Rust the default for every subsystem, DSCope/E8 included, without one; the Qt desktop entry's action "DSView (Qt, C core)" runs `DSView-qt` on C | The user's decision for R1 (Rust everywhere). With `DSVIEW_IMPL_ACQ=rust` every device runs its Rust driver anyway, and the Rust GUI has used it since M6e. The release notes ask DSCope/E8 owners for `selftest` bundles |
| Phase 4: `compat/ds_api_rs.c`, `DeviceManager` | `libsigrok4DSL/ds_api_rs.c` over `dsview_session::Library` | Same place as the other shims; the library is lib_main.c's device manager and capture thread in one |
| Phase 4: CaptureSession state machine, DecodeScheduler | Moved to 4b (with the control server and `dsview-cli serve`) | They replace Qt's SigSession, which stays for the Qt GUI; 4a replaces what is under it |
| Phase 4: CaptureSession state machine with Arming/WaitTrigger/... states | `Session` with C's flags (`working`, the device's running state, `instant`, the collect mode) and start/end counters | The control API and the transcript depend on C's flags; separate states would only rename them |
| Phase 4: `dsview-control` on tokio, `ControlBackend` async | A thread per connection, a synchronous `ControlBackend` | The core crates use threads (§1.4); one connection waiting on a capture must not block the others, which threads give without a runtime |
| Phase 4: transcripts pass "against Qt and `dsview-cli serve`" | The transcript was re-recorded from the Qt GUI on the Rust core | The stored one had the decoder and row order of an earlier install of the decoder directory (F25); every response other than that order was unchanged |
| Phase 4: 1 h loop soak, RSS growth under 1 % | `dsview-cli soak`: 1 h loop mode on the demo's random pattern, 1 h repeat mode with the four protocol.demo decoders, 15 min loop mode on the DSLogic Plus in stream mode. The growth is the least-squares trend of RSS samples taken every 5 s after a 5-minute warm-up | First value against last value is not a usable measure here. A loop capture's ring holds one row of leaves more or less depending on its position (16 channels × 2 MiB: the RSS alternates between two levels 32 MiB apart). Repeat captures allocate and free per round (single samples between 70 and 170 MB, no trend) |
| Phase 4 gaps for Phase 5 | Not in the Rust session yet: profiles of old DSView versions (`<driver><mode>.ses<lang>.dsc`; the current ones are loaded and saved in the Qt GUI's profile directory), advanced/serial trigger, DSO trigger settings, zero calibration prompt, math/spectrum, cursor ranges for save/export | Only the Qt GUI uses them today; they arrive with the GUI that shows them |
| Logic header trigger squares as in Qt (small icons, header as wide as the DSO one) | 16 px squares at a 19 px pitch; the logic header is 190 px, other headers stay 150 px | User request 2026-10-27: easier to hit and to read |
