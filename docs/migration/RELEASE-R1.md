# DSViewR 1.0.0 (draft release notes)

*Draft for the first release (PLAN.md "Release R1", merged with M7a). The tag and the release
text are decided when `philipp` is merged into `master`.*

## DSView, rewritten in Rust

DSViewR is the logic analyzer and oscilloscope software for DreamSourceLab instruments, rewritten
in Rust:
- the GUI (egui), the drivers, the file formats, the sample stores and the decoder engine;
- the 150+ protocol decoders are sigrok's Python decoders, as in DSView.

It has the features of the Qt DSView it replaces (docs/migration/PARITY.md). Each part was compared
with the original C/C++ code while it was ported; the tag `c-final` keeps that code and the
comparison tools.

## New

- **`dsviewr-cli`**: DSViewR without a window:
  - `devices`, `info`, `capture` (.dsl/.csv/.vcd/.srzip), `export` and `decode`;
  - `verify-counter` (DSLogic test mode) and `selftest`;
  - `serve --mcp`: the control socket for scripts and AI agents (see `dsview-mcp`).
- **srzip export** (sigrok's format), readable by `sigrok-cli` and PulseView.
- **Accessibility**: the GUI exposes its controls to screen readers (AT-SPI).
- **Packages carry their own Python**: the .deb and the AppImage work on Ubuntu 22.04, 24.04 and
  26.04 without a matching system Python. The .deb replaces an installed `dsview` package.

## Changed

- The Qt application and the C code are gone; there is no "C core" fallback any more.
- **gnuplot export** is gone; use CSV.
- Decoder options that run a program or read a file (`objdump`, `elffile`, `mapping`) are not
  taken from opened `.dsl` files without asking.
- A missing FPGA bitstream is reported as a missing firmware file, not as a USB error.
- Settings are stored in `dsview-rs.json`; the Qt `DSView.conf` is imported once on the first
  start. The log file is `~/DSViewR.log`.
- Help → Bug Report and Update open github.com/phil16021/DSView_rust.
- The full list of intentional differences from DSView, and of the defects found in its C code,
  is in docs/migration/DEVIATIONS.md.

## Owners of a DSCope or a Hermelin MSO-E8

The developers have no such device. Their Rust drivers were checked against the C drivers on a
simulated device, byte for byte on the USB, but not on real hardware. Please run
`dsviewr-cli selftest --out DIR` and attach DIR to an issue, whether the result is good or bad.

## Platforms

Linux (x86-64) only. Windows and macOS builds come back once the Rust application is ported
there (docs/migration/PLAN.md, follow-on work).
