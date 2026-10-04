# Scoped CI repair: independent architecture review

The initial assessment is preserved below; see the final corrective-pass disposition at the end.

Reviewed 2026-10-05 against `13a76818c65607ffa5e5703c4c8b10080eb79554`. This reviewer authored no production changes. Scope: seven explicit stroke-width types, repository toolchain selection, native workflow activation, and supporting documentation/evidence.

**Accept the scoped repair at 9.1/10, with no critical, high or medium finding.** The source fix and local verification are complete; confirming the published repair still requires a fresh native matrix. This score assesses this small repair only. It does not replace the previous overall implementation acceptance of **8.6/10**, or imply new Windows/macOS success.

| Rubric | Weight | Score | Basis |
|---|---:|---:|---|
| Correctness/completeness | 30% | 9.5 | Every reported fatal site is fixed directly; strict warnings remain enabled. |
| Architecture | 20% | 9.5 | One checked-in compiler/component selection controls normal local and CI commands. |
| Resilience/concurrency | 20% | 9.0 | Removes moving compiler drift; no runtime or concurrency changes. |
| Verification/platform | 20% | 8.0 | Independent local checks pass; fresh native runner results are pending. |
| UX/efficiency | 10% | 9.5 | Rendering values unchanged; setup instructions identify the tested compiler honestly. |

Weighted score: **9.10/10**.

## Findings and improvements

1. **Low, pre-existing CI coverage gap:** `.github/workflows/ci.yml:23–27` runs application formatting/linting and vendored library tests, but does not run standalone vendored formatting/Clippy. Application dependency checking does not enforce that crate's lint policy. `docs/VERIFICATION.md:13–14` correctly records the stronger local checks, and historical vendor lint failures show that the distinction matters. Consider adding at least a Linux job for these existing standalone commands. This gap is not introduced by the repair and does not block fixing the seven compiler errors.

No other actionable defect was found within the reviewed scope. Maintain the pin through deliberate, verified upgrades; a version pin is not proof against runner-image, external-download or platform changes.

## Correctness and toolchain assessment

- The failure JSON records formatting success, Clippy failure, and skipped tests/releases on all three hosts. The fatal diagnostics identify exactly `src/app.rs:421` and `src/app/panels.rs:513,517,554,573,584,636`. The repair adds `_f32` at all seven sites, without lint allowances or changed numbers.
- The locked `epaint 0.33.3` implementation accepts `width: impl Into<f32>` and stores an `f32`. These literals were already resolving through `f32` fallback. Explicit `1.0_f32`, `2.0_f32` and `2.5_f32` retain exactly representable values, so this change does not alter stroke geometry or colors. No dependency, lockfile, capture, output or lifecycle code changed.
- `rust-toolchain.toml:1–4` pins the complete version while retaining the native host and installing the minimal profile plus both required components. `.github/workflows/ci.yml:12–18` executes in the checkout before caching/checks. The commands are valid in both shell families used by the matrix. Local `rustup show` identified this file as the active override; `rustc --version --verbose` reported `1.99.0`, commit `b940084d7`.
- The [rustup override documentation](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file) supports the file/component/host behavior. Automatic installation is enabled by default after the [1.28.1 restoration](https://github.com/rust-lang/rustup/blob/main/CHANGELOG.md#1281---2025-03-05). Current official [Ubuntu](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md), [Windows](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md), and [macOS](https://github.com/actions/runner-images/blob/main/images/macos/macos-14-arm64-Readme.md) inventories include newer rustup. This supports the activation approach; it does not substitute for executing the new workflow. Explicit local environment/CLI overrides can still supersede the file by design.
- `README.md:7,17` accurately separates the selected compiler from the unverified declared Rust 1.88 minimum. Pinning 1.99 fixes local/CI drift without misrepresenting MSRV coverage.

## Independent verification and limits

On Linux x86_64 with repository-selected Rust 1.99.0, independently ran and passed:

- Application and vendored `cargo fmt ... -- --check`.
- Application `cargo clippy --offline --locked --all-targets -- -D warnings` and standalone vendor all-target Clippy with `-D warnings`.
- `cargo test --offline --locked --all-targets`: **109 passed**, zero failed; example/binary test targets also completed.
- `cargo test --offline --manifest-path vendor/scap/Cargo.toml --lib`: **18 passed**, zero failed.
- `cargo build --offline --locked --release --bins --examples` and `git diff --check 13a7681`.

The initial restricted application test run passed 92 tests and failed 17 when localhost socket creation returned `PermissionDenied`; rerunning the unchanged suite with authorized socket access passed all 109. Tests used headless/synthetic inputs and localhost mocks. Existing author evidence was also inspected in `docs/evidence/*ci-repair.log` and the failed-run JSON/log.

No native UI, screen capture, physical-light, native credential, performance, Windows/macOS execution, or fresh toolchain download was performed by this reviewer. The new native CI matrix must establish installation, compilation, tests and artifact upload for the repaired commit before reporting the user's CI issue fully resolved. Existing runtime/hardware limitations remain outside this repair.

## Corrective pass 1 — final scoped disposition

Re-reviewed 2026-10-05. **Accepted at 9.1/10; no unresolved critical, high, medium or low implementation finding in this repair.** The initial score and finding remain historical above. Native matrix evidence is still pending, so the verification/platform score remains unchanged and the earlier overall application score remains 8.6/10.

- **Initial low finding resolved:** the workflow now runs standalone vendor formatting and strict all-target Clippy on Linux. The existing vendor helper-test step still runs on every matrix host. The conditions and commands introduce no platform-shell dependency.
- **QA installation concern resolved:** setup now explicitly runs `rustup toolchain install --no-self-update` before reporting the selected compiler. No version argument duplicates the toolchain file; local CLI help confirms omission installs the active toolchain. This also avoids relying on `rustup show` installation side effects, which upstream schedules to stop in rustup 1.30 ([rustup issue 4836](https://github.com/rust-lang/rustup/issues/4836)). `--no-self-update` leaves toolchain/component installation enabled.
- Independently executed the exact revised setup command sequence successfully: it selected the existing file-controlled Rust 1.99.0 installation and reported the expected compiler/Cargo. Also reran both exact new vendor step commands and `git diff --check 13a7681`; all passed. Inspected `docs/evidence/install-ci-repair.log`. QA's separate empty-home installation result is additional evidence reported by QA, not a fresh-download test performed by this reviewer.

No production source changed in this corrective pass, so the independently passing 109 application tests, 18 vendor tests and release checks above remain applicable. No additional regression or mandatory correction was found. Publication followed by a successful new native matrix remains the outstanding closure evidence; no Windows/macOS result or native runtime claim is added here.
