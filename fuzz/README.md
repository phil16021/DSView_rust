# Fuzzing (cargo-fuzz, nightly)

```sh
cd fuzz
cargo +nightly fuzz list
cargo +nightly fuzz run header corpus/header seeds/header -- -max_total_time=60
cargo +nightly fuzz run dsl_reader corpus/dsl_reader seeds/dsl_reader -- -max_total_time=60 -rss_limit_mb=2048
cargo +nightly fuzz run stores corpus/stores seeds/stores -- -max_total_time=60 -rss_limit_mb=4096
```

The targets:

| Target | What it feeds |
|---|---|
| `header`, `dsl_reader` | `.dsl` header text and whole archives (`crates/dsview-formats`) |
| `stores` | the logic/DSO/analog stores with arbitrary packets and queries, including the packet shapes the C++ classes mishandle (DEVIATIONS.md F9, F10) |
| `usb_header` | the DSLogic driver on a simulated device whose bulk IN data (trigger header, samples) comes from the fuzzer |
| `dso_status_block` | the same for the DSCope: DSO frames and the status block behind each |
| `pcapng_reader` | usbmon pcapng traces (`dsview_usb::trace`) |
| `jsonrpc_request` | the control socket's request handling (`dsview_control::server::handle_line`) |
| `srzip` | the srzip writer: any channels and packets must reopen as the same chunks |
| `dsc_json`, `decoders_json` | profiles and decoder entries loaded into one demo session with every decoder |
| `demo` | the demo driver under any sequence of settings, then a capture |

The JSON targets start Python once and keep it, with their session: run them with
`-detect_leaks=0` (CI does this for all targets).

`seeds/` holds the committed starting inputs (the demo captures and their headers); `corpus/` is
what libFuzzer grows locally and is not committed. Minimize any crash into a regression test next
to the code it found (for example `dsview_usb::trace::tests`).
