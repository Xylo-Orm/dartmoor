Independent QA/platform review of the CI repair, 2026-10-05.

Reviewed the actual working-tree changes against `13a76818c65607ffa5e5703c4c8b10080eb79554`, the proposed `rust-toolchain.toml`, saved native CI diagnostics, README, `docs/VERIFICATION.md` and `docs/REVIEW.md`. This reviewer did not author the application or repair. Initial disposition: **accept the scoped repair for publication and native CI verification, 9.4/10**. There are no critical, high or medium findings in this repair. Native CI success is still pending; approval of this patch does not establish native runtime or hardware correctness.

| Criterion | Weight | Initial score |
|---|---:|---:|
| Correctness and requested scope | 30% | 9.8 |
| Architecture | 20% | 9.6 |
| Resilience and concurrency | 20% | 9.0 |
| Verification and platform coverage | 20% | 8.8 |
| Usability and efficiency | 10% | 9.8 |
| Weighted scoped score | 100% | **9.4** |

Severity-ranked finding:

1. **Low — deprecated implicit installation in the setup step** (`.github/workflows/ci.yml:16`, initial diff). A clean temporary Rustup home successfully installed the pinned Rust 1.99.0 and all five expected components through `rustup show`, but Rustup 1.29.1 printed an explicit deprecation warning for this behavior. Its [upstream issue](https://github.com/rust-lang/rustup/issues/4836) says `show` becomes query-only in Rustup 1.30. Add `rustup toolchain install --no-self-update` before `rustup show`, without repeating the version or components; the install command uses the active repository toolchain. The following `rustc` proxy remains able to install a missing toolchain, so this is a maintainability and setup-diagnostics issue, not evidence that the current workflow fails. A separate negative experiment also showed `rustup show` returning success after a deliberately unreachable distribution server prevented installation, reinforcing the value of an explicit install step.

The failure diagnosis is supported by `docs/evidence/ci-13a7681-failure.json` and `.log`: all three native jobs passed formatting and failed strict Clippy with the same seven float fallback errors; later tests and release steps were skipped. The macOS dependency warnings were not the fatal diagnostics. No lint suppression or skipped required check was introduced by the repair.

The source diff is exclusively seven `_f32` suffixes: one at `src/app.rs:421` and six at `src/app/panels.rs:513`, `:517`, `:554`, `:573`, `:584` and `:636`. I mechanically compared both files after removing those suffixes and obtained equality with the base commit. Epaint's `Stroke::new` accepts `impl Into<f32>`; the explicit types preserve the previous fallback and the exactly representable widths 1, 2 and 2.5. Capture, protocol messages, retry timing, cleanup, configuration, feature selection, dependencies and lockfiles are unchanged. Existing protocol/lifecycle regressions also passed.

The TOML contains one compiler version and additive Rustfmt/Clippy components on the minimal profile. It leaves the host target native to each runner, as described by the [Rustup toolchain-file documentation](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file). The commands in the setup step work as plain executable invocations in both the Unix and Windows workflow shells. Current hosted-image inventories include Rustup on [Ubuntu 24.04](https://github.com/actions/runner-images/blob/main/images/ubuntu/Ubuntu2404-Readme.md), [Windows](https://github.com/actions/runner-images/blob/main/images/windows/Windows2025-Readme.md), and [macOS 14](https://github.com/actions/runner-images/blob/main/images/macos/macos-14-Readme.md). Those inventories support the setup dependency; they do not substitute for running this patch's matrix.

Independent Linux verification used Rust 1.99.0 (`b940084d7`, x86_64):

- `rustup show`, `rustc --version --verbose`, `cargo --version`, and installed-component inspection confirmed repository selection and Rustfmt/Clippy availability.
- In a separate empty temporary Rustup home, `rustup show` downloaded and installed Rust 1.99.0, Cargo, host standard library, Rustfmt and Clippy. Subsequent compiler/Cargo version queries passed. The temporary installation was removed without altering the user's toolchains.
- `cargo fmt --all -- --check` and standalone vendor formatting passed.
- `cargo clippy --locked --all-targets -- -D warnings` and offline standalone vendor all-target strict Clippy passed.
- `cargo test --locked --all-targets`: 109 application tests passed, zero failures. Network tests used localhost mocks only.
- Offline standalone vendor library tests: 18 passed, zero failures. These include portable macOS mailbox/state helpers, not macOS capture execution.
- `cargo build --locked --release --bins --examples` and `git diff --check` passed.

The initial sandboxed `rustup --version` attempt encountered an environment `EPERM` in Rustup's process handling; the subsequent permitted run succeeded. The application test command ran with permission for its localhost sockets. No native window, live capture, real controller, native credential store, or performance test was run.

The repair preserves the documented initial application scope: editor/sampling, WLED and optional Home Assistant outputs, and bounded session ownership/cleanup. That conclusion is limited to the unchanged source contracts and the passing existing regressions. The prior full-implementation **8.6/10** assessment remains historical; this **9.4/10** score assesses only the CI repair. Rust 1.88 remains an untested declared minimum. Native Windows/macOS compilation and tests for this repair, native rendering/permissions/capture, real lights, and new performance measurements remain unverified. The new documentation states these distinctions accurately. Record the new native run against its exact published commit before describing this CI incident as fully resolved.

**Corrective-pass addendum, 2026-10-05:** The lead added `rustup toolchain install --no-self-update` at `.github/workflows/ci.yml:16`, followed by toolchain/compiler/Cargo reporting. The sole low finding above is resolved. I independently ran this exact installation command in a second empty temporary Rustup home: it selected the repository file, installed Rust 1.99.0 and all five required components, and exited successfully. The following `rustup show`, compiler/Cargo queries and installed-component inspection all succeeded without the implicit-installation deprecation warning. The temporary home was removed afterward.

The final workflow also adds standalone vendor formatting and strict all-target Clippy on Linux at `.github/workflows/ci.yml:28` and `:31`. Both exact commands passed independently. These checks strengthen the existing Linux evidence without asserting standalone vendor lint cleanliness on macOS, whose inherited dependency warnings are already documented. All three platforms retain application formatting, strict application Clippy, application and vendor tests, release builds and artifact publication. The final source diff remains the same seven float suffixes; application tests and release verification above remain applicable. Final whitespace validation passed.

Final scoped disposition: **approve publication followed by the native matrix, 9.5/10** (correctness 9.8, architecture 9.6, resilience 9.6, verification 8.8, usability 9.8; weighted 9.52, rounded). No unresolved critical, high, medium or low repair finding remains. Confidence is high in the diagnosed compiler fix and Linux setup/check behavior; cross-platform CI remains pending at this review's completion. All historical-score and native-runtime/hardware limits above still apply.
