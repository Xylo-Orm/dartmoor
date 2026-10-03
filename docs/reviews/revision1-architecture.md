# Revision 1 independent architecture and correctness review

Reviewed 2026-10-03 against the revised source in `/home/florian/Projects/lumen-desktop`. This reviewer did not change implementation code. The initial assessment is preserved in `initial-architecture.md`.

**Score: 8.2/10. Meets the requested 8/10 architecture-review threshold. No unresolved critical or high-severity implementation issue was found in this review.** The initial blockers have been corrected with relevant regression coverage. This accepts a coherent working implementation with explicit platform/hardware limitations, not cross-platform runtime certification or physical-light verification.

| Dimension | Weight | Score | Assessment |
|---|---:|---:|---|
| Correctness | 30% | 8.4 | Ownership, HA geometry and the known non-Linux name-resolution defect corrected. |
| Architecture | 20% | 8.5 | Physical-controller grouping is consistent; blocking credential reads are isolated; capture loop clearer. |
| Resilience | 20% | 8.2 | Finite WLED acquisition replaces indefinite takeover; stale/cancel/release and bounded credential paths covered. |
| Verification | 20% | 7.7 | Independently reproduced 36 application tests, 5 vendor tests, formatting and strict Clippy; hardware/native-platform gaps remain. |
| Usability | 10% | 8.0 | Editor route inspection and shape preservation improved; operational limits need continued clear documentation. |

Weighted score: 8.21, rounded to 8.2. The increase is based on inspected fixes and verification, not the quantity of changes.

## Initial findings rechecked

**WLED indefinite lease — resolved.** `src/outputs.rs:152–203` now only posts `live:false`, validates the acknowledgement, reads the device's existing realtime configuration and rejects missing/disabled/unsafe settings. Accepted timeout values are 0.1–10 seconds; persistent device configuration is not written. `src/outputs.rs:467–485` resets an inherited indefinite session before sending DDP, then captures restoration state in a separate task so it does not interrupt steady packet cadence. All exit paths still attempt release outside the fallible streaming loop.

I independently checked the pinned protocol interpretation: WLED 0.15.1's [configuration implementation](https://github.com/Aircoookie/WLED/blob/v0.15.1/wled00/cfg.cpp) stores the realtime timeout in 100ms units, and its [DDP handler](https://github.com/Aircoookie/WLED/blob/v0.15.1/wled00/e131.cpp) passes that timeout into realtime acquisition. The revised approach addresses the original infinite-lease defect. Tests cover unsafe configuration rejection, absence of `live:true` and persistent-config writes, latest-frame streaming, stale cessation, restart, cancellation and guarded restoration. They do not simulate firmware expiration after killing the application; finite expiration remains a source-backed protocol inference pending hardware testing.

**Duplicate physical-controller owners — resolved.** `src/core.rs:73–85,200–220` normalizes identity before overlap validation, and `src/engine.rs:192–232` uses that same identity for both controller creation and frame combination. Hostname/IP aliases with equivalent MACs combine into one task; disjoint physical ranges share one frame. Tests at `src/config.rs:341–393` and `src/engine.rs:589–622` cover normalization, overlapping aliases, conflicting IDs on one host, and exact combined colors. Selecting the first configured endpoint for equivalent aliases is a reasonable deterministic policy, though it is not endpoint failover.

**Non-Linux application import — resolved by source inspection.** `src/app.rs:28–29` now uses `Vec<crate::capture::Source>`. The specific name-resolution defect is gone. This is not evidence that the whole application builds on Windows/macOS; those complete native builds remain outstanding.

**Blocking keyring reads on Tokio — resolved for network-worker isolation.** `src/outputs.rs:563–577` moves reads into `spawn_blocking`, bounds the async wait, and reports timeout/failure. Discovery uses that path, and HA startup selects between Stop and the lookup (`src/outputs.rs:792–806`). A failed lookup ends that startup attempt instead of spawning further lookups on an automatic retry. The injected blocking-reader test runs on Tokio's current-thread test runtime and proves the timer progresses, lookup times out, and only one operation was started. OS credential operations already running cannot themselves be forcibly cancelled; this residual limitation is acknowledged in source.

**HA strip sampling only the first zone — resolved.** `src/core.rs:155–160` requires one HA zone at the configuration boundary. `src/config.rs:397–428` tests invalid persisted multi-zone input preservation and the complete-path average for a valid single-zone strip. Non-selected persisted rows can no longer bypass the invariant.

**Control-loop readability — improved, with minor debt remaining.** The dedicated `process_capture` function (`src/engine.rs:234–322`) and routing helpers make the key ownership and sampling logic substantially easier to audit. Some UI and output control flow remains densely formatted; this is maintainability work, not a blocker.

## Remaining issues and improvements, severity ranked

1. **Medium, verification limitation:** full application Windows/macOS builds, native capture and physical WLED/HA execution are still unverified. The reported Windows scap-only cross-check is useful but cannot establish application-wide compatibility. The missing MinGW C compiler is an environment limitation, not a passing or failing application build. Run the existing native matrix and perform explicit real-device cleanup/recovery checks when suitable hosts/hardware are available. Do not present this review's score as a substitute for that evidence.
2. **Low, compatibility/documentation:** the finite-lease guard relies on `/json/cfg`, whose firmware source describes its structure as internal and changeable. Failing closed is the right behavior, but users need the actual prerequisite: readable realtime configuration, UDP reception enabled, timeout 0.1–10 seconds, and a compatible configuration schema. The updated README and known-issues file now state the configuration/UDP/timeout prerequisites. Keep the firmware-schema compatibility limit explicit, and carry the current 36-test result into final verification evidence. Do not claim physical lease-expiration testing. Existing external changes to firmware settings or competing controllers can still invalidate the initial observed settings, consistent with the documented lack of exclusive protocol ownership.
3. **Low, test isolation:** `src/outputs.rs:1213` binds fixed localhost UDP port 4048. My first rerun passed 35 tests but failed this test with `AddrInUse` while other review work was active; a subsequent complete run passed all 36. Injecting the UDP destination port for tests would remove this cross-process collision. This is a test harness limitation, not an observed streaming failure.
4. **Low, credential UX:** a timed-out blocking OS read can survive until the credential service returns; explicitly repeating Start can initiate another attempt. The save operation also waits for the OS prompt in a blocking task without a UI timeout. Executor isolation and process shutdown are preserved, but a future credential-operation gate/cancel indication would make repeated locked-keyring use clearer. No real keyring prompt was exercised here.
5. **Low, coverage/maintenance:** the native UI improvements were inspected in code, not walked through manually by this reviewer. Continue targeted checks for route switching, source selection, editing and recovery rather than expanding tests that merely mirror implementation. The existing adapter trait remains a capability sketch rather than the actual dispatch abstraction; avoid overstating it when describing a future backend.

The documented synchronous portal cancellation limit, conservative two-second idle-frame failure policy, SDR-only contract, approximate downsampling and best-effort restoration remain reasonable explicit constraints for this version. They are not silently converted into guarantees by this score.

## Independent checks performed

- `cargo test --locked` with authorized loopback access: **36 passed, 0 failed** on the completed rerun. The initial fixed-port collision is recorded above.
- `cargo test --manifest-path vendor/scap/Cargo.toml --lib`: **5 passed, 0 failed**.
- `cargo fmt --all -- --check`: **passed**.
- `cargo clippy --locked --all-targets -- -D warnings`: **passed**.
- Read the revised controller/configuration/credential/capture/UI paths and their regression tests; checked WLED 0.15.1 source semantics independently.

The lead reports a passing release build and is recording updated performance/UI evidence separately. I did not repeat release timing, real desktop capture, manual UI interaction, native-platform compilation or physical-device tests during this re-review. The earlier Hyprland capture and minimize/restore evidence remain useful but do not establish final optimized capture overhead; use the lead's completed new measurements for that claim.

**Scope judgment:** the requested architecture and functional Linux implementation satisfy this review's acceptance bar, with limited documented issues. All findings that prevented the initial architecture acceptance are addressed. Final delivery should preserve the distinctions between tested Linux behavior, mocked network behavior, source-verified firmware expectations and untested platform/hardware behavior. Confidence is high in the inspected fixes and local checks, moderate in overall desktop usability, and limited for untested native platforms and real lights.
