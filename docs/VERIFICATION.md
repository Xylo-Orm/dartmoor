# Verification evidence

## Native CI repair — 2026-10-05

The published editor/lifecycle batch `13a7681` failed [run37238750940](https://github.com/Xylo-Orm/dartmoor/actions/runs/37238750940) on all three runners. Formatting passed, but strict Clippy rejected seven unsuffixed float literals passed to `Stroke::new` in `src/app.rs` and `src/app/panels.rs` (`float-literal-f32-fallback`). Tests and release builds were skipped, not failed. CI used Rust1.99.0 while earlier local checks used Rust1.96.0. macOS also printed inherited `objc` macro warnings; those were not the failing diagnostics. Original job/step conclusions and fatal diagnostics are preserved in `evidence/ci-13a7681-failure.json` and `.log`.

The repair specifies `f32` for those seven stroke widths without changing their values or suppressing warnings. A shared `rust-toolchain.toml` pins Rust1.99.0 with rustfmt/Clippy; the native workflow explicitly installs the file-selected toolchain, then reports it, instead of independently selecting moving stable. Rustup's toolchain-file behavior is documented in [its primary documentation](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file). Linux CI now also runs standalone vendored capture formatting and strict lint; all platforms retain the helper tests. Cargo dependencies and lockfile are unchanged. Rust1.88 remains the declared minimum, not a separately tested compiler claim.

Local Linux checks on Rust1.99.0 (`b940084d7`, x86_64) pass:

- Application all-target tests:109 passed,0 failed; `evidence/tests-ci-repair.log`. Network tests use localhost mocks only.
- Vendored scap library tests:18 passed,0 failed; `evidence/scap-ci-repair.log`.
- Application and standalone vendored all-target strict Clippy: passed; `evidence/clippy-ci-repair.log`, `evidence/scap-clippy-ci-repair.log`.
- Both formatting scopes and `git diff --check`: passed.
- Linux release bins/examples: passed; `evidence/build-ci-repair.log`.

Two independent GPT-6 Astra reviews accept the corrected repair at architecture9.1/10 and QA9.5/10 (overall scoped9.1/10). One corrective pass automated Linux vendor checks and replaced implicit installation through `rustup show` with an explicit installation command; see `evidence/install-ci-repair.log`, `evidence/toolchain-ci-repair.log` and [review history](REVIEW.md#published-native-ci-repair--2026-10-05). QA also independently proved installation in an empty temporary rustup home. The new native CI results will be assessed after publication. These checks add no native UI, real screen capture, physical-light, keyring or performance measurements. Earlier results below are historical and retain the compiler/commit scope stated at the time.

Recorded 2026-10-03 on Linux x86_64, Rust1.96.0, PipeWire1.6.8, Hyprland Wayland, Intel i7-12700KF (20 logical CPUs), ~32GiB RAM. Tests use simulated images and loopback mock devices unless explicitly stated. Raw evidence files are in `docs/evidence/`.

## Initial implementation

- Linux native compile/release build: passed. Application strict Clippy and formatting: passed (upstream vendored scap emits inherited warnings).
- Application tests:27 passed, including geometry/regions/reversal, linear averaging/smoothing, config roundtrip and backup, socket DDP and mock HA authentication/service capabilities/coalescing, WLED stop/acquire failure and MAC validation, engine permission/connection failure/recovery. Loopback sockets required sandbox elevation; the initial unprivileged run's permission failures were not hidden.
- Vendored scap:5 tests passed including buffer layout/stride checks. Windows/macOS runtime not verified. Native CI matrix added but not executed in this workspace.
- Native UI automated smoke: synthetic source and mock strip; window opened, minimized/restored, then closed. Frame counts120/240/360 at4/8/12s establish synchronization independence from repaint. This is automated UI lifecycle, not a complete manual interaction walkthrough.
- Real capture: Hyprland portal delivered1192 frames after the chooser delay during a30s probe, mock output only, exit0. No screenshot contents saved. Requested30fps but upstream produced ~60fps; lead added a processing cadence cap and color decode lookup before final builds. GNOME, KDE and other wlroots runtimes remain untested.
- Physical-light tests: unavailable. No supplied WLED hardware or HA credentials/server; mocks prove protocol construction and lifecycle, not delivery or visual correctness on bulbs.

## Initial measured workload

Measurements use `scripts/measure.py` (/proc sampling250ms plus child resource usage), release builds. Include startup/cleanup unless explicitly marked. Percent CPU means one logical CPU (CPU seconds / wall seconds ×100), not the whole20-core machine. No GPU usage measurement.

- Synthetic headless default8-zone strip: see raw demo evidence/final measurements.
- Native synthetic UI12s: user0.164s/system0.238s (~3.2% one core including startup/cleanup), maxRSS166.4MiB. Stable visible phase1–4s used about0.04 CPU seconds (~1.3%); minimized4.25–8s about0.04s (~1.1%); restored8.5–12s about0.03s (~0.9%). Sample resolution limits these estimates. Frame cadence30fps while visible and minimized. This does not measure real capture with UI.
- Real capture CLI30s including ~10s chooser wait: user8.576s/system0.027s, maxRSS79.4MiB; active steady phase consumed roughly43% one core at~60fps before the cap/lookup fix. Sampling alone around0.20ms/frame, normalization/capture dominate. Final optimization evidence must be measured separately; do not present pre-fix numbers as final performance.
- CPU synthetic sampling benchmark:160×90 image,16 strips/968 zones,3000 iterations,0.2763ms/iteration, maxRSS13.6MiB. Includes sampling/smoothing; excludes capture/network/editor. Actual output cadence on hardware is unverified.

## Confidence limits

UI interaction coverage, keyring backend availability and native platform runtime require hands-on follow-up. macOS grants may require app restart; portal prompts may recur; Windows unsupported/protected content can fail. Network update responsiveness on actual devices and restoration races require hardware tests. No claims of cross-platform runtime certification or exclusive device control.

## Revision1 final verification

- **Linux release build:** passed with the final native editor and adapters; dependency lockfile included. Formatting and strict Clippy pass. Vendor warnings were cleaned in the targeted patch.
- **Automated tests:**36 application tests and5scap tests passed in independent re-review. The saved `tests-revision1.log` is the earlier35-test integration run; the two independent revision reports record the final36-test runs. Added canonical-controller aggregation, finite DDP acquisition/keepalives/latest/stale/cancel, timeout policy, blocking credential isolation, and single-zone HA regressions. See both independent revision reports. One concurrent test process could not bind fixed UDP4048; its isolated rerun passed, documented rather than counted as a product failure.
- **Windows compilation:** vendored scap cross-check for `x86_64-pc-windows-gnu` passed. Full application cross-check is blocked by absent MinGW C compiler needed for `ring`. The UI name-resolution defect found by initial review is corrected in source. No native Windows or macOS build/run is claimed. CI is prepared, not executed here.
- **Native rendering/visual QA:** compositor capture restricted to the focused simulated Lumen window was visually inspected. Preview gradient, numbered strip handles, settings, lifecycle toolbar and status render without clipping in the tested1244×1416 tiled window. Artifact:`evidence/ui-window.png`. The built-in eframe screenshot captured a blank post-swap buffer and was not used as UI evidence. No complete manual drag/discovery/hardware workflow is claimed.
- **Real screen capture:** the earlier Hyprland portal CLI probe remains the real-capture success evidence. A later30s real-capture UI smoke produced0frames because the chooser path did not complete; that run is recorded as **unverified capture**, despite the UI process exiting0. Its CPU/RSS numbers are not synchronization performance and are excluded. Final optimized real-capture CPU/latency is therefore unverified.
- **Physical lights and real HA:** not tested; devices/credentials were unavailable. No simulated result is represented as hardware verification.

Final available synthetic measurements (`demo-optimized.log`, `benchmark-optimized.log`): default8-zone mock strip,30fps,8s produced240frames; user0.0636s/system0.0051s (~0.83% of one logical CPU including process startup/cleanup), stable sampledRSS6.31MiB and peak childRSS13.90MiB. Sample/encode processing about0.20ms/frame. The16-strip/968-zone benchmark measured0.2820ms/iteration across3000iterations. These figures isolate synthetic capture and sampling; they do not include physical output latency. Earlier visible/minimized synthetic UI evidence remains120/240/360frames and~166MiB peakRSS, with phase estimates above. Local DDP regression observes12successive keepalives within650ms and newest-color replacement, with a33msconfigured cadence; no hardware cadence is claimed.

Revision1 quality: independent architecture8.2/10, QA8.1/10; overall8.1/10. One revision attempt used. No unresolved critical/high finding; platform, hardware, full interaction and final real-capture performance gaps remain explicit.


## Revision2 final verification — 2026-10-04

This follow-up deliberately used only Linux compilation and pure, synthetic or loopback-mock checks. No real capture, native UI interaction, native keyring, physical-light, Windows or macOS testing was performed. Historical measurements above were not rerun and do not measure the new revision.

- **Linux release build:** passed with `cargo build --offline --locked --release`; see `evidence/build-revision2.log`. Lockfile retained.
- **Formatting and strict lint:** `cargo fmt --check` and `cargo clippy --offline --all-targets -- -D warnings` passed.
- **Automated application tests:**49 passed,0 failed; see `evidence/tests-revision2.log`. The lead's initial sandbox run passed37 tests and failed10 mock socket binds with permission errors. The authorized localhost-only rerun passed all49 after the final regressions landed. Both independent reviewers also ran passing final49-test suites.
- **Capture-library unit tests:**5 passed; only packed-buffer/layout helper tests, not actual screen capture.
- **New coverage:** stale inspection results, imported names and capacity failures, source selection, preview identities, capture cleanup on error, interruptible1fps pacing, in-flight configuration edits and preserved smoothing, credential timeout/cancellation exclusion, HA refresh invalidation, WLED physical mapping/DMX/sequence/override policy, first-frame restoration race, and visible failed release after mock disconnection.
- **Manual UI / real screen capture / physical lights / Windows/macOS:** not tested in this revision, as requested. No updated performance claim is made.

Independent final architecture score8.4/10 and QA score8.3/10; overall8.3/10 (lower score). Two of three permitted revision attempts used. No unresolved critical/high finding. Native runtime/hardware evidence, compositor differences, noncancelable OS credential calls, and non-atomic WLED restoration remain documented limits.


## Native CI and local UX follow-up — 2026-10-04

The original strict-compiler failure at `src/app.rs:501` was fixed with explicit f32 stroke widths in commit `ae6c0f1`. [GitHub Actions run37187173038](https://github.com/Xylo-Orm/dartmoor/actions/runs/37187173038) passed formatting, strict Clippy, application tests, vendored helper tests and release builds on Ubuntu24.04, Windows and macOS14. It uploaded release artifacts. `evidence/ci-ae6c0f1.json` records job/step conclusions. These are native CI build/test results for that committed version, not screen-capture or physical-light runtime tests.

The subsequent UI/UX increment is **local and uncommitted**, as requested. It adds responsive setup/properties panels, color-zone/direction overlays, precise placement, point insertion/removal, keyboard movement, bounded undo/redo, dirty/save/validation feedback and guarded async discovery. Verification of these local changes:

- **61 application tests** passed; `evidence/tests-ux.log`. Of these, 8 pure editor tests cover history limits/redo, aspect-aware zones, reversal, overlapping-segment insertion, removal and clamped movement. Four headless interaction tests exercise actual egui layout/input handlers: small-window bounds and preservation of valid saved values, nudge/undo/redo and focused Save, one-transaction dragging, and busy/stale-server discovery handling. Existing output tests use only loopback mocks.
- Formatting and strict Clippy passed. Local Linux release build result is recorded in `evidence/build-ux.log`.
- Two independent reviews accepted the increment at 8.4 architecture and 8.5 UX; lower overall8.4/10. Reviewers did not author the code they assessed.
- No native window, real capture, credentials or hardware were tested in this increment. Headless layout assertions do not establish visual correctness, DPI behavior or accessibility. The local UX changes were not built on Windows/macOS or sent to CI; the earlier successful native CI remains evidence for `ae6c0f1` only. No new performance measurements or low-overhead claim are made.


## Continuous improvements — 2026-10-04

This increment follows the user's request to keep improving the project beyond the accepted original revision loop. It remains local and uncommitted. No new native window, real screen capture, OS credential store, physical device, Windows or macOS runtime was tested.

- **Linux compile:** release build passed (`evidence/build-continuous.log`). Formatting and strict Clippy passed. Dependencies and Cargo.lock are unchanged.
- **Automated application tests:** 88 passed, zero failed (`evidence/tests-continuous.log`). New coverage includes strict clipboard-layout interchange and unit-variant unknown-field rejection, invalid-import atomicity, token omission, modal input isolation, import Undo/Redo, selective HA discovery, rendering a 1000-light list with bounded visible rows, history memory limits, preserved WLED mappings, IPv6 address construction, WLED health-task cancellation/panic and retry recovery, explicit Idle liveness, silence cutoff and cached-image cleanup. Tests use synthetic frames, headless egui input and loopback mock endpoints.
- **Capture helper tests:** eighteen vendored tests passed (`evidence/scap-continuous.log`): five image/layout tests and thirteen shared mailbox/frame-state tests. The latter compile the actual pure queue/eligibility policy on Linux and cover protected latest images, invalidation, missing-status rejection, failed conversions, backend errors, timeout/disconnection and wakeup/drop behavior. The macOS source changes were inspected against local CoreVideo bindings and Apple documentation. They are not compiled by a Linux build; native macOS compilation/runtime remains unverified for this increment.
- **Synthetic sampling benchmark:** same Intel i7-12700KF Linux host, release build, 160×90 SDR image, 16 strips / 968 zones, 3000 iterations: 0.2970 ms per sampling+smoothing iteration, 14.0 MiB peak RSS (`evidence/benchmark-continuous.log`). This excludes capture, networking and editor rendering.
- **Synthetic headless session:** default 8-zone mock strip at 30 FPS, six seconds: 181 processed frames, sampled processing around 0.204–0.214 ms, 0.048831 user / 0.003844 system CPU seconds over 6.254 seconds including startup and cleanup (about 0.84% of one logical CPU), 13.9 MiB peak RSS (`evidence/demo-continuous.log`). Mock output sends no commands to devices.
- **Reviewer stress measurement:** an intentionally extreme valid 64-bulb / 2048-zone / radius-1.0 layout originally took 30.06 ms per sample and retained about 453 MiB peak process RSS. Computing each bulb region once reduced the independent probe to 1.47 ms per sample and about 17.6 MiB RSS; compile time fell from 122.84 to 6.94 ms. Timings use only three synthetic iterations and exclude smoothing, capture, editor and networking; see `evidence/stress-continuous.log` and its preserved source. Broad strip layouts can still be expensive at the supported caps, as documented.
- **Vendor standalone lint:** explicit `cargo clippy --offline --manifest-path vendor/scap/Cargo.toml --all-targets -- -D warnings` initially exposed eleven inherited style lints that application dependency linting did not cover (`evidence/clippy-scap-continuous.log`). Small equivalent map/borrow/tail/if-let cleanups fixed them; the same standalone check passed (`evidence/clippy-scap-continuous-fixed.log`). Vendor formatting also passed.
- **Visible/hidden editor performance:** not remeasured. Historical native UI measurements above concern older code; these headless measurements do not establish current windowing/GL memory, idle/minimized behavior, real capture cost or device cadence. No general low-overhead claim is made.

Two requested GPT-6 Astra review agents hit their model usage limit before returning a result; their failed turns are not counted as completed reviews. Independent GPT-6.1 Sol reviewers performed the available fallback reviews. Their reports and final disposition are recorded separately in the review log.

Final independent architecture score **8.4/10** and QA score **8.5/10**; overall **8.4/10**. Both reviewers accepted the coherent implementation with no unresolved critical/high/medium implementation blocker. Their reports independently rerun 88 application and 18 vendor tests, both packages' formatting/strict Clippy, Linux release and diff validation. See [review log](REVIEW.md). One corrective pass was used in this separately requested follow-up; the original accepted loop remains three of three revisions.


## Local fixes and strip presets — 2026-10-04

This follow-up addresses the accepted review’s remaining actionable issues and adds edge/perimeter placement presets. All changes are local; no commit or push, native window, desktop capture, physical light, OS credential store, macOS or Windows testing is included. Earlier native CI applies only to ae6c0f1.

- Compact sampling represents weight-one pixel runs and sparse feather pixels, building one f64 linear-sRGB row-prefix table per frame. Dense independent-reference tests cover dimensions, aspect ratios, edges/corners, crossing/repeated paths, tiny/large radii, single-color and ordered regions, and reversal. The maximum-zone resource test bounds retained region storage below 4 MiB.
- An isolated release before/after probe on the same Intel i7-12700KF Linux host used synthetic 160×90 SDR frames and eight lights ×256 zones. Broad strips improved from 26.747 to 0.301 ms/frame; process-local VmHWM fell from 394,484 to 6,092 KiB. Final compile time was 218.782 ms versus 314.618 ms. Narrow crossing paths improved from 0.277 to 0.061 ms/frame; bulbs from 0.283 to 0.013 ms/frame. Broad timing used120 samples, narrow/bulbs1000, plus five warmups. This isolates core sampling, excludes smoothing, UI, capture/networking and devices, and does not predict general application CPU. Raw agent evidence: `evidence/sampling-runs-local-fixes.log`; current-workload reproduction: `cargo run --release --example sampling_stress -- broad` (or narrow/bulbs).
- Comparing150,624 before/after zone colors found maximum linear difference0.000211477 and encoded difference1/255 from improved summation precision; the new implementation agrees with a separate dense f64 reference within1e-6.
- Headless egui interaction regressions verify selected coincident markers remain selected and independently draggable, simple clicks clear drag state, Unicode paste stays valid and Undo restores the original, and perimeter presets preserve hardware/sampling settings as one Undo transaction. Pure preset tests cover direction, closure, normalized bounds and extreme inset inputs. These do not establish native rendering, DPI or accessibility.
- Capture-thread creation now returns a recoverable session error before output tasks acquire resources; an injected OS-spawn failure followed by a successful Start exercises cleanup and recovery. Busy and closed command-queue errors are separately tested. Application networking-runtime construction also propagates errors instead of panicking. WLED/HA task helpers retain their existing protocol, timing and ownership behavior.

- **Integration verification:**101 application tests (all targets),18 vendored helper tests, both package formatting and strict all-target Clippy, Linux release bins/examples and diff whitespace checks passed. Logs: `evidence/tests-local-fixes.log`, `evidence/scap-local-fixes.log`, `evidence/clippy-local-fixes.log`, `evidence/build-local-fixes.log`. No lockfile/dependency change.
- **Current application release measurements:** the repository example’s broad-strip workload measured220.658 ms compilation /0.294 ms per sample, with process-local VmHWM6172 KiB (`evidence/stress-local-fixes.log`). The application’s standard16-strip/968-zone sampling+smoothing benchmark measured0.0533 ms per iteration across3000 (`evidence/benchmark-local-fixes.log`). A six-second synthetic/mock session produced181 frames at30FPS, reporting0.015–0.028 ms processing,0.008760 user +0.010513 system CPU seconds over6.254 seconds including startup/cleanup (~0.31% of one logical CPU), and sampled steady RSS6.97MiB (`evidence/demo-local-fixes.log`). The measurement wrapper’s child-resource peak around13.9MiB includes fork/exec overhead and is not the process-local VmHWM value. Visible/hidden editor performance was not remeasured.

Initial independent reviews scored architecture8.5 and QA8.6; the correctness finding and subsequent corrective review history are recorded below and in the review reports.


### Corrective pass1

Independent reviews reproduced a valid near-zero strip whose distinct normalized f32 endpoints become identical after pixel scaling. The empty sliced path previously sampled pixel0. Preserving the first transformed point fixes that placement; a nonuniform-frame regression checks the expected local color for two rounding fixtures, single/addressable zone counts and both directions, and exercises the dense oracle without an empty-path panic. The original reproduction source is preserved at `evidence/local-fixes-collapse-probe.rs`.

The same pass bounds WLED inspection, configuration, state and live/restore acknowledgement reads by advertised and incremental body size, gives HA explicit transport and retained-event/discovery metadata budgets, moves large JSON parsing off the Tokio executor without copying an entire owned message, and takes HA result values instead of cloning them. Invalid light state identities/metadata fail the current HA stream before further stale-capability control. See configuration documentation for exact limits and large-installation tradeoffs.

Corrective integration checks pass:107 application tests/all targets, formatting, strict all-target Clippy, Linux release bins/examples and whitespace validation (`evidence/tests-local-fixes-revision1.log`, `evidence/clippy-local-fixes-revision1.log`, `evidence/build-local-fixes-revision1.log`). The five new ingress regressions use small helper budgets and localhost endpoints to prove advertised and chunked overflow rejection before body completion, oversized WebSocket header rejection without a body, normal reconnection, retained event accounting across requests, metadata/cache caps and safe parser errors. The18 vendor helpers/standalone checks passed on the unchanged vendor tree earlier in this batch. No final native window/real capture/hardware measurements were added. The correction1 reviews resolved both initial findings but held acceptance on the newly introduced cancellation race described below.


### Corrective pass2

Re-review found an introduced HA scheduling race that ordinary107-test coverage missed: offloaded JSON parsing yielded after consuming a message, so a cadence tick could discard an availability update. QA’s deterministic held-blocking-worker probe reproduced the actual read cancellation boundary; architecture independently confirmed it. The steady loop now selects raw message receipt first, then finishes committed decoding without timer cancellation. Stop still ends the entire connection promptly. Queued parser jobs abort when their await owner is canceled; already-running bounded parses cannot be interrupted by Tokio. The final109-test application suite passes (all targets), along with formatting, strict all-target Clippy, Linux release bins/examples and whitespace checks. Two new deterministic regressions use the actual HA stream: they acknowledge a baseline color, hold a large unavailable or unsupported on/off-capability event behind one blocking worker, change the target color and advance cadence, then verify no stale service; Stop completes before releasing the held worker. A scoped notification is compiled only in unit tests. A separate queued-parser test proves canceled jobs do not execute and release their owned buffers. The negative control restoring the old timer/read selection fails with the expected stale-state call and exits101, rather than hanging; exact mutation/log/command are preserved at `evidence/ha-cadence-negative-control.*` and `evidence/ha-cadence-negative-control-command.txt`. Final passing logs are `evidence/tests-local-fixes-revision2.log`, `evidence/clippy-local-fixes-revision2.log` and `evidence/build-local-fixes-revision2.log`. The unchanged vendor tree has18 passing helper tests and both vendor format/lint checks in this batch. No native window/capture/device/keyring/Windows/macOS test or new performance claim is included. Final independent architecture8.6/10 and QA8.7/10 accept the batch; overall8.6/10, with two of three corrective attempts used. Reports and exact provenance are in [the review log](REVIEW.md). No unresolved critical/high/medium implementation blocker remains; native/hardware confidence limits above remain explicit. No commit or push was performed.
## Window capture performance adjustment

The Linux backend now requests bounded fixed and variable capture rates,
reserves queue capacity before processing pixels, and reduces borrowed mapped
rows directly to at most 160x90 RGB. The reduction regression compares packed
and padded buffers across all negotiated channel orders and verifies identical
output through worker normalization. Vendor tests cover FPS bounds, jitter and
stall handling, lazy slot reservation and disconnect/teardown behavior.

Validation: 118 application tests pass (all targets), with one intentionally
ignored release benchmark; 23 vendored scap tests pass. Strict all-target
Clippy, vendor Clippy, formatting, whitespace validation and release build pass.
The release benchmark over 100 3840x2160 frames measured 2.796 ms/frame for a
full-buffer copy plus reduction versus 0.798 ms/frame for mapped reduction and
worker normalization. Transferred pixels fall from 33,177,600 to 43,200 bytes
per frame. These measurements exclude compositor capture/GPU readback and are
not an end-to-end application latency claim. Reproduce with:

```sh
cargo test --release --lib benchmark_mapped_capture_reduction -- --ignored --nocapture
```

A live Fedora/GNOME/Wayland portal probe captured a user-selected window at
5120x2880 BGRx, negotiated variable FPS (0/1) with maximum 30/1, and produced
367 frames in about 15 seconds. Incoming/delivered rate was approximately
24 FPS; buffer processing averaged 0.83–0.94 ms/frame. The user confirmed that
window capture stuttering improved. This confirms the selected-window path and
an observed improvement, without proving that all compositor/driver stuttering
is eliminated. The probe now starts its duration at the first frame and allows
up to 120 seconds for portal selection. Output used mock lights throughout.

## Black bar detection

The saved Synchronization checkbox enables conservative detection of opposing
letterbox/pillarbox bars on reduced capture frames. Preview and sampling share
the resulting active picture. Stable crops require at least three observations
over 350 ms; dark scenes retain the established crop, resizing resets it, and
disabling restores the full image on the next processed frame.

Seven new regressions cover horizontal/vertical/combined bars, transient bars,
full-picture restoration, dark scenes, disabling/re-enabling, asymmetric/deep
edge rejection, resizing, old-layout defaults, enabled-setting persistence,
the actual UI checkbox, and live worker sampling/preview changes. The worker
test samples a light placed within the original top bar: enabled detection
produces the content's red color and a cropped preview; disabling produces
black and the full preview; re-enabling returns to red after confirmation.
All 125 application tests pass, with one intentionally ignored release capture
benchmark. Formatting, strict all-target Clippy, whitespace validation and
Linux release build pass. These feature checks use constructed frames and
headless UI; no additional native capture/device session was needed or tested.


## Soft ambience — 2026-10-05

Implemented on top of the existing uncommitted per-segment LED allocation,
Linux mapped-buffer downsampling and black-bar detection work, preserving those
changes. The new saved controls use strict defaulted decoding, live Apply and
existing configuration history. No capture or vendor implementation was changed
for this feature, and no commit or push was made.

Verification on this Linux host used Fedora Rust 1.98.1 and matching staged
rustfmt/Clippy binaries at `/tmp/dartmoor-rust-tools/usr/bin` (the repository pins
1.99.0; that rustup toolchain was unavailable here). Final results:

- `cargo test --locked`: **133 passed**, one intentionally ignored capture
  benchmark; main and doc-test targets passed. The initial sandbox run passed
  non-network tests but denied 17 localhost socket tests. The complete final run
  passed with sandbox escalation for mock-device sockets.
- `cargo test --offline --manifest-path vendor/scap/Cargo.toml --lib`:
  **23 passed**. An initial standalone attempt with `--locked` failed: first
  registry access was blocked, then the authorized retry reported no standalone
  vendor lockfile. The offline command generated its ignored local lockfile and
  passed using cached dependencies; the application lockfile is unchanged.
- `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`,
  `git diff --check`, and `cargo build --locked --release --bins --examples`: passed.
- Eight added tests cover exact disabled/zero-strength bypass, uniform neutrals
  and colors, substantial color emphasis, tiny outliers, continuous competing
  hues, a direct pixel-weight oracle, linear strength blending, luminance/gamut
  constraints, spatial zones/reversal/bulb broadcast, defaults/validation/disk
  persistence/layout interchange, headless checkbox/sliders/history, and live
  cropped-picture updates without reopening capture. Existing mapping, smoothing,
  routing, black-bar and backend regression tests remain included.

Release sampling probe: `target/release/examples/sampling_stress` with modes
`typical`, `broad`, `narrow`, `bulbs`. Each uses a deterministic mixed-color
160×90 SDR frame, five warmups and 1,000 measured iterations per path; enhanced
uses enabled mode with default strength/emphasis/vibrancy. The accurate measurement
uses the same public entry point with the mode disabled. Times include sampling
allocations and color production. A final sequential run after builds finished:

| Workload | Accurate ms/frame | Enhanced ms/frame | Added ms/frame | Ratio |
|---|---:|---:|---:|---:|
| One three-sided strip, 60 zones, radius 0.04 | 0.014 | 0.053 | 0.039 | 3.8× |
| Eight broad strips, 2,048 zones, radius 1.0 | 0.366 | 1.024 | 0.658 | 2.8× |
| Eight crossing narrow strips, 2,048 zones, radius 0.035 | 0.057 | 0.428 | 0.371 | 7.5× |
| Eight broad bulbs broadcasting 2,048 zones | 0.014 | 0.269 | 0.255 | 19.2× |

These are local sampling measurements, not end-to-end synchronization performance.
Rounded ratios are particularly sensitive to small baselines. Earlier repeat runs
were similar (broad enhanced 1.021–1.098 ms/frame). Broad plan compilation took
245.880 ms in the final run; enhancement settings do not recompile geometry.
Observed process high-water RSS for these probes was about 3.6–6.3 MiB, including
both paths, plan and process runtime; this is not an incremental memory budget or
native UI RSS claim. Enhanced sampling uses one additional bounded row-prefix
representation and the accurate output colors, avoiding per-zone image scans.

The method favors colorful area with bounded continuous influence, then applies
linear-sRGB luminance-preserving gamut-limited saturation and a linear-light blend.
It does not choose a winning hue; balanced opposing hues can still cancel, and
capture downsampling can already have lost tiny details. Color weighting can
change mean luminance. See [configuration](configuration.md#soft-ambience).

The rebuilt executable is
`/home/vladimir/Projects/Developer/dartmoor/target/release/lumen-desktop`.
Headless/synthetic and localhost mocks only: no new native UI, real desktop
capture, physical WLED/HA lights, keyring or Windows/macOS runtime verification.

## Dark zones and Music Pulse — 2026-10-06

Implemented saved per-zone black-to-off gating with sensitivity, and a separate
Music workspace with Linux PipeWire playback-monitor capture, bounded bass-onset
analysis, a uniform Pulse palette, live effect controls and explicit source
handover. Existing Soft ambience, crop, per-segment mapping and mapped-buffer
video downsampling remain in place. The application directly uses the already
locked PipeWire 0.10.1 dependency; no capture/vendor implementation was changed
for the music backend.

Final checks used Fedora Rust 1.98.1 and matching staged rustfmt/Clippy; the
repository toolchain pin is 1.99.0, which is unavailable on this host:

- Application tests: **148 passed, 0 failed, 1 intentionally ignored** capture
  benchmark. Localhost mock tests ran with authorized socket access.
- Vendored backend tests: **23 passed**.
- Formatting, strict all-target Clippy (`-D warnings`), whitespace checks and
  release build of binaries/examples: passed.
- Added coverage includes independent dark regions, confirmation/hysteresis,
  live sensitivity/disable changes, smoothing-history clearing, Soft ambience
  and active-picture cropping, strict defaults/persistence, conditional controls
  and history, Music navigation versus explicit start, live effects without
  source reopening, source handover, onset/silence/noise/stereo analysis and
  missing-audio failure without fabricated frame freshness.

An initial sandbox run failed localhost socket tests and exposed an immediate
worker-drop assertion racing asynchronous shutdown. The assertion now waits for
bounded eventual cleanup; the final authorized suite above passed. The release
synthetic Music demo produced 105 frames and seven onsets, with approximately
0.010–0.011 ms reported processing per frame. This does not measure native audio
capture, device latency or physical timing.

Repeated release Soft ambience sampling probes measured 0.014 ms accurate versus
0.052 ms enhanced for the typical 60-zone strip (+0.038 ms), and 0.372 versus
1.090 ms for 2,048 broad-strip zones (+0.718 ms). Broad plan compilation measured
243.479 ms. These synthetic sampling-only measurements do not establish dark-gate
costs or end-to-end performance; the earlier broader workload table remains above.

An isolated PipeWire test-server experiment did not establish native playback
success and was discontinued at the user's request. Real desktop audio testing
will be done by the user. No new native UI, real capture, physical WLED/HA,
keyring or Windows/macOS runtime verification is claimed. The user's prior
physical testing used WLED only; Home Assistant coverage remains localhost mocks.

Rebuilt executable:
`/home/vladimir/Projects/Developer/dartmoor/target/release/lumen-desktop`.

## Music sensitivity adjustment — 2026-10-06

Following the user's real desktop-audio feedback, lowered the relative bass-rise
threshold from 3.5–1.5× to 2.1–1.12× over the sensitivity range, with a perceptual
square-root mapping (default 50% now about 1.41× instead of 2.5×). Sensitivity
also lowers absolute bass/quiet gates. A 10 ms bass-energy smoother and positive
rise requirement suppress sustained-tone retriggers. A detected onset holds its
full target for 60 ms, allowing the default 20 ms attack envelope to reach a
visible peak; silence overrides the hold. Existing saved sensitivity values are
retained and use the revised response. Source capture and output routes are
unchanged.

Added regressions cover low-level bass reaching visible output, sub-gate noise
remaining black across sensitivity settings, repeated 30% bass rises over a
sustained bed, a visible brief-beat peak followed by decay, and steady 40–160 Hz
bass without continuing onset triggers at maximum sensitivity. Updated the
existing sensitivity comparison for the new range. The UI test fixture and smoke
mode now reset the workspace when replacing configuration with defaults; real
saved Music mode had exposed their previous dependency on user configuration.

Final authorized localhost-capable application suite: **152 passed, 0 failed,
1 intentionally ignored**. Strict all-target Clippy, formatting, whitespace
checks and release build of binaries/examples passed with Fedora Rust 1.98.1.
The first run exposed the old sensitivity test's threshold assumptions and ten
UI tests inheriting saved Music workspace; both were corrected before the final
passing run. No new isolated audio server, native playback or physical-light
probe was run; real listening/output assessment remains with the user. Existing
Home Assistant verification remains mocks only.

## Advanced Music — 2026-10-06

Implemented the approved multiband/spectral/tempo plan in one saved Advanced
detector, keeping the previously user-tested Classic detector available. The
new mode emphasizes midbass/body attacks, separates upper-band accents, follows
confident tempo/phase estimates, and falls back to direct reactions. Added live
Subbass weight, Sparkle amount and Follow beat controls, band/tracking diagnostics
and logical-zone previews. Bulbs retain broadcast output; single-color strips
receive restrained whole-light accents. Existing video sampling/crop/ambience,
segment routing and mapped-buffer video capture were preserved; this increment
changes no capture/vendor implementation.

Music polling/analysis now runs independently of 30 Hz color output. Audio sample
counts drive event timing; coalesced PipeWire drop/format-change signals reset
continuity. Explicit idle remains distinguishable from timeouts. Spectral work
uses fixed 2,048-point stereo transforms, reusable scratch/history and precomputed
weights; event histories are capped at 96 main and 96 upper-band entries. RustFFT
6.4.1 plus three supporting packages were added to the application lockfile;
existing locked dependency versions were not upgraded. The first sandbox fetch
failed DNS resolution; the authorized dependency fetch succeeded.

Final Linux checks used Fedora Rust 1.98.1 with matching staged rustfmt/Clippy;
the repository 1.99.0 pin remains unavailable on this host:

- `cargo test --locked` with authorized localhost socket access:
  **165 passed, 0 failed, 2 ignored**. The ignored tests are the existing native
  capture benchmark and the new explicitly invoked release Music benchmark.
- `cargo test --offline --manifest-path vendor/scap/Cargo.toml --lib`:
  **23 passed**.
- Formatting, strict `cargo clippy --locked --all-targets -- -D warnings`,
  `git diff --check`, and `cargo build --locked --release --bins --examples`:
  passed.
- Release Music analysis benchmark invoked separately: **passed**.
- Release CLI `--music-demo-seconds 5`: **166 output frames**, eleven main
  attacks, 120 BPM following by the later snapshots, no stream resets. This
  fixture contains bass pulses without hats, so zero sparkle events are expected.
  The polling loop measures about 5.5 seconds including its initial observation.

Added deterministic coverage checks independent bands/opposite-phase stereo,
quiet input and non-finite samples, steady tones and pitch movement, repeated
midbass attacks with upper subdivisions, compressed bass beds versus sensitivity,
subbass influence, spatial accents/bulb broadcast/zero brightness, chunk-boundary
invariance, detector changes and discontinuities, tempo acquisition at
60/90/120/180 BPM, missing-beat prediction, prediction/confirmed-event merging,
irregular events and hats alone, and tempo-change recovery. A known 120 BPM
fixture matches at least 22 of 24 main attacks within 70 ms after their source
onsets without duplicate main events per beat. This is sample-analysis timing,
not desktop capture or physical-light latency. Settings defaults/ranges/strict
JSON/persistence, conditional controls/history and live engine Apply remain
covered. There is no annotated real-music recognition-accuracy result.

Initial development checks caught spectral leakage from upper attacks into main
pulses and insufficient response to smaller attacks over sustained bass. Energy
share gating and the wider sensitivity range fixed those fixture regressions.
The new timing check, final full suite and strict lint checks passed afterward.

Release benchmark command:
`cargo test --locked --release music_analysis_benchmark -- --ignored --nocapture`.
Each mode warms up with eight seconds of deterministic 180 Hz attacks plus
6.5 kHz accents, then measures 8,000 ten-millisecond hops. Rendering separately
measures 1,000 60-zone outputs and 1,000 sets of eight 256-zone outputs:

| Detector | Mean analysis ms/hop | p95 | p99 | Observed maximum | 60-zone color production ms/frame | 2,048-zone color production ms/frame |
|---|---:|---:|---:|---:|---:|---:|
| Classic | 0.0024 | 0.0024 | 0.0024 | 0.0074 | 0.0001 | 0.0010 |
| Advanced | 0.0213 | 0.0211 | 0.1594 | 0.2344 | 0.0015 | 0.0486 |

Added mean analysis cost is approximately 0.0189 ms per 10 ms hop on this host.
The p99 includes periodic candidate estimation spikes; the observed maximum is
not a universal upper bound. Advanced p95 meets the provisional 2 ms/hop target
for this fixture. These measurements include analyzer work or logical-zone color
production respectively; they exclude capture/event-loop copying, UI, physical
LED expansion, adapters/network scheduling and device latency. They are not an
end-to-end performance or worst-case workload claim.

The user previously verified real desktop playback and WLED response of Classic.
The new Advanced behavior was checked with deterministic signals, headless UI,
CLI demo and localhost mocks only. No new native playback server/probe, physical
WLED/HA, keyring or Windows/macOS runtime test was performed. Home Assistant has
no real-device verification. Listening quality and audio-to-light timing remain
for the user's desktop playback test.

Rebuilt executable:
`/home/vladimir/Projects/Developer/dartmoor/target/release/lumen-desktop`.
