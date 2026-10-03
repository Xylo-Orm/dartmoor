# Verification evidence

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

Final quality: independent architecture8.2/10, QA8.1/10; overall8.1/10. One revision attempt used. No unresolved critical/high finding; platform, hardware, full interaction and final real-capture performance gaps remain explicit.
