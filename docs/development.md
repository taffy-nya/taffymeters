# Development

## Requirements

- Rust 1.92 or later with the `x86_64-pc-windows-msvc` toolchain (CI uses stable and checks 1.92)
- Visual Studio Build Tools with MSVC and the Windows SDK (`rc.exe` embeds the icon and version)
- Python 3.11+ for `tools/`
- For packaging only: `cargo install cargo-about --locked --features cli`

## Checks

The same commands as CI:

```text
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --features automation -- -D warnings
cargo test --locked --features automation
```

`cargo test` runs the unit tests, the headless `core` suite (`tests/automation_core.rs`: synthetic audio, simulated windows and input, no audio device or desktop needed) and the crash-log test (`tests/automation_crash.rs`).

When running the app by hand, pass `--config-dir <new directory>` so it doesn't touch your settings in `%APPDATA%\taffymeters`. Only one instance runs per config directory.

## Automation

The non-default `automation` feature adds a test harness and a CLI to the same executable:

```text
cargo build --release --locked --features automation --target-dir target/automation
taffymeters.exe --automation run --suite core|dataflow-perf --output <new dir>
taffymeters.exe --automation run --suite visual --output <new dir> [--baseline <dir>]
taffymeters.exe --automation compare --expected <png> --actual <png> --output <new dir>
taffymeters.exe --automation preview --input synthetic|sweep|system [--trace <new file>] [--config-dir <dir>]
taffymeters.exe --automation crash --config-dir <new dir>
```

`visual` renders offscreen with DX12 and compares against a baseline you supply. `preview` opens real windows; `--trace` writes JSON lines with state, per-window control rectangles and rates.

## Native checks

```text
python tools/check.py [--headless | --native] [--only snap,lock] [--audio] [--ci] [--out <empty dir>]
```

Without a selector it runs the headless tests, then the native groups in `tools/native/checks/` against a debug automation build. Native checks open real windows and take over the mouse and keyboard after a five-second warning; `--audio` also plays a test tone. Results and screenshots of failures go to `target/checks/<time>/`.

## Performance measurement

The non-default `perf-trace` feature adds bounded timing hooks (audio callback, analysis worker, UI pass) to the normal product; `--perf-trace <dir>` writes `<dir>/perf.json` on exit. Timestamps are raw `QueryPerformanceCounter` ticks.

```text
cargo build --release --locked --features perf-trace --target-dir target/perf-trace
python tools/perf/run.py --output <new dir> --build new=new:<perf-trace exe> [--build new-off=new-off:<plain exe>] \
    [--scenarios single,shared,different,lifecycle] [--rounds 5]
python tools/perf/summarize.py <run dir>
python tools/perf/churn.py --exe <automation exe> --output <new dir>
```

`run.py` plays a looping 440/880 Hz tone on the default playback device for the whole session (the app captures it through loopback); scenarios marked `stimulus: music` (`daily-bar`, `daily-heavy`) instead restart a music excerpt at every run, decoded with `ffmpeg`, which must be on PATH (see `tools/perf/assets/README.md`). Each run opens its scenario from a fresh `--config-dir`, and the tool moves, resizes, minimizes and closes the windows itself; keep the desktop otherwise idle and the system volume fixed, since loopback may capture the signal after the volume control.

Prefer headless and offscreen checks while developing. For a performance change, screen it with the two or three scenarios it affects at `--rounds 3`, then confirm a change worth keeping with five rounds: three pairs can point the wrong way. `--build old=old:<exe>` also drives the pre-rewrite probe used as the comparison baseline. `churn.py` repeatedly splits, closes and opens windows through the settings window with real input and checks that live objects return to their starting counts.

## Releasing

1. Set the version in `Cargo.toml`, move the `[Unreleased]` notes in `CHANGELOG.md` into a dated section for it, and commit.
2. On `main`, run the Release workflow manually (`workflow_dispatch`): it builds and uploads the package as an artifact without publishing. Download it and check that it runs.
3. Tag and push: `git tag vX.Y.Z && git push origin vX.Y.Z`. The workflow builds the package again and creates a draft release with the CHANGELOG section as notes. Review it and publish.

`python tools/package.py [--tag vX.Y.Z] [--out <empty dir>]` does the packaging locally, by default into `target/dist/`: release build with a statically linked C runtime, version check of the exe, `THIRD_PARTY_LICENSES.txt` from cargo-about (`tools/about.toml`, `tools/about.hbs`), zip and `release-notes.md`.

## Conventions

- Commit messages: [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `perf:`, `refactor:`, `style:`, `docs:`, `test:`, `build:`, `ci:`, `chore:`), a one-line summary in the imperative, details in the body if needed. `refactor:` is only for restructuring that keeps behavior; new, changed or removed behavior is `feat:` or `fix:`, and automation and test tooling is `test:`.
- Formatting is plain `cargo fmt`; extra lints are in `Cargo.toml`. Comments explain only what the code can't: reasons, invariants, units, platform quirks, and `// SAFETY:` before every `unsafe` block.
