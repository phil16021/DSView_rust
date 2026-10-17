# GUI parity checklist (frozen 2026-10-17)

The Rust GUI (Phase 5) becomes the default at milestone M6e. Qt is removed once every item below is
checked. The list was taken from the Qt GUI as it is today (`DSView/pv`, about 65k lines). New features
are not added until M7.

Milestones: **a** viewer, **b** logic capture, **c** decoders, **d** DSO/analog, **e** docks and dialogs,
**f** polish.

## Modes and capture
- [ ] (b) Logic, DSO and analog (DAQ) work modes; mode selector
- [ ] (b) Single, repeat (with interval), loop and instant collect modes
- [ ] (b) Hardware buffer vs stream mode
- [ ] (b) Device sources: hardware, demo, file; hot-plug with a "switch device?" prompt and auto-save
- [ ] (b) Sampling bar: device list, options, sample count/duration, sample rate, start/stop, instant
- [ ] (d) Auto calibration entry

## Views
- [ ] (a) Logic traces: edge rendering, sample dots when zoomed in, line width, channel dividers, Y-scale badge, auto-scroll while streaming
- [ ] (a) Ruler in time or sample units; header with rename, colour picker, drag reorder, context menu
- [ ] (a) Zoom/scroll: wheel (configurable), PgUp/PgDn, inertial drag, pinch, `show_region` fit/fixed
- [ ] (a) Numbered time cursors (add, delete, drag, labels), trigger cursor, search cursor; delta and measurement readouts
- [ ] (c) Decode traces: multiple rows, row show/hide, colours, best-fit text, adaptive font width, "zoom in for details", progress, errors, start/end cursor range
- [ ] (d) DSO: envelope and polyline, persistence/DPO fade, split layout, V/div and timebase dials, AC/DC, probe ×1/×10/×100, zero-offset drag, trigger level/position drag, auto-set, out-of-range warning, hover value, X/Y cursors
- [ ] (d) Analog: envelope or polyline, value mapping
- [ ] (d) Spectrum/FFT: windows (rect, Hann, Hamming, Blackman, flat-top), length, interval, DC ignore, dBV or linear, range
- [ ] (d) Math trace (11 operations, filter window), lissajous (X/Y, size), reference waveforms

## Docks
- [ ] (c) Protocol: searchable decoder picker; decoder list (format, settings, delete); annotation table (virtualised, 100k+ rows); search with previous/next and match count; click to jump; export CSV/TXT
- [ ] (b) Trigger, simple
- [ ] (e) Trigger, advanced: 16 stages (X/0/1/R/F/C, and/or, invert, counter, contiguous) and serial trigger; trigger position
- [ ] (d) DSO trigger: source, edge, position, hold-off, sensitivity, auto
- [ ] (e) Measure: mouse measurement, cursor distances, per-channel width/period/frequency/duty, edge counts
- [ ] (e) Search (bottom panel): per-channel pattern, previous/next

## Toolbars and dialogs
- [ ] (e) Trig bar: trigger/decode/measure/search toggles, function menu (FFT, math, lissajous, ch-ch, histogram, reference), display menu
- [ ] (a/e) File bar: open/save session, load/store/default profile, export, screenshot
- [ ] (f) Logo bar: about, manual, bug report, update, language, log options
- [ ] (e) Device options, generated at runtime from the device's option list; channel enable and layout
- [ ] (c) Decoder options, generated at runtime; channel binding; start/end cursor; stacked decoders
- [ ] (e) Calibration (VGAIN, VOFF, VCOMB; save, reset, reload)
- [ ] (d) FFT, math and lissajous options; DSO measurements; histogram/jitter; channel-to-channel delay/skew/phase
- [ ] (e) Reference, interval, region and search options; store progress (cursor range, compressed or original); protocol export and list
- [ ] (f) Application options: antialiasing, quick scroll, wheel action, font size, ruler units, line widths, persistence time, decoder font widths, profile in bar, trigger position in middle, don't ask to save
- [ ] (f) About, message box, waiting dialog

## Cross-cutting
- [ ] (f) Shortcuts: S, I, T, D, M, R, O, PgUp/PgDn, ←/→ zoom, 0/1 select DSO channel, ↑/↓ V/div, Esc; 200 ms debounce
- [ ] (f) Settings (app options, window state, user history, fonts, per-decoder format); first-run import of the Qt `DSView.conf`
- [ ] (a/f) `.dsl` open/save; `.dsc` profiles, including automatic `<driver><mode>.ses.dsc` and the shipped defaults
- [ ] (f) i18n: cn/en/de from `lang/*.json`, runtime switching, CJK font
- [ ] (f) Themes: dark, light, frappé, latte; about 250 SVG icons
- [ ] (f) Help: user guide PDFs, bug-report and release URLs, open log file
- [ ] (a) Control API (`--mcp`) hosted natively: the 31 methods and `testdata/control/demo-scenario.json` pass
- [ ] (f) Command line: `-l`, `-v`, `-s`, `--mcp`, file to open
- [ ] (f) Accessibility: AccessKit audit
