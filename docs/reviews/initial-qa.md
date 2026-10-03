# Independent initial QA, platform and protocol review

2026-10-03. Reviewer did not author implementation. This records the initial revision inspected, before the lead's subsequent fixes. Line locations refer to that revision. Reviewed README, PLAN, VERIFICATION, KNOWN_ISSUES, configuration, implementation, CI and measurement harness; checked primary WLED and Home Assistant documentation/source. No physical lights, HA credentials, or Windows/macOS runtime were available.

**Score: 6.8/10. Do not accept this initial revision at the required 8/10 threshold.** It is a coherent working Linux initial implementation with substantial scope covered, but an actual WLED lifecycle defect and a non-Linux compile defect require correction. Missing hardware and native OS access are honestly documented confidence limits, not demands for fabricated evidence.

| Rubric | Weight | Score |
| --- | ---: | ---: |
| Correctness and scope | 30% | 6.5 |
| Architecture | 20% | 8.0 |
| Resilience | 20% | 5.5 |
| Verification and platforms | 20% | 7.0 |
| Usability | 10% | 7.0 |

Weighted result 6.75, rounded to 6.8.

## Severity-ranked findings

1. **High / P1: acquisition disables WLED's finite realtime timeout.** `src/outputs.rs:152-161,408-417` acquires with JSON `{"live":true}`. In the official [WLED JSON implementation](https://raw.githubusercontent.com/Aircoookie/WLED/main/wled00/json.cpp), this calls `realtimeLock(65000)`. The [WLED v0.15.1 realtime implementation](https://raw.githubusercontent.com/Aircoookie/WLED/v0.15.1/wled00/udp.cpp) maps this to `UINT32_MAX`, and incoming realtime packets preserve that value. A killed app or failed HTTP cleanup can therefore leave the controller in realtime indefinitely. The README claim that unreachable devices may fall back to their realtime timeout is not true for this acquisition path. Acquire through normal finite-timeout DDP and keep best-effort explicit release; preserve honest ownership/restoration semantics. Add a mock that models finite versus infinite lease behavior. Current mocked HTTP tests only assert request ordering and miss actual firmware semantics.

2. **High / P1: non-Linux application field does not resolve.** `src/app.rs:28` uses `Vec<capture::Source>` under `cfg(not(target_os = "linux"))` but the module is not imported in the file. Other references use `crate::capture`. Windows and macOS compile this branch. Qualify the type or import the module and verify a Windows cross-check/native matrix. The author was already checking platform compilation; this finding remains part of the initial assessment even if fixed concurrently. A Linux pass does not exercise this branch.

3. **Medium / P2: hostname normalization differs between validation and routing.** `src/core.rs` accepts the same WLED ID with case-insensitively equal hosts and disjoint ranges. `src/engine.rs:112-129,159-183` groups and publishes using case-sensitive strings. Thus `wled.local` and `WLED.local`, same MAC and adjacent ranges, are accepted but become two controller tasks, each sending its own partially black frame to the same device. Use one canonical controller key throughout validation, grouping and status. Also canonicalize MAC spelling: validation currently compares raw IDs while connection identity verification removes separators/case. Different textual representations must not bypass the one-controller rule. Add a combined-route test proving one task and one complete frame.

4. **Medium / P2: credential lookup is blocking inside network tasks.** `src/outputs.rs:624,705` calls synchronous OS-keyring `load_token` directly within async discovery/stream startup. UI token saving already uses `spawn_blocking`, but the read path does not. A slow/locked credential backend occupies a Tokio worker and delays cancellation; no credential-backend availability or latency evidence was collected. Move reads to the blocking pool and keep Stop responsive while a result is pending. This is a source-confirmed blocking call, not a claim that an observed keyring hang occurred.

5. **Low / P3: clicking the active shape destroys geometry.** `src/app.rs:203-223` unconditionally replaces a bulb or strip with defaults whenever its selected shape button is clicked, including clicking the already-selected shape. Reproduce by moving strip points, then clicking the selected “Strip path” button: the path returns to its two default points. Only replace geometry when the shape actually changes.

6. **Low / P3: RGBW/WW documentation promises a policy that is not implemented.** `src/outputs.rs:638-645` sends `rgb_color` for RGB, RGBW and RGBWW. [HA documents automatic conversion](https://developers.home-assistant.io/docs/core/entity/light/#turn-on-light-device) to supported modes. This is a legitimate service request, but it does not explicitly set white channels to zero as KNOWN_ISSUES states. Either send explicit supported `rgbw_color`/`rgbww_color` tuples with zero white channels and test them, or document that HA performs conversion and controls the white-channel allocation.

7. **Low / P3: selected source loses its human-readable name.** `src/app.rs:148` uses the saved ID as the selected combo label, although entries have `Source::name`. After choosing a named display/window the closed selector shows `display:<id>` or `window:<id>`. Resolve the saved ID against enumerated names, with an explicit unavailable-source fallback.

## Checks and strengths

- Independently ran `cargo test --locked` with authorized loopback socket access: **27 passed**. Independently ran `cargo test --manifest-path vendor/scap/Cargo.toml --lib`: **5 passed**. These checks did not mutate implementation.
- Inspected DDP header flags, RGB24 data type, byte offsets, payload splitting, length fields, sequence rollover and final-packet PUSH. Socket and packet unit tests meaningfully exercise construction; successful UDP writes are correctly described as unconfirmed delivery. WLED's documented port is [4048](https://kno.wled.ge/interfaces/ddp/).
- HA handshake and ID-correlated results match the [WebSocket API](https://developers.home-assistant.io/docs/api/websocket/). Failed acknowledgements are errors. Freshness checks, coalescing, round-robin selection and interval reset after acknowledgement avoid bursts. HA and direct routes remain separate; no hidden fallback. On/off/white-only targets are excluded.
- Linear-light region averaging, strip arc-length zones, reversal, geometry bounds, aspect-preserving reduction and time-based smoothing have inspectable implementations and relevant tests. One-zone strips cover their path; bulbs duplicate one region regardless of route pixel count.
- Strict versioned credential-free JSON, bounded loading, safe demo defaults, invalid-file backups and same-directory replacement are appropriate. Tokens are not serialized or printed; user-facing transport errors avoid echoing server payloads. HTTP redirects are disabled. No plaintext credential fallback was found.
- Main-thread native UI, independent blocking capture worker, bounded command queue, latest-value frame/output channels and task-owned cleanup provide a sensible initial architecture. The finite-lease defect is a protocol-level hole in otherwise deliberate stop/error handling.

## Evidence and confidence limits

Linux formatting, strict application Clippy and release build were reported by the lead; this reviewer independently reran tests, not every build command. The reported real Hyprland portal probe delivered 1192 frames over 30 seconds including chooser delay, routed only to mocks. The reported synthetic native lifecycle probe advanced frames while minimized/restored. Those establish useful capture and UI independence evidence, not physical output, broad Wayland compatibility, native Windows/macOS behavior or a complete manual editor walkthrough.

The measurement script uses `/proc` process CPU and RSS samples plus child resource usage. Its one-core CPU percentage definition and startup/chooser caveats are correct. The original real-capture measurement preceded the processing cap/LUT optimization and cannot substantiate final optimized performance. Synthetic sampling timings exclude capture, network and editor; synthetic UI timings exclude real capture. Require final post-change measurements to be labeled separately. No GPU measurement exists.

No screenshot of the actual desktop was requested or retained by this reviewer. `/tmp/lumen-ui.png` was absent at review time, so no visual QA is claimed. UI interaction assessment above is code-grounded. Hardware color calibration, physical restoration races, real HA service/device behavior, credential backend availability, macOS grants, Windows capture and other Wayland compositors remain explicitly unverified. WLED segment-only realtime/custom mapping settings also require documented setup or rejection before claiming physical indices on every controller configuration.

Scope is substantially implemented for an initial Linux release: native editor, simulated and real capture paths, adjustable bulbs/strips, bounded processing, direct WLED, optional HA, persistence, diagnostics and documented exclusions. Scope should be accepted only after the high findings are fixed and regressions checked; no requirement to invent inaccessible platform/hardware evidence is imposed.
