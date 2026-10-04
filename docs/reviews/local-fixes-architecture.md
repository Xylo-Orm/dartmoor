# Independent architecture and correctness review — local fixes

Reviewed 2026-10-04 against the actual local working tree in `/home/florian/Projects/dartmoor`, whose committed HEAD is `ae6c0f1`. This reviewer authored no implementation changes and changed only this report. No commit or push was performed. The review covered the latest sampling, startup, adapter-helper, text, handle-selection and preset changes, with inspection of their surrounding engine, capture, persistence and routing boundaries.

**Initial score: 8.5/10. Initial scope: yes, with one localized correctness finding to correct before final acceptance.** The implementation is coherent for the requested Rust/scap/egui/Tokio desktop-light scope. I found no critical or high implementation defect. A valid near-zero strip can sample the wrong pixel after its endpoints collapse during f32 scaling; the paired QA reviewer found this inherited edge and I independently reproduced it. Current native-platform, capture, credential and physical-device behavior remains unverified; this score is an implementation assessment with headless/mock evidence, not runtime certification.

| Dimension | Weight | Score | Evidence and limits |
|---|---:|---:|---|
| Correctness and features | 30% | 8.5 | Sampling order, feathering, linear averaging, single-color/addressable paths, strict layouts and preserved preset settings have regression evidence; one valid near-zero-strip edge remains to correct. |
| Architecture | 20% | 8.9 | Pure core/editor operations, capture-worker isolation, immutable snapshots and task-owned adapters fit the modular monolith. Startup is fallible; unused capability scaffolding is removed. |
| Resilience and concurrency | 20% | 8.8 | Bounded commands/events, latest-value frames/colors, cancellation guards, finite leases, stale-frame handling and recoverable worker-spawn failure are covered. |
| Verification and platform | 20% | 7.6 | Independently passed 101 application and 18 vendor helper tests, both packages' formatting/strict lint and Linux release builds. Current Windows/macOS compilation and native boundaries were excluded. |
| Usability and efficiency | 10% | 9.0 | Byte-bounded Unicode editing, coincident-handle selection, one-edit presets and the substantial sampling resource reduction have headless/synthetic evidence. Native visual quality remains unverified. |

Weighted total: **8.53**, rounded to **8.5**. This is the initial report on the frozen baseline, before the requested corrective pass.

## Findings, ranked by significance

1. **Medium, open — a valid strip can become empty after f32 pixel scaling and sample pixel zero.** Validation at `src/core.rs:142` compares the normalized points, but [compilation](../../src/core.rs) at `src/core.rs:286` multiplies each coordinate in f32. Distinct normalized x values `f32::from_bits(0x3ee66666)` and `f32::from_bits(0x3ee66667)` both become `72.0` at width 160. With both y values 0.5, the computed total length is zero, [cut_path](../../src/core.rs) at `src/core.rs:403` produces an empty path, and `make_region` at `src/core.rs:499` falls back to `(0, 0)` rather than the strip's location. I reproduced this against the actual release library with a pure probe in `/tmp/local-fixes-architecture-collapse.rs`: a valid one-zone, tiny-radius strip near `(72, 45)` sampled the blue first pixel in an otherwise red image. No native inputs or network were used. Preserve the real path position when scaled length is zero, or calculate the geometry with sufficient precision; add a regression checking the actual color for this valid case. The issue is localized to degenerate scaling, with no memory/concurrency or output-ownership failure established.

2. **Medium verification limitation — current native boundaries are not established.** The current uncommitted capture changes include macOS callback, metadata and image-buffer integration. Linux compiles and exercises the shared mailbox/image-state helpers, but does not compile those macOS framework paths. The successful native CI recorded for `ae6c0f1` does not cover this tree. Headless egui tests also cannot establish compositor rendering, DPI/accessibility, native permission dialogs, keyring availability or physical output. Locations: [macOS integration](../../vendor/scap/src/capturer/engine/mac/mod.rs), [capture boundary](../../src/capture.rs), [credential boundary](../../src/outputs.rs), and [verification record](../VERIFICATION.md). This is an explicit evidence gap, not evidence of a platform failure or a blocker within the permitted review scope. Later authorized validation should exercise these actual native boundaries.

3. **Low, documented — extreme layout compilation still pauses the capture worker.** [SamplingPlan::compile](../../src/core.rs) at `src/core.rs:278` evaluates each ordered region's geometry; [process_capture](../../src/engine.rs) at `src/engine.rs:332` recompiles on geometry changes before producing another frame. My release broad-strip probe measured **221.435 ms** compilation for eight strips × 256 zones, compared with **0.343 ms** sampling. This is a finite, observable interruption during extreme edits, while the UI and output tasks remain independent. [Known issues](../KNOWN_ISSUES.md) accurately documents it. Coalescing rapidly changing geometry or caching compiled plans could be useful if large live-edit workloads become a requirement; no redesign is required for the initial scope.

The first finding warrants one bounded corrective pass and focused re-review. Noncancelable portal/keyring calls, non-atomic WLED restoration, unconfirmed UDP delivery, approximate SDR downsampling and the lack of reliable cross-route physical identity remain clearly documented operational limits.

## Assessment of the latest fixes

The compact accumulation/storage algorithm is internally consistent; the first finding concerns the geometry it receives. [Region storage](../../src/core.rs) at `src/core.rs:232` distinguishes contiguous weight-one runs from feather pixels; normalization uses the sum of both classes. One f64 row-prefix table per frame supports both interior sums and individual feather contributions. Prefix indices include the leading zero for each row, and validated frame caps plus the compile-time assertion at `src/core.rs:241` protect the u16 representation. Runs cannot cross rows. Invalid frame dimensions/buffer length return black with the original light/zone cardinality. Bulbs retain one average broadcast to their zones, strips retain ordered regions, reversal changes only that order, and time-based smoothing and encoding remain separate.

The dense regression evaluates every pixel without prefix sums or runs and passed across six frame dimensions, four radii, repeated/crossing paths, corner bulbs and reversal. It shares path-cut/distance helpers with production, so it is strong evidence for the changed accumulation/storage algorithm, not a wholly independent proof of the entire geometry model. Separate existing arclength, reversal, averaging, encoding and editor tests provide the complementary checks. The maximum-zone storage regression checks the stated broad-strip workload below 4 MiB; neither it nor the benchmark proves a universal resource budget for every valid layout.

Startup now handles thread-creation failure before starting output tasks. [worker](../../src/engine.rs) at `src/engine.rs:428` propagates the OS spawn error; [Start handling](../../src/engine.rs) at `src/engine.rs:539` installs worker/output/active state only on success. The injected failed-spawn regression verifies no capture open, no preview/device state, and successful subsequent Start. [App::new](../../src/app.rs) at `src/app.rs:202` and [main](../../src/main.rs) propagate networking-runtime construction failure through the application startup result. This path is source/compile verified; I did not exhaust real OS resources to induce a runtime-builder failure.

The extracted WLED connection, health and lease helpers preserve the ownership boundary. One task per canonical WLED MAC owns connection/retry, cadence and release; the health task abort guard survives cancellation/unwind. Frames remain latest-value snapshots, no live lease is acquired before a fresh validated frame, the pre-acquisition restoration baseline remains intact, and shutdown keeps release outside the cancellable streaming loop. HA remains separately paced and round-robin, with unchanged-color coalescing, bounded acknowledgements/events and its own connection lifecycle. Removing the unused adapter capability trait makes the documented extension boundary match the actual task/channel dispatch.

The editor changes are narrow and coherent. [ByteText](../../src/app/text.rs) at `src/app/text.rs:16` budgets bytes, cuts incoming text at a UTF-8 boundary and delegates character-index editing to egui's String buffer; it permits repairing an existing oversized draft without truncating it merely by rendering. Config-backed fields use the matching schema limits. [Canvas selection](../../src/app/panels.rs) at `src/app/panels.rs:654` resolves equal-distance handles in favor of the selected light and point; pointer release clears drag state even for a click. [Strip presets](../../src/editor.rs) at `src/editor.rs:35` produce finite normalized paths with consistent screen order. The [preset menu](../../src/app/panels.rs) at `src/app/panels.rs:233` changes only points and selected-point state, preserving route, zones, extent and reversal. The actual headless menu regression proves one Undo restores the previous configuration.

## Initial-scope architecture

The requested scope is present: native egui placement for bulbs, single colors and ordered addressable strips; normalized geometry; SDR/sRGB conversion with linear-light averaging and smoothing; scap capture on a dedicated worker; bounded commands/events and latest preview/color/config snapshots; WLED JSON inspection and DDP; optional rate-limited HA WebSocket output; one selected route per record; explicit Start/Pause/Stop/error handling; strict credential-free persistence; and OS-keyring isolation.

The engine owns sessions, rejects retired-session frames and restricts live updates to unchanged sources/routes/rates. Physical WLED identities aggregate into one controller task with disjoint mappings. HA/WLED physical identity equivalence is deliberately not guessed. Capture cleanup guards cover successful opens, processing errors and unwind; stale capture ends output. The pure editor/history and core modules remain usable without native UI or device APIs. These are appropriate boundaries for the modular monolith; Hue Entertainment, HDR, multi-source composition and daemon/tray behavior remain deferred.

## Independent verification

I ran these checks on the reviewed tree:

- `cargo test --offline --locked --all-targets`: **101 passed, zero failed** in 5.13 seconds, using approved localhost-only mock socket access. Binary/example test targets also passed with zero tests.
- `cargo test --offline --manifest-path vendor/scap/Cargo.toml --lib`: **18 passed, zero failed**. These test packed-buffer and shared mailbox/image-state helpers; no native macOS execution occurred.
- Application and vendor `cargo fmt --check`: **passed**.
- Application and standalone vendor `cargo clippy --offline --all-targets -- -D warnings` (application also `--locked`): **passed**.
- `cargo build --offline --locked --release --bins --examples`: **passed**.
- `git diff --check`: **passed**.
- Release synthetic `--demo-seconds 3`: **91 frames**, reported processing approximately **0.021–0.041 ms**, clean shutdown; no device commands or desktop capture.

I also ran the existing release [sampling_stress example](../../examples/sampling_stress.rs) sequentially. Each workload produced eight lights / 2,048 zones:

| Synthetic workload | Compile | Sampling | Iterations | Process-local VmHWM |
|---|---:|---:|---:|---:|
| Broad strips | 221.435 ms | 0.343 ms/frame | 120 | 6,096 KiB |
| Narrow crossing strips | 2.714 ms | 0.058 ms/frame | 1,000 | 4,068 KiB |
| Bulbs | 0.422 ms | 0.014 ms/frame | 1,000 | 3,444 KiB |

These are single-process synthetic sampling checks after five warmups, with no capture, smoothing, UI or network work. They corroborate the implementation agent's current-workload measurements in [sampling evidence](../evidence/sampling-runs-local-fixes.log); I did not independently rerun the old implementation or claim native-window/real-capture CPU improvement.

Confidence is high in the inspected pure sampling/editor logic and mock-tested lifecycle, moderate in the overall desktop implementation, and limited for the deliberately untested native framework, compositor, credential and physical-device boundaries. No native UI, real capture, hardware, native keyring, Windows or macOS testing was performed during this pass.

## Corrective pass 1 — intermediate review

Re-reviewed the frozen corrected `src/core.rs` and `src/outputs.rs` on 2026-10-04. The initial findings, score and measurements above remain the baseline record. This reviewer authored no implementation changes and updated only this report.

**Provisional score: 8.5/10. Not yet accepted: one new medium HA cancellation finding requires correction.** The geometry defect is resolved and the ingress limits pass their checks, but the background-parse await introduced a message-loss boundary in the steady HA loop. One of the three permitted corrective attempts has been used.

| Dimension | Weight | Provisional score | Change from initial assessment |
|---|---:|---:|---|
| Correctness and features | 30% | 8.8 | The independently reproduced strip-placement error is corrected and covered with actual color expectations. |
| Architecture | 20% | 8.9 | The correction stays within the existing pure geometry and task-owned transport boundaries. |
| Resilience and concurrency | 20% | 8.3 | Ingress, deferred events and retained metadata now have explicit limits; WLED release remains intact, but an HA cadence tick can cancel processing after message consumption. |
| Verification and platform | 20% | 7.6 | Final independent Linux/headless/mock checks passed. Current native-platform and device evidence gaps remain. |
| Usability and efficiency | 10% | 9.0 | Earlier headless editor and sampling evidence stands; no new native performance claim is added. |

Provisional weighted total: **8.50**, rounded to **8.5**. Final scoring will follow the second corrective pass.

The medium geometry finding is **resolved**. `cut_path` at `src/core.rs:419` now keeps the first transformed point when no segment survives, so the region samples its declared location. The regression at `src/core.rs:769` covers both reviewers' collapse fixtures, one/eight zones and both reversal states, with a nonuniform frame and a direct expected color. I also rebuilt and reran my independent baseline probe against the corrected release library: the strip near `(72, 45)` now samples red at its own position rather than blue from pixel zero. The probe expectation changed only to require the corrected color; source is `/tmp/local-fixes-architecture-collapse-corrected.rs`.

The paired QA review's optional ingress hardening is also coherent. All WLED JSON consumers use `bounded_wled_json` at `src/outputs.rs:140`, rejecting an advertised body above 1 MiB before reading it and checking every chunk before extending retained body data. Inspection, timeout configuration, live reset, state and restoration acknowledgement use the same boundary. A failed restoration-state read still permits the separate live-reset attempt, and a failed/oversized reset acknowledgement follows the existing visible cleanup-error path. No indefinite lease or bypass of release ownership was introduced.

HA transport configuration explicitly limits messages to 32 MiB and frames to 16 MiB. `HaEvents` at `src/outputs.rs:918` stores the actual text-byte total, including whitespace and events retained across earlier requests, alongside the 256-event cap. Overflow rejects the new event without changing retained accounting; draining clears both values and byte total. Discovery and state-update projection limit retained light count and field sizes; malformed/mismatched light identities propagate an error through the stream rather than continuing with stale capabilities. `result.take()` transfers the acknowledgement result without cloning it. Inputs of at least 64 KiB move into `spawn_blocking` at `src/outputs.rs:121`; cancellation may let that CPU parse finish, but the closure owns only response data and cannot issue a device command. Stop/retry, health-task guards and WLED release remain in their existing owners.

The compatibility effect is intentional and documented in [configuration](../configuration.md) and [known issues](../KNOWN_ISSUES.md): unusually large installations or metadata may be refused in full. Ordinary supported RGB/RGBW/RGBWW/HS/XY metadata and short unknown future modes remain accepted. These limits bound raw messages and retained records, not total parsed-JSON heap size or all executor work. The review did not establish a universal RSS/CPU bound.

Independent final verification:

- `cargo test --offline --locked --all-targets`: **107 passed, zero failed**, 5.13 seconds, with approved localhost-only mock sockets. This includes the six new geometry/ingress regressions and existing cancellation, finite release, restoration, stale-frame and retry checks.
- `cargo fmt --all -- --check`, `cargo clippy --offline --locked --all-targets -- -D warnings`, `cargo build --offline --locked --release --bins --examples`, and `git diff --check`: **passed**.
- The unchanged vendor tree retains the independently passed **18 helper tests**, formatting and standalone strict all-target lint from the initial pass; these were not unnecessarily rerun for the application-only correction.
- The separate release geometry probe described above: **passed**. Earlier sampling measurements were preserved and were not repeated or relabeled as new native performance evidence.

**Medium, open — HA cadence can discard a consumed state event while background JSON parsing is pending.** `ha_stream` selects `ws_read(socket)` against its cadence tick; a large Text message is consumed at `src/outputs.rs:848` before `ws_read_sized` awaits background parsing at `src/outputs.rs:852`. If the tick wins during that await, dropping the read future discards the consumed state event and can retain stale availability/capabilities until refresh. The paired QA review raised this race, and I independently confirmed the cancellation path in source; the passing suite above lacks a regression for this interleaving. Cadence should select a raw receive, then commit processing of the received message; Stop may still end the complete session. A second bounded corrective pass with a deterministic timer/parser interleaving regression is required before acceptance. No native incident is claimed.

The explicit native verification gap and documented extreme-layout compilation delay also remain. Confidence is high in the corrected geometry and inspected ingress byte accounting, moderate in the whole desktop application, and limited at the excluded native/device boundaries. No native window, real capture, physical output, OS credential operation or Windows/macOS test was performed in this corrective pass. Native CI still applies only to committed `ae6c0f1`.

## Corrective pass 2 — final acceptance

Independently reviewed the frozen final `src/outputs.rs` on 2026-10-04 and reran the checks below. Earlier scores, findings and measurements remain the history of their respective trees. This reviewer authored no implementation changes and changed only this report.

**Final score: 8.6/10. Accepted for the initial scope. No unresolved critical, high or medium implementation finding was identified.** Two of the three permitted corrective attempts were used. No further correction is required for this batch.

| Dimension | Weight | Final score | Evidence |
|---|---:|---:|---|
| Correctness and features | 30% | 8.8 | Geometry and large-message state updates now preserve their declared behavior; feature coverage remains coherent. |
| Architecture | 20% | 8.9 | Raw receipt, committed decoding and terminal cancellation are distinct within the existing adapter task. |
| Resilience and concurrency | 20% | 8.9 | Cadence no longer discards consumed messages; queued parsers have cancellation ownership, with finite input budgets. |
| Verification and platform | 20% | 7.6 | 109 application tests independently rerun and a meaningful negative control passed their expected outcomes; native gaps remain. |
| Usability and efficiency | 10% | 9.0 | Existing headless editing/resource evidence remains valid; no new native performance claim is made. |

Weighted final total: **8.64**, rounded to **8.6**. This assesses implementation and the permitted headless/mock evidence, not cross-platform or physical-light certification.

The pass-1 medium HA finding is **resolved**. The steady loop at `src/outputs.rs:1220` selects raw `socket.next()` against cadence and Stop. After receipt, the loop retains the message while `decode_ha_message` runs; only terminal Stop can cancel that decoding. A cadence tick cannot take the consumed message away. Authentication and correlated acknowledgements use the same decoder through `ws_read_sized`, preserving JSON byte accounting, Ping/Pong handling, closure/capacity errors and response semantics. Their existing timeout/Stop cancellations end the affected connection rather than returning it to steady operation with a missing event.

The blocking parser at `src/outputs.rs:134` now has `AbortTaskOnDrop`. Cancellation aborts queued work; a parser already executing may finish its size-bounded CPU work, owning only its response buffer and no device-command capability. The new buffer-observation regression verifies cancellation releases the queued input without a second read/parse. The task-local queued notification is compiled only for tests and is scoped to the tested stream, so parallel tests do not depend on a shared scheduling hook.

The actual `ha_stream` regression holds its runtime's sole blocking worker, waits until a large message has been consumed and its parser queued, changes target colors, and advances the output timer. It verifies no stale-capability call occurs while parsing is held, then verifies the unavailable-RGB and unsupported on/off-only events suppress later calls once parsing completes. Its third scenario proves Stop completes while parsing is still held. These are actual loopback stream and service-call observations, not helper-only assertions. I independently reran the author's isolated `/tmp/dartmoor-cancel-regression-control` copy with only the old timer-versus-`ws_read` branch restored: the same behavioral test failed immediately with `cadence sent a color using stale state while parsing was held` and exit 101, as expected. The final production test passes. This provides evidence that the regression detects the repaired interleaving.

Independent final checks:

- `cargo test --offline --locked --all-targets`: **109 passed, zero failed**, 5.13 seconds, with approved localhost-only mocks. Binary/example test targets also passed with zero tests.
- `cargo fmt --all -- --check`, `cargo clippy --offline --locked --all-targets -- -D warnings`, `cargo build --offline --locked --release --bins --examples`, and `git diff --check`: **passed**.
- The isolated negative-control command recorded in `/tmp/dartmoor-cancel-regression-control/command.txt`: **expected failure confirmed independently**. Production files were not modified for it.
- The unchanged vendor tree retains its independently passed 18 helper tests, formatting and standalone strict all-target lint from the initial review. The geometry correction and independent release probe from pass 1 remain valid; neither area changed in pass 2.

Remaining limits are documented and do not require extending this batch: current native platform/capture/UI/credential/hardware behavior is unverified, extreme layout recompilation briefly interrupts frame production, unusually large HA replies can exceed supported limits, parsed JSON may use more memory than raw bytes, running blocking parses cannot be interrupted, and WLED delivery/restoration remain best effort under their existing policy. Confidence is high in the inspected pure/headless/mock logic and corrected ownership boundaries, moderate in the complete desktop implementation, and limited at excluded native/device boundaries. No commit, push, native test or renewed native performance measurement was performed. The native CI result continues to cover only `ae6c0f1`.
