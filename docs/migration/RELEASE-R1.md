# Release R1 (draft release notes)

*Draft for the A/B release of PLAN.md "Release R1". The tag name and the release text are decided
when `philipp` is merged into `master`.*

## DSView is now written in Rust

On Linux, `DSView` is the new Rust application: the same views, docks, dialogs, decoders and
devices as before (docs/migration/PARITY.md lists all 52 features), drawn with egui.

The Qt application stays for this one release as **`DSView-qt`** (menu entry "DSView (Qt)"). It
now runs on the Rust core as well: file formats, sample stores, the decoder engine, the drivers
and the acquisition library. Its right-click menu entry "DSView (Qt, C core)" starts it on the
original C code (`DSVIEW_IMPL=c DSView-qt`). If something works in the C core but not in the Rust
one, please report it with the steps and, for hardware, a `dsview-cli selftest` bundle.

The next release removes the Qt application and the C code.

## New

- **`dsview-cli`**: DSView without a window. `devices`, `info`, `capture` (.dsl/.csv/.vcd/.srzip),
  `export`, `decode`, `verify-counter` (DSLogic test mode), `selftest`, and `serve --mcp` (the
  control socket for scripts and AI agents, see `dsview-mcp`).
- **srzip export** (sigrok's format), readable by `sigrok-cli` and PulseView.
- **Accessibility**: the Rust GUI exposes its controls to screen readers (AT-SPI).
- **Packages carry their own Python**: the .deb and the AppImage work on Ubuntu 22.04, 24.04 and
  26.04 without a matching system Python.
- The AppImage starts the Rust GUI; `DSView-x86_64.AppImage --qt` starts the Qt one.

## Changed

- **gnuplot export** is gone; use CSV.
- Decoder options that run a program or read a file (`objdump`, `elffile`, `mapping`) are not
  taken from opened `.dsl` files without asking.
- A missing FPGA bitstream is reported as a missing firmware file, not as a USB error.
- Settings are stored in `dsview-rs.json`; the Qt `DSView.conf` is imported once on the first
  start.
- The full list of intentional differences from the C code, and the C defects found on the way,
  is in docs/migration/DEVIATIONS.md.

## Owners of a DSCope or a Hermelin MSO-E8

The developers have no such device. Their Rust drivers are checked against the C drivers on a
simulated device, byte for byte on the USB, but not on real hardware. Please run
`dsview-cli selftest --out DIR` and attach DIR to an issue, whether the result is good or bad.
Until the C code is removed, "DSView (Qt, C core)" is the fallback.

## Windows and macOS

This is the last Windows build (the Qt application as before). Later releases are Linux-only until
the Rust application is ported (docs/migration/PLAN.md, follow-on work).
