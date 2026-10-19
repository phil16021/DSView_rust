# DSViewR

DSViewR is logic analyzer and oscilloscope software for
[DreamSourceLab](https://www.dreamsourcelab.com) instruments (DSLogic, DSCope, and the Hermelin
MSO-E8). It is a rewrite of [DSView](https://github.com/DreamSourceLab/DSView) in Rust. The GUI,
drivers, file formats, sample stores and decoder engine are all Rust; the more than 150 protocol
decoders are the sigrok Python decoders DSView ships.

![Logic capture with I²C, UART, SPI and CAN-FD decoders](docs/screenshots/logic-decoders.png)

| Protocol table | Oscilloscope |
|---|---|
| ![Decoder dock with the protocol table](docs/screenshots/protocol-table.png) | ![DSO view of the demo oscilloscope](docs/screenshots/oscilloscope.png) |

## Features

- **Logic analyzer:** buffer and stream capture at every rate of the DSLogic channel modes;
  simple, advanced (multi-stage) and serial triggers; cursors, measurements and edge search.
- **Protocol decoders:** 150+ decoders (I²C, SPI, UART, CAN/CAN-FD, I3C, SMBus/PMBus, JTAG,
  SENT, DShot, …), stacked decoders, a protocol table with search and export.
- **Oscilloscope and data acquisition:** V/div and timebase controls, trigger settings,
  measurements, math channels, FFT, Lissajous view, persistence, reference waveforms,
  histograms, channel-to-channel measurements, and a demo oscilloscope for use without hardware.
- **Files:** opens and saves DSView's `.dsl` captures and `.dsc` profiles; exports CSV, VCD and
  srzip (readable by sigrok-cli and PulseView).
- **`dsviewr-cli`:** captures, exports and decodes from the command line, and runs DSViewR
  headless behind its control socket (`serve --mcp`).
- **Automation:** a JSON-RPC control socket (`DSViewR --mcp`) and an MCP server
  ([`dsview-mcp`](dsview-mcp/README.md)), so scripts and AI agents can drive captures and read
  the results.
- **Interface:** dark, light, Catppuccin Latte and Frappé themes; English, German and Chinese;
  keyboard shortcuts; accessible to screen readers (AT-SPI).

## Install

Linux, x86-64. Packages are on the [releases page](https://github.com/phil16021/DSView_rust/releases):

- **Debian/Ubuntu (`dsviewr_*.deb`):** `sudo apt install ./dsviewr_*.deb`. It replaces an
  installed `dsview` package, carries its own Python, and installs the udev rule that gives
  users access to the instruments.
- **AppImage (`DSViewR-x86_64.AppImage`):** `chmod +x` it and run it. For USB access the udev
  rule has to be installed once; DSViewR shows the command for it on its first start.

Tested on Ubuntu 22.04, 24.04 and 26.04.

## First steps

Start `DSViewR`. Without an instrument, choose *Demo Logic* or *Demo Oscilloscope* in the
device list and press **Start**. *File → Open* opens `.dsl` captures, for example the demo in
`/usr/share/dsview/demo/logic/protocol.demo`, which comes with I²C, UART, SPI and CAN-FD decoders.

## dsviewr-cli

```sh
dsviewr-cli devices                                   # instruments and demo devices
dsviewr-cli capture --device dslogic --samplerate 10000000 --samples 1000000 --out cap.dsl
dsviewr-cli export cap.dsl cap.csv                    # .csv, .vcd or .srzip
dsviewr-cli decode cap.dsl --decoder i2c:scl=0,sda=1  # one line per annotation
dsviewr-cli verify-counter --device dslogic           # DSLogic internal test mode
dsviewr-cli selftest --out report/                    # a report bundle for bug reports
dsviewr-cli serve --mcp                               # headless, behind the control socket
```

`dsviewr-cli --help` lists every option.

## Building from source

Requirements: Rust (the version in `rust-toolchain.toml`, installed by rustup) and the Python
development files (`python3-dev`).

```sh
cargo build --release -p dsview-gui -p dsview-cli
target/release/dsview        # DSViewR; finds its data in assets/
target/release/dsview-cli    # dsviewr-cli
```

The packages are built by `.github/workflows/build.yml` with `cargo deb` and the scripts in
`tools/package/`. Tests: `cargo nextest run --workspace` and `cargo xtask golden check`.

## Project status

DSViewR 1.0 has the features of the Qt DSView it replaces, as listed in
[docs/migration/PARITY.md](docs/migration/PARITY.md) (gnuplot export is gone; srzip is new). It was ported subsystem by subsystem and
compared with the original C/C++ code at each step: file formats, sample stores and decoder
output byte for byte, and USB traffic on a DSLogic Plus. The tag `c-final` holds the last tree
with that C code and the comparison tools.

- How the port was done, and what is left: [docs/migration/PLAN.md](docs/migration/PLAN.md),
  [docs/migration/STATUS.md](docs/migration/STATUS.md).
- Where DSViewR intentionally behaves differently, and the defects of the original that were
  found: [docs/migration/DEVIATIONS.md](docs/migration/DEVIATIONS.md).
- **DSCope and MSO-E8 owners:** the drivers are checked against the C drivers on a simulated
  device but not yet on real hardware. Please run `dsviewr-cli selftest --out DIR` and attach
  the directory to an issue, whether it works or not.

Bugs and requests: [issues](https://github.com/phil16021/DSView_rust/issues).

## Credits

- [DreamSourceLab](https://www.dreamsourcelab.com), for DSView and the instruments.
- The [sigrok project](https://sigrok.org), on which DSView is based, and the authors of the
  protocol decoders.
- [Schildkroet's DSView fork](https://github.com/Schildkroet/DSView), the starting point of this
  rewrite.

## License

GNU General Public License, version 3 or later ([COPYING](COPYING)). The protocol decoders,
firmware and FPGA bitstreams in `assets/` keep the licenses stated in their files (see
`assets/res/license.txt`).
