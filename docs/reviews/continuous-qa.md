# Independent continuous QA, platform and UX review

Reviewed 2026-10-04 against the final local, uncommitted working tree. This reviewer inspected the implementation and ran independent Linux checks without authoring implementation changes, committing or pushing. Reviewer changes are limited to this report; additional reviewer probes lived in `/tmp`. This is the available Sol fallback review. The requested Astra reviewers exhausted their model usage allowance without producing a completed review.

**Score: 8.5/10. Accepted for the requested initial scope, with no unresolved critical, high or medium implementation finding identified.** The two meaningful defects reported during this review were corrected and reverified before this verdict. Current native-platform, hardware and visual evidence limits remain substantial and explicit.

| Dimension | Weight | Score | Assessment |
|---|---:|---:|---|
| Correctness and features | 30% | 8.8 | The editor, strict layout interchange, source/session routing, sampling order, selective HA discovery and WLED lifecycle have concrete source and regression evidence. |
| Architecture | 20% | 8.7 | Pure core/editor operations, immutable session/configuration snapshots and adapter-owned tasks fit the scope. UI modules improve separation; the capability trait remains a future sketch. |
| Resilience | 20% | 8.8 | Bounded history/channels, conservative freshness, credential exclusion, cleanup guards, finite WLED leases and the repaired macOS mailbox cover consequential failures. |
| Verification and platform | 20% | 7.7 | Independent Linux release, 88 application tests, 18 vendor tests and strict lint checks pass. Current macOS native integration and Windows/macOS runtime, physical devices, keyring and native presentation remain unverified. |
| UX and efficiency | 10% | 8.5 | Actual headless egui handlers cover selection, modal interchange, Undo/Redo and placement. Bulb duplication is fixed; extreme broad strip plans remain expensive and documented. |

Weighted total: **8.53**, rounded to **8.5**. This is an implementation assessment under the authorized headless/mock verification boundary, not platform or device certification.

## Findings, severity and disposition

### Medium, resolved: Idle could perpetually refresh an obsolete macOS image

The original one-slot `try_send` queue could contain Idle when a changed Complete frame arrived. Dropping that Complete frame and subsequently accepting Idle let the application retain its old image while continually refreshing capture liveness. The pinned ScreenCaptureKit wrapper also defaults absent status metadata to Idle, which did not establish that the image was unchanged. I reported this before finalization and reproduced the queue failure with a deterministic standard-library model: one changed image was discarded while 101 accepted Idle events preserved the old image. This model was evidence about the inspected queue algorithm, not a native capture test.

The concrete repair is now present in [mac_mailbox.rs](../../vendor/scap/src/capturer/engine/mac_mailbox.rs), `Sender::send` at line 75: changed images and invalidations replace pending samples, while Idle cannot overwrite either. Displaced native samples are released outside the mutex. The macOS path in [capturer/mod.rs](../../vendor/scap/src/capturer/mod.rs), lines 145 and 166, no longer drains a later Idle over an already selected image. Deadline, wakeup, disconnect and exact-drop tests exercise the actual portable mailbox implementation.

[pixelformat.rs](../../vendor/scap/src/capturer/engine/mac/pixelformat.rs), `explicit_frame_status` at line 59, reads present, correctly typed native status metadata instead of using the wrapper fallback. The actual worker policy in [mac_frame_state.rs](../../vendor/scap/src/capturer/engine/mac_frame_state.rs), lines 16 and 47, permits Idle only after a successfully converted image. Missing/unknown statuses, failed image conversion and backend errors invalidate that eligibility. The raw [pixel_buffer.rs](../../vendor/scap/src/capturer/engine/mac/pixel_buffer.rs) constructor at line 81 also rejects missing/Idle status and null image buffers. Thirteen shared mailbox/policy regressions pass on Linux. Native CoreMedia/ScreenCaptureKit/CoreVideo calls were source-reviewed and are not compiled by these Linux tests.

This interpretation agrees with Apple's description of [Idle](https://developer.apple.com/documentation/screencapturekit/scframestatus/idle?language=objc) and its [capture example](https://developer.apple.com/documentation/screencapturekit/capturing-screen-content-in-macos?language=objc), which validates explicit status attachments before processing. It does not establish actual behavior on a macOS desktop.

### Medium, resolved: hardware zones duplicated bulb region storage and work

The original sampling plan cloned one identical bulb weight vector per output zone and averaged it repeatedly. My valid synthetic stress fixture used eight bulbs, 256 zones each, centers `(0.5, 0.5)`, radius `1.0`, mock routes and a 160×90 frame. Before repair it compiled in 121.473 ms, sampled in 30.064 ms/frame over 30 iterations and reached 453.52 MiB process peak RSS.

[core.rs](../../src/core.rs), `LightRegions` at line 238 and `SamplingPlan::sample` at line 337, now stores one `Uniform` region per bulb, averages it once and broadcasts the color to its hardware zones. Strip regions remain ordered. The mixed bulb/strip regression at line 560 checks color, light/zone ordering, output count and the invalid-frame black shape. A final independently relinked release probe measured 1.500 ms compilation, 0.302 ms/frame and 13.41 MiB peak RSS with the same eight-bulb fixture. An earlier post-fix run measured 0.288 ms/frame; these short runs vary and are not general performance guarantees. The separate architecture review's preserved 64-bulb fixture is a different workload.

### Low, open and documented: broad strips can exceed a small resource budget

`LightRegions::Ordered` retains and averages separate weights for each strip zone. Input caps therefore do not guarantee a small sampling plan or a configured frame rate for every valid layout. My final release probe used eight straight strips, each with 256 zones, points `(0.1, 0.5)` and `(0.9, 0.5)`, radius `1.0` and mock routes. It compiled in 316.526 ms, sampled in 27.590 ms/frame and reached 384.93 MiB peak RSS. An earlier rerun measured 25.633 ms/frame. This is deliberately extreme geometry at the supported total-zone cap, rather than the typical/default workload.

The cause is visible in [core.rs](../../src/core.rs), the strip region construction around line 292 and ordered averaging around line 338. [Known issues](../KNOWN_ISSUES.md) now explicitly describes the limit and extent/zone/frame-rate adjustments. If predictable costs for these extreme layouts become a requirement, estimate the retained weight count before allocating and enforce an explicit budget, or introduce a measured compact representation. No further optimization is required for acceptance of the present scope.

### Low, open: exact placement and Unicode editing have small UX edges

Coincident canvas handles can favor another light at equal distance in [panels.rs](../../src/app/panels.rs), the nearest-handle selection around line 619. The list and numerical coordinates provide a recovery path; preferring the currently selected light for equal-distance hits would make selection more predictable. The same file limits a light name to 256 characters at line 175, while [core.rs](../../src/core.rs) validates 256 bytes at line 106. A long multibyte name can therefore remain an invalid draft until shortened; the validation error is visible. A UTF-8 byte-aware edit limit or explicit remaining-byte feedback would improve this edge. Neither finding threatens persisted valid layouts or warrants a redesign.

## Scope and implementation assessment

The implementation coherently covers single-source scap capture, native egui placement of bulbs and single/addressable strips, linear-light sampling and smoothing, WLED JSON inspection plus DDP output, optional HA WebSocket ambient control, persistent credential-free layouts and explicit lifecycle/status handling. Wayland delegates source choice to a single-source portal session. Windows/macOS paths exist in source. Hue Entertainment, HDR, multiple-source composition, plugins and tray/daemon operation remain deferred. `OutputAdapter` is a capability sketch, while actual extensibility comes from isolated output tasks and frame/Stop/status channels; it should not be represented as an already integrated generic dispatch framework.

The current HA picker is explicit, searchable and virtualized. Unsupported, unavailable and already-added entities remain disabled with explanations; capacity limits are applied before committing additions. Its 1000-entity test inspects actual egui output and bounds painted names to eight while retaining search across the complete list. Filtering still scans the list; bounded painted rows do not imply constant total CPU cost. Clipboard import/export uses the persistence schema, validates before replacing the layout, omits tokens, isolates modal input and records import as one Undo transaction. [app.rs](../../src/app.rs) headless tests from lines 1093–1231 exercise the actual renderer/handlers, including invalid-import atomicity and clipboard output, rather than merely testing helper methods.

Persistence rejects unsupported versions, unknown/repeated fields, documents above 1 MiB and invalid geometry/routes. A secondary schema check closes serde's unit-variant unknown-field exception while preserving its initial duplicate-field checks. Invalid/oversized existing files are preserved for diagnosis, and saves use temporary-file replacement with synchronization. History has count and approximate aggregate 8 MiB bounds, coalesces drag/focus transactions and avoids Undo silently jumping across an unrecorded oversized edit. Imported discovery names are truncated on UTF-8 boundaries. Access tokens are kept out of layout files, snapshots and exported clipboard data; URL validation refuses embedded credentials.

WLED inspection is read-only for device settings and fails closed unless finite timeout and physical indexing prerequisites are verified. Canonical physical identity groups ranges, checks saved identity against reassigned addresses and rejects conflicting/out-of-range mappings. Reinspection preserves verified mappings and refuses shrunken devices. The session uses finite DDP leases, latest-frame keepalives, stale/cancel release, bounded cleanup and visible release errors. Health helper ownership now covers cancel/unwind, IPv6 literals avoid bracketed hostname lookup and retry backoff resets after actual local recovery. UDP send success and HTTP reachability remain separate from confirmed physical output. Optional restoration keeps its original baseline and is explicitly non-atomic.

HA authentication, discovery, capability conversion, state updates, acknowledgements, coalescing and stale-output refusal have loopback mock coverage. Calls are conservatively paced, and RGBW/RGBWW white channels are explicitly zero. A shared credential-operation lease remains held after async timeout/cancellation until the blocking OS operation returns, preventing overlapping calls. These tests inject a credential backend and never invoke the native keyring. Route changes do not infer physical identity or automatically fail over.

Capture/normalization/sampling use a dedicated worker and immutable snapshots. Session identities reject retired output/preview data; live scalar edits preserve smoothing. Cleanup guards handle errors/unwind, and Stop can wake low-FPS pacing. Ordinary silence cannot renew output freshness, while eligible explicit macOS Idle can. The pending portal chooser remains synchronously noncancelable, and another capture worker is refused while it is pending. These behaviors and conservative Linux/Windows unchanged-desktop cutoff limitations are documented rather than hidden behind a success status.

## Independent checks and evidence boundaries

On the final frozen implementation I ran:

| Check | Result |
|---|---|
| `cargo test --offline --locked --lib -- --test-threads=2` | 88 passed, zero failed; 7.76 seconds. Authorized localhost-only mock socket access. |
| `cargo test --offline --locked --manifest-path vendor/scap/Cargo.toml --lib` | 18 passed, zero failed: five image/Linux helpers and thirteen portable mailbox/status/image-state tests. |
| `cargo fmt --all -- --check` | Passed. |
| `cargo fmt --manifest-path vendor/scap/Cargo.toml -- --check` | Passed. |
| `cargo clippy --offline --locked --all-targets -- -D warnings` | Passed. |
| `cargo clippy --offline --locked --manifest-path vendor/scap/Cargo.toml --all-targets -- -D warnings` | Passed. |
| `cargo build --offline --locked --release` | Passed; final independent incremental build completed in 0.14 seconds after the successful 17.24-second lead build. |
| `git diff --check` | Passed. |
| Relinked synthetic resource probes | Passed against the final release library; measurements and exact fixtures above. |

The initial unprivileged 87-test run passed 74 and failed 13 loopback socket binds with `EPERM`. It was not treated as product failure or protocol success. The authorized rerun passed all 87; the final additional regression brought the passing suite to 88. The standalone vendor lint initially exposed eleven inherited lints that application dependency linting did not cover. The [failure log](../evidence/clippy-scap-continuous.log), equivalent map/borrow/tail/if-let repairs and [passing log](../evidence/clippy-scap-continuous-fixed.log) remain explicit. I inspected those changes and independently reran the standalone check.

The lead's current [verification record](../VERIFICATION.md) and evidence logs separately report the representative synthetic benchmark and six-second mock session. I inspected them but do not count them as my own runs. They exclude native capture, editor/window rendering and physical output. Historical native CI for `ae6c0f1` built earlier Linux/Windows/macOS code; it does not establish compilation of the new local macOS FFI changes or current uncommitted UX. Earlier native UI/capture artifacts are historical evidence, not tests of this increment.

Confidence is high for pure validation, sampling, editor state and mocked protocol/lifecycle behavior; moderate for inspected integration boundaries; low for current native/macOS runtime, platform permissions, DPI/accessibility, OS keyring and physical output. No new native UI, real capture, keyring, device or Windows/macOS execution was performed, in accordance with the task constraint. No critical/high issue was observed, and no platform failure is inferred from the deliberate verification gaps.

## Resource-probe reproducibility

The `/tmp/dartmoor-qa-max-sampling.rs` probe used this code; the strip probe changed only the shape and printed label as described above. The frame and sampling APIs are the actual public application implementations.

```rust
use lumen_desktop::{capture, config::Config,
    core::{Light, Point, Route, Shape, SamplingPlan}};
use std::time::Instant;
fn main() {
    let lights = (0..8).map(|i| Light {
        id: format!("max-bulb-{i}"), name: format!("Bulb {i}"), zones: 256,
        shape: Shape::Bulb { center: Point { x: 0.5, y: 0.5 }, radius: 1.0 },
        route: Route::Mock,
    }).collect();
    let config = Config { lights, ..Config::default() };
    config.validate().unwrap();
    let frame = capture::synthetic_frame(0.0);
    let start = Instant::now();
    let plan = SamplingPlan::compile(&config.lights, frame.width, frame.height).unwrap();
    println!("compile {:.3}ms", start.elapsed().as_secs_f64() * 1000.0);
    let start = Instant::now();
    for _ in 0..30 { std::hint::black_box(plan.sample(&frame)); }
    println!("sample {:.3}ms/frame", start.elapsed().as_secs_f64() * 1000.0 / 30.0);
}
```

Each fixture was compiled separately with `rustc --edition=2024 -O`, the release `liblumen_desktop-edcbcf0f5242ad41.rlib` and `-L dependency=target/release/deps`, then run separately. Python `subprocess.run(..., check=True)` and `resource.getrusage(resource.RUSAGE_CHILDREN)` recorded each child process's Linux peak RSS. Measurements exclude smoothing, capture, UI and networking, and 30 iterations are too short for a general throughput guarantee.
