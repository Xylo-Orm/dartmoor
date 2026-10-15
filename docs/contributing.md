# Development

Use [rust-toolchain.toml](../rust-toolchain.toml), retain the lockfile unless changing dependencies, and install the [native libraries](../README.md#build-and-run).

## Checks

On Linux or a Bash shell:

```sh
bash scripts/check.sh
```

The script stops on failure and uses RTK when installed. It runs application formatting, strict all-target Clippy, all-target tests, doc tests, vendored scap library tests, and release builds of binaries/examples. Linux also checks vendor formatting and strict lint. Local TCP/UDP mock tests need loopback socket permission; avoid simultaneous test runs that contend for WLED port 4048.

Zigbee driver tests run only when `target/zigbee-runtime/bin/python` exists or `DARTMOOR_ZIGBEE_PYTHON` names an executable interpreter. A missing interpreter is explicitly skipped. To prepare and run them without opening a radio:

```sh
python3 -m venv target/zigbee-runtime
target/zigbee-runtime/bin/python -m pip install -r src/zigbee/requirements.txt
target/zigbee-runtime/bin/python src/zigbee/test_helper.py
```

On Windows, use the environment's `Scripts/python.exe` and set `DARTMOOR_ZIGBEE_PYTHON` when using the Bash check script. The script itself does not install dependencies.

[CI](../.github/workflows/ci.yml) runs native Ubuntu, Windows and macOS jobs: application formatting/lint/tests, Python driver tests, scap library tests and a release application build. Vendor formatting/lint run on Linux. CI builds do not test live capture, keyring prompts, USB radios or physical lights.

## Deterministic regression probes

For changes to Video, compare replay output before and after the change:

```sh
cargo run --locked --quiet --example video_replay > /tmp/video-before.csv
# Make the change, then replay the same four scenes / 480 frames.
cargo run --locked --quiet --example video_replay > /tmp/video-after.csv
cmp /tmp/video-before.csv /tmp/video-after.csv
```

This checks those synthetic scenes, not perceptual quality on hardware. See [music replay](music.md#offline-replay) for audio fixtures. Targeted music checks:

```sh
cargo test --locked music
cargo test --locked --bin music_replay
cargo test --locked --release music_analysis_benchmark -- --ignored --nocapture
```

Sampling performance probes exclude capture, UI, network and devices:

```sh
cargo run --locked --release -- --benchmark
cargo run --locked --release --example sampling_stress -- broad
```

Stress modes: `ordinary`, `typical`, `broad`, `narrow`, `bulbs`, `adversarial`. Treat performance numbers as workload- and host-specific.

## Runtime probes

After `cargo build --locked --release`, these commands use mock light output:

```sh
target/release/dartmoor --demo-seconds 10
target/release/dartmoor --music-demo-seconds 10
target/release/dartmoor --capture-probe 10
target/release/dartmoor --music-probe 10
target/release/dartmoor --ui-smoke-seconds 12 --ui-smoke-workspace video
```

`--capture-probe` opens real capture permissions and needs source selection. `--music-probe` captures Linux system playback. UI smoke opens, minimizes/restores and closes a native window; add `--real-capture` for a real source. Desktop probes need a graphical session. Inspect errors and nonzero frame counts; process startup alone does not prove capture works.

For manual validation, check source permission/cancellation, live edits, Stop/restart, save/import, and the intended physical device. Test WLED mapping and finite-lease release, HA shared cadence, or Zigbee pairing/transitions as applicable. Record platform, revision, commands and scope in the PR or CI artifact; do not treat old results as current verification.

## Utilities and documentation

- `scripts/measure.py COMMAND [ARGS...]` samples Linux `/proc`, reports startup/cleanup-inclusive process metrics and propagates the child exit code.
- `scripts/render_workspace_review.py DIRECTORY` uses Pillow to render exported egui meshes to PNG without opening a native window.

Keep user behavior in the corresponding guide and implementation ownership in [architecture](architecture.md). Prefer reproducible checks and CI artifacts over committed one-off logs, screenshots, scored reviews or completed implementation plans. Preserve upstream vendor documentation and licenses.
