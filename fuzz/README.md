# Fuzzing (cargo-fuzz, nightly)

```sh
cd fuzz
cargo +nightly fuzz run header corpus/header seeds/header -- -max_total_time=60
cargo +nightly fuzz run dsl_reader corpus/dsl_reader seeds/dsl_reader -- -max_total_time=60 -rss_limit_mb=2048
```

`seeds/` holds the committed starting inputs (the demo captures and their headers); `corpus/` is
what libFuzzer grows locally and is not committed. Minimize any crash into a regression test in
`crates/dsview-formats`.
