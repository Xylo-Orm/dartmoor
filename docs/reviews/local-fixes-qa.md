# Independent QA, platform, UX and security review — local fixes

Reviewed 2026-10-04 by an independent GPT-6.1 Sol fallback reviewer. The requested GPT-6 Astra reviewer reached its model quota before producing a report; that failed turn is not counted as a completed review. I did not author production code, commit or push. This report assesses the local working tree, not only commit `ae6c0f1`.

**Initial disposition: accepted with two low-severity findings, 8.6/10.** No critical/high/medium implementation blocker was observed. The implementation satisfies the initial feature scope **yes**, subject to the stated native-platform and hardware verification limits. This score means coherent implementation with limited documented issues; it is not cross-platform or device certification. The overall paired-review score must be the lower of the two independent scores.

| Rubric | Weight | Score | Evidence and limits |
|---|---:|---:|---|
| Correctness and feature completeness | 30% | 8.8 | Single-source capture, bulb/single/addressable sampling, WLED physical mapping and HA capability handling are integrated. Compact sampling agrees with a dense reference on the tested geometries. One independently reproduced extreme valid-geometry error remains below. |
| Architecture and maintainability | 20% | 8.8 | Pure core/editor operations, a dedicated capture worker, immutable session snapshots and latest-frame output channels are coherent. The output helper refactor exposes connection, lease, command selection and refresh responsibilities. The unused capability-only trait is removed. |
| Resilience and failure behavior | 20% | 8.7 | Fallible worker creation precedes output task creation; retry recovery, cancellation, stale-output refusal and failed releases have regression coverage. Native credential calls remain noncancelable, and network ingress could use tighter resource budgets. |
| Verification and platform confidence | 20% | 7.8 | Independently passing current Linux compilation, lint and 101 application/18 portable helper tests. Current Windows/macOS compilation, native capture/UI, keyring and physical output are deliberately unverified. Earlier native CI applies to `ae6c0f1` only. |
| Usability and efficiency | 10% | 8.9 | Headless actual-handler tests cover byte-limited Unicode edits, selected overlapping handles, drag cleanup and one-transaction presets/Undo. Independently reproduced synthetic compact sampling costs are much lower for the extreme broad-strip workload. Native rendering, DPI/accessibility and actual capture overhead remain unverified. |

Weighted total **8.59**, rounded to **8.6/10**. The two remaining findings affect unusual inputs and resource hardening; neither prevents acceptance of the authorized headless/mock implementation scope.

**Finding 1 — Low, open: transformed-degenerate valid strips sample the wrong corner.**

`validate_lights` compares normalized points for nonzero length at [core.rs](../../src/core.rs#L140). `SamplingPlan::compile` multiplies those f32 points by frame dimensions at lines 286–288 and computes transformed segment lengths at lines 307–317. Distinct normalized points can round to the same transformed coordinate. Every zero-length segment is then excluded by `cut_path` at lines 403–419; the empty region's fallback at lines 497–500 uses `(0, 0)` rather than the declared placement.

I reproduced this against the current release library using `/tmp/dartmoor-local-qa-degenerate.rs`: a one-zone mock strip has points `(0.9f32, 0.9)` and `(f32::from_bits(0.9f32.to_bits() + 1), 0.9)`, radius `0.00001` and reversal false. Normalized validation passes. Both transformed x values equal `144` in a 160×90 frame. With every pixel green except red pixel zero, the actual result is `[[[1.0, 0.0, 0.0]]]`; placement near the declared points should produce green. This is an inherited extreme imported-geometry edge, rather than a demonstrated regression in the compact representation. It still gives a wrong color for a configuration accepted by the public API.

Repair: preserve the first transformed point when the complete transformed path collapses, or reject the transformed-degenerate path with a recoverable error. Add an adjacent-f32 regression that checks the actual declared region on a nonuniform frame. The existing dense oracle at lines 626–655 shares `cut_path`, `distance` and `path_distance`; it is independent of the storage/averaging optimization, but does not independently validate those geometry helpers. If given this empty path, its fallback `path[0]` at line 617 would also panic. The new regression should address both paths or use a separate expected pixel/color assertion.

Minimal fixture reproduction:

```rust
use lumen_desktop::core::{Frame, Light, Point, Route, SamplingPlan, Shape};
let x = 0.9f32;
let light = Light {
    id: "tiny-imported-strip".into(), name: "Tiny imported strip".into(),
    zones: 1, route: Route::Mock,
    shape: Shape::Strip {
        points: vec![Point { x, y: 0.9 },
                     Point { x: f32::from_bits(x.to_bits() + 1), y: 0.9 }],
        radius: 0.00001, reverse: false,
    },
};
let mut frame = Frame { width: 160, height: 90,
                        pixels: vec![[0, 255, 0]; 160 * 90] };
frame.pixels[0] = [255, 0, 0];
let actual = SamplingPlan::compile(&[light], 160, 90).unwrap().sample(&frame);
// Observed before correction: [[[1.0, 0.0, 0.0]]].
// Correct placement expectation: [[[0.0, 1.0, 0.0]]].
```

**Finding 2 — Low, open: remote JSON has generous or absent byte budgets.**

WLED inspection, reset acknowledgement, configuration and state use `Response::json()` at [outputs.rs](../../src/outputs.rs#L115), lines 173, 190 and 243. The locked reqwest 0.12.28 implementation in `src/async_impl/response.rs`, lines 269–295, calls `self.bytes()` and `BodyExt::collect()` before deserializing. Neither these call sites nor the HTTP client supplies a body-byte limit. The two-second request timeout limits waiting; it does not establish a memory bound for a fast large response or preempt synchronous serde parsing. Oversized remote strings are retained before the later persisted-layout validation/name truncation.

HA is **not unlimited per message**. [outputs.rs](../../src/outputs.rs#L827) uses `connect_async`, and locked tokio-tungstenite 0.28.0 `src/connect.rs:37` passes configuration `None`. Locked tungstenite 0.28.0 `src/protocol/mod.rs:95–104` defaults to **64 MiB incoming messages**, **16 MiB individual frames**, and **128 KiB read/write buffers**. Its write-buffer maximum defaults to `usize::MAX`, but application sends are serial and acknowledged/timeout-bounded; I did not establish an unbounded outgoing application queue.

During an acknowledged HA request, [outputs.rs](../../src/outputs.rs#L854) enforces a two-second timeout and rejects a 257th deferred event at lines 858–862. That is a useful **256-event count limit**, without an aggregate byte budget. The steady-state loop handles unsolicited events directly at lines 1080–1081 and drains deferred events after request completion; it does not maintain a permanent replay backlog. `get_states` still constructs/clones a complete JSON result at line 869 and retains every discovered light's strings/modes at lines 877–911. The picker truncates friendly names and limits painted rows, but discovery count, remote entity IDs and mode strings have no application-level ingress budget; [ha_picker.rs](../../src/app/ha_picker.rs#L34) retains/searches the discovered list. Persisted light/zone limits apply after discovery, not before receipt/parsing.

This is optional hardening for an oversized or misbehaving configured endpoint, **not an observed correctness failure against ordinary WLED/HA responses and not a blocker**. I inspected the actual locked library implementation and application call sites; I did not claim an out-of-memory attack or perform a destructive resource-exhaustion experiment. Repair: bounded streaming HTTP reads, an explicit HA WebSocket configuration chosen to accommodate legitimate large `get_states` replies, a deferred-event aggregate byte budget, and recoverable rejection or field/count limits before retaining discovery data. A shared 1 MiB limit copied from layout files would risk rejecting otherwise valid large HA installations and needs a separate budget decision.

**Assessment of the local fixes and regression coverage.**

The compact plan stores weight-one horizontal runs plus sparse feather pixels and builds a single f64 linear-sRGB row-prefix table per sampled frame. Prefix indices fit the declared maximum dimensions, weights normalize in f64 and malformed/mismatched frames preserve the configured black output shape. The reference test covers several frame sizes/aspect ratios, crossing and repeated paths, edges/corners, tiny/broad radii, zone ordering and reversal; the dark-pixel test targets cancellation after a bright row. The 2048-zone broad-strip test explicitly counts retained region/run/feather bytes below 4 MiB. It is a workload-specific retained-plan assertion, not a whole-process or universal layout-memory guarantee.

The coordinator now assigns `worker_state`, then starts `OutputGroup`, only after the fallible capture-thread spawner succeeds at [engine.rs](../../src/engine.rs#L539). Its injected failure test reaches Error without opening capture, then successfully starts a fresh mock session. Source ordering establishes that output helpers are not created on spawn failure; the test observes empty device status rather than instrumenting every possible network attempt. Busy and closed command queues produce distinct messages. Networking runtime construction returns errors through `App::new` and the native application creation callback, rather than calling `expect`.

ByteText delegates character-index deletion/insertion to egui's String implementation while clipping inserted text on a UTF-8 boundary to the schema's byte budget. Merely rendering an oversized existing draft does not mutate it. The actual egui paste test replaces a selected name with multibyte text and verifies valid persistence plus one layout Undo after leaving the editor. Host, URL, WLED identity/entity and layout-interchange fields use corresponding bounded adapters. Selected coincident canvas markers win exact-distance ties, pointer release clears click/drag state, and the actual input test verifies independent dragging and Undo. The strip preset handler replaces points only; the actual perimeter menu regression checks radius, reversal, zones, route and identity preservation. Pure tests cover all edge directions, three-side openness, closure and extreme inset values.

The WLED/HA refactor retains tested protocol behavior: canonical-controller aggregation, verified saved MAC identity and physical ranges; fail-closed finite timeout/indexing prerequisites; DDP offsets/sequence/push and latest-frame keepalives; stale/cancel release, visible release failure and guarded non-atomic restoration; HA authentication, acknowledgements, RGB/RGBW/RGBWW/HS/XY capabilities, availability changes, conservative call pacing, round-robin selection and unchanged-color coalescing. The health helper's abort guard addresses cancellation/unwind. HTTP reachability and successful local UDP send remain explicitly distinct from physical-light acknowledgement. Native token access stays isolated on a blocking task with a shared lease held across timeout/cancellation; tokens are absent from persisted/exported layouts. URL checks refuse embedded credentials and HTTP redirects are disabled.

**Independent checks against the reviewed working tree.**

| Check | Independent result |
|---|---|
| `cargo test --offline --locked --all-targets -- --test-threads=2` | 101 application tests passed, zero failed; 8.77 seconds. Binary/example test targets also completed. Authorized elevation supplied only localhost mock socket access. |
| `cargo test --offline --locked --manifest-path vendor/scap/Cargo.toml --lib` | 18 passed, zero failed. Pure packed-layout and portable macOS mailbox/frame-state helpers; not native capture. |
| Application and vendor `cargo fmt … -- --check` | Both passed. |
| Application and vendor `cargo clippy --offline --locked … --all-targets -- -D warnings` | Both passed. |
| `cargo build --offline --locked --release --bins --examples` | Passed on Linux; incremental completion 0.16 seconds after the recorded lead build. |
| `git diff --check` | Passed. |
| Current release `sampling_stress broad/narrow/bulbs` | Broad compile 222.705 ms, sample 0.340 ms/frame over 120 iterations, process-local VmHWM 6196 KiB. Narrow compile 2.360 ms, sample 0.062 ms/frame over 1000 iterations, VmHWM 3944 KiB. Bulbs compile 0.697 ms, sample 0.015 ms/frame over 1000 iterations, VmHWM 3448 KiB. Each mode ran in its own process; five warmups. |
| Separate transformed-degenerate strip probe | Source validation/compile passed and reproduced the wrong red corner color as detailed above. |

Synthetic resource probes exclude smoothing, capture, editor rendering, network and physical output; the process-local `/proc` figures are not the outer measurement script's child maximum RSS. The saved [local evidence](../VERIFICATION.md#local-fixes-and-strip-presets--2026-10-04) supports the lead's separately recorded checks and comparative sampling figures. I inspected those records and do not present their runs as my independent measurements. Dependencies and lockfile are unchanged.

**Confidence and remaining verification limits.**

Confidence is high for current pure validation, compact sampling on tested geometry, editor state and loopback protocol/lifecycle behavior; moderate for the inspected integration boundaries. Current macOS FFI/platform code is not compiled by Linux tests of shared helpers. The native CI result for `ae6c0f1` does not verify the current uncommitted capture/UI changes. No native GUI, real capture, real keyring, physical WLED/HA, Windows or macOS execution was performed in this review, as requested. Historical Wayland/UI evidence concerns older code. Native permissions, chooser/compositor behavior, display/DPI/accessibility, keyring backend behavior, visual color quality, actual output cadence and restoration races remain unverified. These evidence boundaries reduce the platform score; they are not inferred native failures or critical blockers.

The implementation covers the initial single-source scap/egui desktop synchronization scope, including manual WLED inspection/mapping, optional rate/capability-sensitive HA ambient output, persistent credential-free configuration and explicit Start/Stop/error/retry handling. HDR, multi-source composition, native vendor entertainment protocols and background/tray operation remain explicit deferred features. Acceptance requires reporting the two low findings and preserving the native/hardware confidence limits above.

**Corrective pass 1 focused review — original findings resolved; new cancellation edge found.**

I inspected the corrected working tree and independently reran 107 all-target application tests: all passed, zero failures, 9.02 seconds. The 18 vendored helpers, both packages' formatting/strict all-target Clippy, Linux release bins/examples and diff whitespace validation also passed. No production files were edited by this reviewer.

Finding 1 is **resolved**. [core.rs](../../src/core.rs#L419) preserves the first transformed point when slicing yields an empty path. The added regression at line 769 checks both reviewers' adjacent-f32 fixtures, one/eight zones and both reversal values against an independently specified green result on a nonuniform frame, and checks the dense oracle. I relinked the original standalone fixture against the corrected release library and observed `[[[0.0, 1.0, 0.0]]]`, the expected declared-position color.

Finding 2 is **resolved within the stated budgets**. [outputs.rs](../../src/outputs.rs#L140) checks declared WLED content length and each received chunk before retaining a body above 1 MiB; inspection, configuration, state, reset and restoration acknowledgement all use the helper. HA explicitly permits 32 MiB messages/16 MiB frames, retains at most 256 deferred events/8 MiB of their actual text bytes across consecutive requests, limits discovered/cached lights to 10,000, and validates retained entity/name/mode fields before cloning them. The request result is taken from the parsed object rather than cloned. Unknown short color modes remain available for future capability handling. Malformed/mismatched state identities fail visibly; normal unavailable/removed entities keep their existing behavior. Large parses move the owned buffer off the async executor. The raw budgets and parsed-JSON amplification/native-platform limits are accurately documented; these are not a universal RSS cap.

The new regressions exercise declared-length/chunked overflow before EOF plus valid HTTP recovery, oversized WebSocket frame-header rejection without sending its body plus reconnect, deferred-event byte/count bounds and reset, metadata/cache boundaries, large valid parsing and private error messages. Source inspection also verified every WLED JSON body consumer uses the bounded helper.

**Finding 3 — Medium, open in pass 1: a timer can discard a consumed large HA event.**

The new [outputs.rs](../../src/outputs.rs#L849) text path removes a message from the WebSocket, then awaits a blocking JSON parse at lines 851–852 for inputs at least 64 KiB. The steady-state `ha_stream` select at lines 1195–1200 races that entire `ws_read(socket)` against its output timer. If the timer wins while parsing is queued or running, the read future is dropped after consuming the message; its completed parse result has no owner. A valid availability/capability update can disappear until the periodic refresh. The earlier synchronous parser had no await between consuming the text and returning its value.

I reproduced the cancellation boundary with `/tmp/dartmoor-qa-cancel`, whose library is a copy of the actual corrected `src/outputs.rs` plus one isolated test; production code was not changed. Its runtime has one blocking worker held behind a channel gate, and a localhost WebSocket sends a valid `state_changed` event marking `light.desk` unavailable, padded with legal JSON whitespace beyond 64 KiB. Canceling the actual `ws_read` with a 100 ms timeout consumes the event and waits on its queued parser. After releasing the parser and sending a small sentinel, the next actual read returns only `{"marker":"next"}`; the unavailable event cannot be recovered. The deterministic test passed in 0.20 seconds with 32 unrelated copied output tests filtered out. This is a controlled scheduling probe, not a measurement of normal parsing latency or a claim that usual small HA light events trigger the race.

Repair: retain ownership of an in-flight parse across timer ticks, use a guarded dedicated reader, or select on receipt and finish parsing a received message before resuming the timer select. Preserve Stop responsiveness and off-executor parsing. Add a deterministic blocked-parser regression that proves an availability event survives a timer tick and suppresses the next service call. This is a medium regression because it undermines capability/availability-sensitive output, despite requiring an unusually large event or delayed parser. No critical/high issue was found; the initial feature scope remains implemented. A focused repair is recommended before the batch's final sign-off.

Pass 1 rubric scores: correctness/features **8.7** (30%), architecture **8.7** (20%), resilience **8.5** (20%), verification/platform **7.9** (20%), usability/efficiency **8.9** (10%); weighted **8.52**, rounded to **8.5/10**. The original initial score/findings above remain the record of that earlier tree. The native/platform/hardware confidence limits are unchanged.

**Final corrective pass 2 assessment — accepted, 8.7/10.**

Finding 3 is **resolved** in the final working tree. The steady HA loop at [outputs.rs](../../src/outputs.rs#L1218) selects raw `socket.next()` against the timer and Stop. Once that branch receives a message, the nested select finishes its shared decoder/parser before another timer branch can run. Only terminal Stop can cancel this committed work, which is appropriate because the connection is then retired. Availability/capability updates reach the cache before the next command selection. Authentication and acknowledged requests still use the same message decoder, WebSocket limits and bounded request/event paths; parsing remains off the executor for large messages.

The parser also now owns an abort-on-drop guard at [outputs.rs](../../src/outputs.rs#L134). Cancellation can prevent a queued blocking parse from executing; a blocking parse that already began may finish, with its independently bounded input. The code and evidence do not claim that Tokio can interrupt a running synchronous parser. The scoped `cfg(test)` notification observes only the tested stream's queued parse and introduces no production/global scheduling state.

I inspected and independently ran the actual behavioral regression at [outputs.rs](../../src/outputs.rs#L1402) as part of the passing final suite. It first proves baseline service acknowledgement, then holds the sole blocking worker, commits a large state event, changes the target color and advances virtual output ticks. It verifies that no service call uses the stale cache while parsing is held. Releasing parsing produces zero supported targets for both unavailable and unsupported on/off-only mode changes, and a separate case proves Stop completes while the parser remains held. The queued-parser test at line 1515 observes buffer release and proves the parser never reads its canceled queued input. These are actual streaming/parser behavior checks, not tests of a duplicated selection helper.

I also independently reran the temporary negative control described in `/tmp/dartmoor-cancel-regression-control/command.txt`, with only the old `ws_read`-versus-timer branch restored in the copied module. It exited **101 as expected**: `ha_stream_commits_large_state_events_across_ticks_and_stop_interrupts_parsing` failed on **“cadence sent a color using stale state while parsing was held.”** The inspected `negative-control.patch` changes that cancellation boundary only. This failed control is evidence that the regression detects the reported bug; it is not a failure of the final production tree.

All three findings in this report are resolved for the final tree. No unresolved critical/high/medium implementation blocker was observed. Initial feature scope: **yes**. The original and pass 1 assessments above remain historical records; this final disposition supersedes their open-finding status. Two of the permitted three corrective passes were used for this follow-up. No additional feature work or corrective pass is required for acceptance within the authorized verification boundary.

| Final rubric | Weight | Score | Assessment |
|---|---:|---:|---|
| Correctness and feature completeness | 30% | 8.9 | Declared-placement regression fixed with independent color expectations; HA availability/capability changes survive parser/timer scheduling. Existing feature/protocol regressions pass. |
| Architecture and maintainability | 20% | 8.8 | Raw receive/committed decode makes ownership explicit; shared decoding and abort guards preserve the isolated output design without an extra reader lifecycle. |
| Resilience and failure behavior | 20% | 8.9 | Bounded ingress/backlog/cache data, recoverable overflow/reconnect paths, private errors, parser cancellation and responsive Stop are covered. Running blocking work and native calls remain bounded/documented limitations rather than falsely cancelable operations. |
| Verification and platform confidence | 20% | 7.9 | Current final Linux checks and meaningful positive/negative scheduling tests pass. Native Windows/macOS, real capture/UI/keyring and physical-light verification remain outside the allowed scope. |
| Usability and efficiency | 10% | 8.9 | The reviewed Unicode/drag/preset corrections remain intact; compact sampling measurements retain their explicitly synthetic scope. Native rendering and general capture/network overhead are unverified. |

Weighted total **8.68**, rounded to **8.7/10**. This score reflects inspected behavior and verification, not the number of changes or review passes. The paired final score must still use the lower independent reviewer score.

Final independently rerun checks:

| Check | Final independent result |
|---|---|
| `cargo test --offline --locked --all-targets -- --test-threads=2` | **109 passed, zero failed**, 9.02 seconds; binary/example test targets completed. Authorized localhost mock sockets only. |
| Vendored scap library tests | **18 passed, zero failed**, including the unchanged portable mailbox/frame-state/layout helpers. |
| Both packages' formatting and strict all-target Clippy | Passed. |
| `cargo build --offline --locked --release --bins --examples` | Passed on Linux; final incremental completion 0.16 seconds. |
| `git diff --check` | Passed. |
| Temporary old-branch negative control | Expected regression failure, exit 101; final production regression passes. |

The lead's separate [109-test log](../evidence/tests-local-fixes-revision2.log), [lint log](../evidence/clippy-local-fixes-revision2.log) and [build log](../evidence/build-local-fixes-revision2.log) agree with the checks above; they are retained as lead evidence, not represented as my own runs. The earlier independent corrected release probe establishes the green declared-position result, and the final suite reruns its regression. No further performance claim is added by this scheduling repair.

Confidence remains high for the reviewed pure/editor logic and deterministic mocked protocol/lifecycle paths, moderate at inspected integration boundaries, and limited for native/platform/hardware behavior. Large HA installations may exceed the declared response/cache limits; parsed JSON can occupy more heap than its raw byte budget; an already running parser or OS credential call can outlive cancellation. Pending portal selection and non-atomic physical-device restoration retain their documented constraints. These are explicit practical limits, with no observed critical blocker. Native GUI/capture/keyring/device/Windows/macOS execution was not performed, and earlier CI for `ae6c0f1` is not current-tree certification. The implementation is accepted under this boundary; this reviewer edited only the QA report and temporary probes, with no production edit, commit or push.
