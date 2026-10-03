# Initial independent architecture and correctness review

Reviewed 2026-10-03 by the independent architecture reviewer. This is the initial implementation assessment, before the lead's proposed review fixes. The reviewer did not author or change implementation code. The Windows scap dependency patch was being completed concurrently; this review does not mistake that patch's library cross-check for an application build.

**Score: 7.1/10. Not yet at the requested 8/10 acceptance threshold.** The Linux application has a coherent, working capture/sampling/editor architecture and credible local evidence, but controller ownership and failure cleanup contain material defects, and a non-Linux application branch does not compile. No catastrophic security/data-loss issue was found; the high-severity issues below nevertheless prevent acceptance of the full requested scope.

| Dimension | Weight | Score | Assessment |
|---|---:|---:|---|
| Correctness | 30% | 6.8 | Sound geometry/color core; controller identity and platform defects remain. |
| Architecture | 20% | 7.7 | Good ownership and bounded/latest-value separation; blocking credential access breaks isolation. |
| Resilience | 20% | 6.2 | Deliberate cleanup/recovery paths, undermined by an infinite WLED live lease. |
| Verification | 20% | 7.5 | Reproduced 27 application and 5 vendor tests; native/hardware coverage remains incomplete. |
| Usability | 10% | 7.7 | Functional native editor and candid route/status explanations; limited interaction coverage. |

Weighted total: 7.07, rounded to 7.1. Scores assess delivered behavior, not effort.

## Severity-ranked findings

### High — WLED acquisition disables the timeout relied upon after failed cleanup

Evidence: `src/outputs.rs:152–162` posts `{"live":true}`, called at `src/outputs.rs:412–413`. Cleanup attempts `live:false`, but a failed request can end the output task; `README.md` says an unreachable device may fall back to its realtime timeout.

The firmware semantics contradict that fallback. WLED's [JSON handler](https://github.com/Aircoookie/WLED/blob/main/wled00/json.cpp) calls `realtimeLock(65000)` for this field. Its [realtime implementation](https://github.com/Aircoookie/WLED/blob/main/wled00/udp.cpp) converts 65000 to `UINT32_MAX` and does not replace that value with subsequent finite packet leases. Thus a successful acquisition followed by an application crash, HTTP-only failure, or network outage can leave the controller in realtime indefinitely. This is a source-backed protocol conclusion, not a physical-device test.

Use DDP's finite lease acquisition or another verified finite mechanism; retain bounded explicit release as best effort. Test against a stateful mock that models lease expiration, including failed release, lost acquire response and process disappearance. Do not merely change the timeout documentation while preserving the indefinite takeover.

### High — One physical WLED controller can acquire multiple independent output owners

Evidence: `src/core.rs:180–192` compares hosts ignoring ASCII case but compares saved IDs literally. `src/engine.rs:111–129` and `159–184` group routes in case-sensitive string maps. `src/outputs.rs:330` normalizes MAC identity only at connection time.

A valid pair of disjoint ranges with the same MAC and hosts `Desk.local` / `desk.local` gets two controller tasks, not one. Each publishes its own zero-based buffer; the range starting later includes black pixels over the earlier route. The streams consequently compete, as do acquisition/restoration/release. A second pair using one hostname and one IP with MAC spellings `AA:BB:CC:DD:EE:FF` / `aabbccddeeff` also passes validation and both adapters subsequently accept the same physical identity.

An independent temporary Rust probe linked to the compiled application library reproduced `Config::validate() == Ok(())` for both inputs. No network or real light was involved. Normalize validated controller identities and canonical endpoint keys consistently before overlap checking, grouping and dispatch. Add regression tests proving equivalent identities cannot start competing owners and that disjoint mappings are combined into one complete frame.

### High — Application does not compile on Windows/macOS as inspected

Evidence: `src/app.rs:27–28` declares `sources: Vec<capture::Source>` only on non-Linux platforms. Its imports at `src/app.rs:1–6` do not bring `capture` into scope. This branch requires `crate::capture::Source` or an explicit import. The `Refresh sources` handler already uses the fully qualified path.

This is a direct name-resolution defect found by source inspection, not a claimed native compiler run. The Linux suite excludes the branch. A successful `cargo check -p scap --target ...` cannot detect it. Correct it and require full-application Windows/macOS build evidence when the corresponding environments are available. Document unavailable verification without implying that a matrix file is a passing matrix.

### Medium — Synchronous keyring reads block Tokio workers and bypass cancellation

Evidence: `src/outputs.rs:623–625` and `704–711` call `load_token`, which directly invokes `get_password` at `491–494`. Only saving a token is moved into `spawn_blocking` (`src/app.rs:185–188`). The runtime has two worker threads (`src/app.rs:38–40`). The configured synchronous Secret Service backend can perform blocking D-Bus calls and unlock interaction; its implementation unlocks locked entries before reading them.

A stalled keyring read occupies a network worker and cannot observe Stop. In the worst case it also holds the worker executing the coordinator/cleanup call stack, so independent WLED progress and shutdown responsiveness are not guaranteed. Move every credential operation, including discovery loads, to a blocking boundary; resolve credentials before starting the adapter, expose errors, and bound/cancel the async wait. A timed-out blocking operation itself cannot be forcibly cancelled, so ownership and late results need explicit treatment. Add an injected stalled credential loader test showing WLED and Stop remain responsive.

### Medium — Valid HA strip configurations can silently sample only part of the drawn strip

Evidence: `src/core.rs:94–99` permits any route to have 1–256 zones. HA routing uses only `colors[0]` (`src/engine.rs:173`). The UI sets one zone only for the currently selected HA light (`src/app.rs:260–262`).

A persisted HA strip with multiple zones passes strict configuration validation; if it is not the selected row, it can run while mapping only the first portion of its geometry to the light. This violates the one-color whole-strip interpretation. Enforce one logical zone for non-addressable routes during validation/migration, or deliberately aggregate the entire geometry. Test loaded configurations as well as UI-created ones.

### Low — Critical control loops are harder to review than necessary

`src/engine.rs:224–253`, portions of `src/app.rs`, and the inner WLED/HA loops compress many state changes into long statements. The ownership design is reasonably clean, but this presentation makes cancellation and state-transition audits harder. `OutputAdapter` (`src/outputs.rs:56–66`) currently has no implementations and is not the actual dispatch boundary; describe it as a sketch or make adapters use it when adding another backend. Split setup, steady state and cleanup into small named functions when revising the affected code; avoid building an elaborate abstraction solely for this review.

## What is sound

- Capture construction, blocking frame waits, normalization and sampling execute on a dedicated thread, with panic containment. No screenshot processing is performed on the UI thread or Tokio network loop.
- Bounded command/error channels and watch channels for latest processed frames/config/output targets prevent unbounded replay. Configuration pointer checks suppress output from outdated geometry snapshots. Session IDs suppress old worker failures.
- Output tasks are independently timed. HA awaits bounded acknowledgements, spaces successful calls after the acknowledgement, coalesces unchanged colors and uses round-robin selection. WLED status explicitly distinguishes HTTP reachability from unconfirmed UDP delivery.
- Stop first signals the capture worker, then signals all output tasks and joins their cleanup concurrently with a bound. A still-pending portal worker prevents another capture session. The synchronous portal limitation is honestly documented.
- Geometry validation bounds light count, zones, coordinates, radii and output ranges. Sampling uses immutable regional weights, linear-light averaging, elapsed-time smoothing and an explicit SDR contract. Single-zone paths cover the full strip rather than just one endpoint.
- Strict versioned configuration excludes credentials, uses atomic replacement, and preserves invalid/future/oversized input files before recovery saves. Safe simulated defaults avoid accidental device activation on a bad configuration.
- The vendored Linux path uses the authorized PipeWire FD, owns portal resources, validates packed row bounds/stride, returns dequeued buffers and limits the callback queue. It stays a scap adapter rather than creating an unrelated capture subsystem.

## Verification performed and limits

Independently read all application modules, relevant tests, README/plan/verification/limitations, CI, and the vendor capture/portal/lifecycle code. Re-ran `cargo test --locked`: the sandbox run passed 20 tests and failed 7 solely because sockets were denied. Re-ran with authorized loopback socket access: **27 passed, 0 failed**, including mocked DDP/HTTP/WebSocket cases. Re-ran `cargo test --manifest-path vendor/scap/Cargo.toml --lib`: **5 passed, 0 failed**. Ran the independent duplicate-identity validation probe described above.

Existing documented Linux release/fmt/Clippy results, real Hyprland capture and visible/minimized/restored UI counts were inspected as evidence, not independently repeated by this reviewer. The lead is recording updated performance after the cadence/lookup changes; pre-optimization capture measurements must not be presented as final overhead. A source review of WLED firmware was used to validate lease semantics.

Not verified here: physical WLED/HA lights; restoration races against actual firmware; real keyring unlock/cancel; native Windows/macOS application builds or capture; GNOME/KDE; prolonged sleep/resume; a full manual geometry/discovery/editor walkthrough. These gaps reduce confidence but are not fabricated failures. Missing targeted tests for the findings above matter more than increasing the raw test count.

**Scope judgment:** the main Linux end-to-end application and architectural direction are substantially satisfied. The full requested acceptance scope is not yet satisfied because finite failure cleanup, single-controller ownership, and application cross-platform compilability require correction. Confidence is high in the reported source-level defects and Linux mock-suite result; moderate in overall Linux desktop usability; low in untested platform/hardware runtime behavior.
