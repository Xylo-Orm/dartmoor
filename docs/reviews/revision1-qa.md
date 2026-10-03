# Independent revision 1 QA, platform and protocol review

2026-10-03. Same independent reviewer as `initial-qa.md`; no implementation edits. Reviewed the actual revised sources and regression tests. Documentation/performance recording was being finalized concurrently, so later measurements are not assumed in this score.

**Score: 8.1/10. Acceptable as a coherent initial release with the documented limits.** The initial high-severity defects are corrected, and no unresolved critical/high finding was found in this re-review. This is not a claim of physical-light certification or native Windows/macOS runtime verification.

| Rubric | Weight | Score |
| --- | ---: | ---: |
| Correctness and scope | 30% | 8.4 |
| Architecture | 20% | 8.5 |
| Resilience | 20% | 8.2 |
| Verification and platforms | 20% | 7.2 |
| Usability | 10% | 7.5 |

Weighted result 8.05, rounded to 8.1. Verification/usability scores retain the real limits on native platforms, hardware and complete manual UI testing.

## Resolution of initial findings

1. **WLED infinite acquisition lease: resolved.** `src/outputs.rs:152-205,367-510` never acquires with JSON `live:true`. It reads the device configuration, requires enabled realtime reception and a finite timeout of 0.1–10 seconds, resets inherited live state with acknowledged `live:false`, then streams DDP. Stop/stale/error paths attempt explicit release. The timeout unit matches [WLED v0.15.1 configuration serialization](https://raw.githubusercontent.com/Aircoookie/WLED/v0.15.1/wled00/cfg.cpp): hundred-millisecond units. Normal DDP refreshes the finite timeout, unlike the original special infinity acquisition. The device remains responsible for timeout enforcement; mocks cannot establish physical firmware behavior.

   Regression coverage rejects missing/disabled/out-of-range timeout settings, forbids `live:true` in the fake server, checks failed reset cleanup, receives repeated newest-color DDP packets, verifies stale capture stops sending, and verifies cancellation release. It also asserts persistent device configuration is not written. An asynchronous post-send state snapshot avoids blocking DDP keepalives during optional restoration setup.

2. **Non-Linux unresolved type: resolved in source.** `src/app.rs:29` now uses `Vec<crate::capture::Source>`. The full application has not been compiled or run on Windows/macOS here. The documented successful Windows cross-check applies to vendored scap only; the distinction is appropriate.

3. **Split controller tasks from hostname/MAC spelling: resolved.** `src/core.rs:74-85,200-220` canonicalizes identity and host comparison. `src/engine.rs:190-233` uses canonical device identity for both task grouping and frame aggregation. Aliases of one physical identity combine into one route/frame; overlapping ranges are rejected across aliases, and one host cannot claim conflicting identities. A regression test checks adjacent ranges with differently formatted MACs and different host aliases produce one complete frame.

4. **Blocking keyring reads on Tokio workers: resolved.** `src/outputs.rs:564-579,783-809` uses `spawn_blocking`, a bounded wait, and a Stop-selectable startup attempt. A current-thread async regression confirms the executor continues while a fake lookup blocks, times out, and starts once. Native OS calls already running cannot be force-cancelled; that residual constraint is explicit and no internal retry loop creates repeated lookups.

5. **Active shape button resets geometry: resolved in source.** Shape replacement now requires an actual change of shape. Clicking the already-selected bulb/strip no longer resets placement.

6. **RGBW/WW zero-white policy mismatch: resolved.** `src/outputs.rs:714-751` now emits supported `rgbw_color`/`rgbww_color` arrays with explicit zero white channels; tests verify those arrays and brightness normalization. HA configuration also requires exactly one logical zone so an externally edited strip layout cannot silently send just its first sampled region.

7. **Raw selected-source IDs: improved.** The selector resolves known IDs to source names. The saved-source fallback remains generic until Refresh populates the list, but this is a minor usability limitation. “Inspect this route” now supplies a practical way to populate the WLED MAC and LED range after changing an existing record's route.

## Independent verification

- `cargo test --locked` with authorized local socket access: **36 passed**, zero failures.
- `cargo fmt --all -- --check`: **passed**.
- `cargo clippy --locked --all-targets -- -D warnings`: **passed**.
- Reviewed revised output lifecycle, canonical routing, credential lookup and UI paths directly.
- Initial independent scap test result remains **5 passed**. Lead reports revised release build and scap Windows cross-check passing; those were not independently repeated in this re-review.

Tests meaningfully cover local protocol construction and state transitions. The finite-lease tests inspect application messages and mock timing; they do not emulate a complete WLED firmware or prove actual light output. Similarly, mock HA success establishes correlated service acknowledgements, not delivery to a physical lamp.

## Remaining issues and limits

No new critical/high issue found. These are bounded follow-ups rather than grounds to reject the initial release:

- **Device-configuration compatibility/setup:** `/json/cfg` is read-only here, but WLED's source explicitly treats its schema as unstable. Firmware without the expected fields safely refuses streaming. Document the tested WLED schema/version assumption and actionable timeout/reception setup. Whole-controller physical indexing additionally assumes compatible realtime settings: main-segment-only mode, realtime LED maps and a nonzero realtime offset can change the result. Document those prerequisites instead of implying arbitrary controller configurations have identical mapping semantics.
- **Native platform coverage:** Windows scap cross-compilation is valuable but does not replace a full application build, permission test or capture session. macOS compilation/runtime and Windows runtime remain unverified. CI is configuration, not evidence of a run. Native keyring backend availability also remains unverified.
- **UI coverage:** Shape-reset and route/source improvements are code-reviewed. Automated synthetic minimize/restore evidence does not cover all marker drags, small-window layouts or a full manual workflow. The lead reports the attempted built-in screenshot was black, so this reviewer claims no visual QA from it. Friendly saved-source names require refresh; very large HA discovery sets can exceed the documented 64-record limit and require removing records before Start/Save.
- **Performance:** Initial real capture figures preceded optimization. Updated benchmark/headless measurements and any later final real-capture evidence must retain their workload boundaries. No GPU measurement exists; synthetic sampling and UI figures cannot stand in for a combined real-capture editor workload. No final post-change performance result is invented by this review.
- **Documented protocol limitations:** UDP delivery remains unconfirmed, HA updates are deliberately slow and retained after Stop, restoration cannot protect perfectly against concurrent changes, and no automatic cross-route identity match or failover is attempted. Other senders or manual configuration changes during a session can invalidate assumptions; there is no exclusive protocol lease.

The requested initial scope is substantially satisfied: native editor, screen/window capture path, mock demonstration, bulb and strip geometry, one/many-zone sampling, independent direct/HA output paths, persistence, failure handling and diagnostics. Acceptance is for this bounded initial implementation with the above honest limits, not for inaccessible hardware or platform evidence.
