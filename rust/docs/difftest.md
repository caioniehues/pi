# Differential tests

The Rust `pi` is checked against the TypeScript `pi` at the commit in `rust/tools/pi-difftest/TS_PIN`: scripted
RPC scenarios run against a fake provider (`rust/tools/pi-fake-provider`), and their normalized output (stdout
records, session files, provider requests, the working directory, exit status) must equal the goldens.

```bash
cd rust
cargo run -p pi-difftest -- compare --all --pi target/release/pi
```

Everything else (the scenario format, the normalizer rules, regenerating goldens, adding a scenario) is in
[`rust/tools/pi-difftest/README.md`](../tools/pi-difftest/README.md).
