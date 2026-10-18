# GUI parity checklist (frozen 2026-10-17)

The Rust GUI (Phase 5) becomes the default at milestone M6e. Qt is removed once every item below is
checked. The list was taken from the Qt GUI as it is today (`DSView/pv`, about 65k lines). New features
are not added until M7.

Milestones: **a** viewer, **b** logic capture, **c** decoders, **d** DSO/analog, **e** docks and dialogs,
**f** polish.

## Modes and capture
- [x] (b) Logic, DSO and analog (DAQ) work modes; mode selector
- [x] (b) Single, repeat (with interval), loop and instant collect modes
- [x] (b) Hardware buffer vs stream mode
- [x] (b) Device sources: hardware, demo, file; hot-plug with a "switch device?" prompt and auto-save
- [x] (b) Sampling bar: device list, options, sample count/duration, sample rate, start/stop, instant
- [x] (d→e) Auto calibration entry (with the calibration dialog in M6e)

## Views
- [x] (a) Logic traces: edge rendering, sample dots when zoomed in, line width, channel dividers, Y-scale badge, auto-scroll while streaming
- [x] (a) Ruler in time or sample units; header with rename, colour picker, drag reorder, context menu (Qt's header context menu is empty: nothing to port)
- [x] (a) Zoom/scroll: wheel (configurable), PgUp/PgDn, inertial drag, pinch, `show_region` fit/fixed
- [x] (a) Numbered time cursors (add, delete, drag, labels), trigger cursor, search cursor; delta and measurement readouts
- [x] (c) Decode traces: multiple rows, row show/hide, colours, best-fit text, adaptive font width, "zoom in for details", progress, errors, start/end cursor range
- [x] (d) DSO: envelope and polyline, persistence/DPO fade, split layout, V/div and timebase dials, AC/DC, probe ×1/×10/×100, zero-offset drag, trigger level/position drag, auto-set, out-of-range warning, hover value, X/Y cursors
- [x] (d) Analog: envelope or polyline, value mapping
- [x] (d) Spectrum/FFT: windows (rect, Hann, Hamming, Blackman, flat-top), length, interval, DC ignore, dBV or linear, range
- [x] (d) Math trace (11 operations, filter window), lissajous (X/Y, size), reference waveforms

## Docks
- [x] (c) Protocol: searchable decoder picker; decoder list (format, settings, delete); annotation table (virtualised, 100k+ rows); search with previous/next and match count; click to jump; export CSV/TXT
- [x] (b) Trigger, simple
- [x] (e) Trigger, advanced: 16 stages (X/0/1/R/F/C, and/or, invert, counter, contiguous) and serial trigger; trigger position
- [x] (d) DSO trigger: source, edge, position, hold-off, sensitivity, auto
- [x] (e) Measure: mouse measurement, cursor distances, per-channel width/period/frequency/duty, edge counts
- [x] (e) Search (bottom panel): per-channel pattern, previous/next

## Toolbars and dialogs
- [x] (e) Trig bar: trigger/decode/measure/search toggles, function menu (FFT, math, lissajous, ch-ch, histogram, reference), display menu (themes and options since M6f)
- [x] (a/e) File bar: open/save session, load/store/default profile, export, screenshot (export: CSV and VCD; srzip follows in M6g, gnuplot is dropped)
- [x] (f) Logo bar: about, manual, bug report, update, language, log options
- [x] (e) Device options, generated at runtime from the device's option list; channel enable and layout
- [x] (c) Decoder options, generated at runtime; channel binding; start/end cursor; stacked decoders
- [x] (e) Calibration (VGAIN, VOFF, VCOMB; save, reset, reload)
- [x] (d) FFT, math and lissajous options; DSO measurements; histogram/jitter; channel-to-channel delay/skew/phase
- [x] (e) Reference, interval, region and search options; store progress (cursor range, compressed or original); protocol export and list (Qt's region dialog is never opened: not ported, DEVIATIONS F29)
- [x] (f) Application options: antialiasing, quick scroll, wheel action, font size, ruler units, line widths, persistence time, decoder font widths, profile in bar, trigger position in middle, don't ask to save (decoder font width as a smaller font, F29)
- [x] (f) About, message box, waiting dialog (the waiting dialog's GIF as animated dots; the logo with the icons, step 7)

## Cross-cutting
- [x] (f) Shortcuts: S, I, T, D, M, R, O, PgUp/PgDn, ←/→ zoom, 0/1 select DSO channel, ↑/↓ V/div, Esc; 200 ms debounce
- [x] (f) Settings (app options, window state, user history, fonts, per-decoder format); first-run import of the Qt `DSView.conf`
- [x] (a/f) `.dsl` open/save; `.dsc` profiles, including automatic `<driver><mode>.ses.dsc` and the shipped defaults
- [x] (f) i18n: cn/en/de from `lang/*.json`, runtime switching, CJK font (the system's; a shipped font with the packaging, Phase 6)
- [x] (f) Themes: dark, light, frappé, latte; about 250 SVG icons (the 27 icons on bars and menus, both sets; Qt's widget-decoration SVGs have no use in egui)
- [x] (f) Help: user guide PDFs, bug-report and release URLs, open log file
- [x] (a) Control API (`--mcp`) hosted natively: the 31 methods and `testdata/control/demo-scenario.json` pass
- [ ] (f) Command line: `-l`, `-v`, `-s`, `--mcp`, file to open
- [ ] (f) Accessibility: AccessKit audit

## Added 2026-10-18 (missed by the first inventory)
These are existing Qt features found in the Phase 5 analysis, not new features.
- [x] (a) Right-drag rubber-band zoom; right double-click toggles full view / previous view; left double-click adds a cursor snapped to an edge
- [x] (a) Click measurements: edge jump (time/sample delta to the nearest edge, drawn as a curve) and edge counting from the click to the hover point; mouse-measure popup (width, period, frequency, duty)
- [x] (a) Vertical zoom of trace heights (Ctrl+Shift+wheel, 0.5-20×) with the Y-scale badge (click resets); wheel/Ctrl+wheel swap option; horizontal wheel pans
- [x] (a) Ruler: cursor selector strip (left click: add or move cursor N), go-to-cursor strip (right click), hover arrow, hover snapping to minor ticks
- [x] (a) Header: trigger squares per logic channel, multi-select (Ctrl) and 10 px-grid drag, wheel over a DSO header steps its dial
- [x] (b) Trigger-wait animation and progress while waiting for the hardware trigger
- [x] (c) Decode region markers (start/end), search-hit marker, type-2xx annotations marking the bound logic channels, "Unshown" row for a hidden decoder
- [x] (c) Per-decoder number format (hex/dec/oct/bin/ascii) kept per decoder id; protocol list viewer (choose stack, map zoom)
- [x] (d) Deep DSO frames (≥ 256 samples per pixel) drawn from the envelope; per-column min/max bars between 2 and 256 samples per pixel; trigger sub-sample offset
- [x] (d) X/Y cursor measurements (XM: width/period/frequency/duty from three clicks; YM: voltage delta — unreachable in Qt, not ported, DEVIATIONS F28)
- [x] (e) Measure dock: cursor list with go-to/delete and per-mode rows; search dock with pattern dialog and cancel
- [x] (f) Dock layout kept per work mode; help-documents dialog and driver hint at start-up; log options (save to file, append, level)
