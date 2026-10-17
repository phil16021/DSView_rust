# Fuzzing (cargo-fuzz, nightly)

```sh
cd fuzz
cargo +nightly fuzz run header corpus/header seeds/header -- -max_total_time=60
cargo +nightly fuzz run dsl_reader corpus/dsl_reader seeds/dsl_reader -- -max_total_time=60 -rss_limit_mb=2048
cargo +nightly fuzz run stores corpus/stores seeds/stores -- -max_total_time=60 -rss_limit_mb=4096
```

`stores` drives the logic/DSO/analog stores (`crates/dsview-store`) with arbitrary packets and queries,
including the packet shapes the C++ classes mishandle (DEVIATIONS.md F9, F10).

`seeds/` holds the committed starting inputs (the demo captures and their headers); `corpus/` is
what libFuzzer grows locally and is not committed. Minimize any crash into a regression test in
`crates/dsview-formats` or `crates/dsview-store`.
