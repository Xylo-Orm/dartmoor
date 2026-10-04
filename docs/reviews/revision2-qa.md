# Independent revision 2 QA, protocol and platform review

Reviewed 2026-10-04 in `/home/florian/Projects/dartmoor`. This reviewer made no implementation changes. The review inspected the current sources and independently ran synthetic/mock Linux checks. Real capture, native UI interaction, native keyring/device access, and macOS/Windows checks were excluded as requested. Earlier evidence is historical and was not rerun or treated as current hardware verification.

**Score: 8.3/10. Accept the bounded initial working implementation. No unresolved critical/high finding was identified.** The protocol setup defects found during this review were corrected and rechecked before this verdict.

| Dimension | Weight | Score |
| --- | ---: | ---: |
| Correctness and scope | 30% | 8.6 |
| Architecture | 20% | 8.5 |
| Resilience | 20% | 8.5 |
| Verification and platforms | 20% | 7.8 |
| Usability | 10% | 8.0 |

Weighted score: 8.34, rounded to 8.3. The score covers implemented behavior, inspectable safety boundaries and local verification; it does not certify physical output, native platform support or complete desktop usability.

## Findings and resolution

1. **Medium, WLED physical indexing and packet filtering — resolved.** The first revision 2 guard checked main-segment mode, realtime LED maps and offset but omitted the DMX start address and sequence filter. WLED 0.15.1 adds `DMXAddress / 3` to the RGB DDP pixel index, so an address of 4 shifts every intended physical index by one. Its optional stale-packet heuristic assumes at most four packets before PUSH; a local model of that exact condition rejected 36 of 137 packets in subsequent large frames with this application's wrapping sequence. This is source-derived evidence, not a device test. The final `src/outputs.rs:196` guard requires readable DMX address 1 and sequence filtering disabled, alongside the existing finite timeout and mapping prerequisites. Policy regressions at `src/outputs.rs:1154` reject incompatible, missing and wrongly typed settings. Firmware references: [DDP handler](https://raw.githubusercontent.com/Aircoookie/WLED/v0.15.1/wled00/e131.cpp), [configuration serialization](https://raw.githubusercontent.com/Aircoookie/WLED/v0.15.1/wled00/cfg.cpp).

2. **Medium, persistent realtime override prevents output — resolved.** `live:false` exits realtime but does not clear the permanent override value 2. DDP can then acquire a timeout while skipping pixel writes. Final inspection requires readable `state.lor=0` in `src/outputs.rs:121`, with an actionable refusal rather than silently changing the device. The policy regression covers override values 1, 2 and missing state. The README's former phrase “clears any inherited live override” should describe clearing an inherited indefinite lease; the lead is updating operational documentation separately. Firmware references: [realtime exit](https://raw.githubusercontent.com/Aircoookie/WLED/v0.15.1/wled00/udp.cpp), [state serialization](https://raw.githubusercontent.com/Aircoookie/WLED/v0.15.1/wled00/json.cpp).

3. **Medium, stopped release failure hidden by wrapper — resolved after the architecture reviewer reported it.** The final wrapper at `src/outputs.rs:374` preserves the failed-release error when Stop or channel closure prevents retry. `wled_failed_stop_release_remains_visible` at line 1181 acquires against a mock controller, removes its HTTP server and verifies that final status contains the release failure without claiming a retry. Finite timeout remains the fallback; this test does not establish firmware expiry.

4. **Restoration baseline — improved and covered.** WLED restoration compares current observable state with the pre-acquisition baseline, rather than adopting a user change observed after the first DDP frame. The first-frame concurrent-change regression and unchanged-state restoration test pass. The application still cannot perform an atomic comparison-and-restore against WLED; that documented race remains a protocol limitation.

5. **Credential operation accumulation — resolved within process control.** The shared gate at `src/outputs.rs:645` covers both reads and saves. Its lease belongs to the blocking closure, so timeout or cancellation does not permit another native operation before the first returns. Injected blocked-operation tests verify executor responsiveness, repeated-call rejection, timeout, cancellation and eventual gate release. No native credential store was accessed. An in-flight OS operation can still finish after timeout, as its error explicitly says.

6. **HA coalescing after refresh — improved and covered.** `src/outputs.rs:1013` preserves the acknowledged RGB cache when refreshed availability and capabilities are unchanged, and invalidates changed/removed targets. Own color events do not invalidate the cache. Mock WebSocket tests check acknowledgement correlation, authentication rejection, stale suppression and coalescing. These tests establish local message behavior, not lamp capability or delivery.

7. **Capture cleanup and live edits — improved and covered.** `CaptureGuard` at `src/engine.rs:127` calls trait cleanup even after processing errors; low-FPS pacing can be interrupted by Stop. The engine accepts frames from the active session after a scalar/layout update, avoiding starvation from comparing immutable configuration identities. `same_geometry` at line 405 avoids resetting sampling/smoothing for a rename or scalar adjustment. The gated-source regression exercises an Apply while retrieval holds the old configuration and checks continued frames and smoothing. Mock failure and permission recovery tests pass.

8. **Editor and persistence boundaries — improved and covered.** `src/app.rs:31` validates additions transactionally; imported names are bounded without splitting UTF-8. Route inspection results apply only if the same light and route remain present. Repeated desktop-source selection retains its saved target, and preview identity distinguishes restarted sessions. `src/config.rs:195` preserves invalid, future and oversized originals before recovery saves. Existing strict-config and atomic-persistence tests pass. UI helpers were unit-tested and code-reviewed, not exercised in a native window.

## Independent checks

- Final `cargo test --locked` with authorized loopback access: **49 passed, 0 failed**.
- `cargo test --manifest-path vendor/scap/Cargo.toml --lib`: **5 passed, 0 failed**.
- Final `cargo fmt --all -- --check`: **passed**.
- Final `cargo clippy --locked --all-targets -- -D warnings`: **passed**.
- Reviewed output lifecycle, capability conversion, identities, capture/engine, persistence, UI helpers and documentation claims.

The first sandboxed application run was 34 passed/10 failed because socket creation returned `Operation not permitted`; those were environment failures, not counted as passing protocol checks. An authorized intermediate run passed 47 tests. The final 49-test run includes the later mapping-policy and failed-release-status regressions. The former UDP4048 test collision is removed: mock session destinations and socket listeners use ephemeral loopback ports; production remains port4048. No release build, benchmark, UI smoke, capture probe, external HA/WLED connection or native-platform execution was performed by this reviewer.

## Remaining limits and follow-ups

- **Medium, verification boundary:** physical WLED/HA output, finite firmware timeout after process loss, native keyring behavior, real capture/UI workflows and full Windows/macOS application behavior remain unverified in this iteration. These were explicitly excluded and are not requests for more testing now. Keep them visible in release claims.
- **Low, compatibility:** the guard deliberately supports a narrow WLED 0.15.1 configuration schema and refuses alternatives. Some mathematically equivalent DMX addresses are conservatively refused. Device settings or DNS/identity changes during a running session and competing senders are not locked atomically; startup inspection cannot establish exclusivity.
- **Low, documentation maintenance:** at review start, `KNOWN_ISSUES.md` still described the removed fixed-port test limitation, and aggregate review/verification conclusions still described revision 1. Update them to identify historical results and final revision 2 evidence. This report's facts reflect the final source and checks; documentation edits are being completed by the lead.
- **Low, maintainability:** several UI branches and output loops remain densely formatted. The `OutputAdapter` trait is a future capability sketch rather than the active dispatch mechanism. Current routes have clear task ownership, but a future backend should not assume a complete plugin interface already exists.

The requested initial Linux implementation is coherent and locally verified within the allowed scope. The acceptance threshold is met without inventing capture, display, keyring, hardware or platform evidence.
