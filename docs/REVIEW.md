# Bounded quality-review log

Rubric: correctness/scope30%, architecture20%, resilience/concurrency20%, verification/platform20%, usability/efficiency10%. Overall is the lower independent score; critical findings block approval. Initial implementation plus at most three revisions. Authors were delegated core/config and outputs (GPT-6.1 Sol), capture/platform fixes (GPT-6 Astra); two separate GPT-6 Astra agents independently reviewed the actual code and reran checks. Reviewers did not author implementation. Lead owns integration/editor/engine/docs.

| Iteration | Architecture | QA/platform | Overall | Result |
|---|---:|---:|---:|---|
| Initial |7.1|6.8|6.8|Changes required|
| Revision1 |8.2|8.1|8.1|Accepted; no critical/high findings|
| Revision2 |8.4|8.3|8.3|Accepted after user-requested hardening|

## Initial implementation

Linux release/fmt/Clippy passed,27 application tests plus5vendor tests passed. Real Hyprland portal capture and synthetic native UI lifecycle tested. Review reports: [architecture](reviews/initial-architecture.md), [QA](reviews/initial-qa.md).

High findings: JSON `live:true` makes WLED lease indefinite; case/alias differences allow duplicate controller tasks; non-Linux UI name-resolution error. Medium: keyring reads block Tokio; HA configs could sample only first strip zone. Low: repeated shape selection resets geometry, source labels show raw IDs, RGBW white-channel documentation mismatch, compressed control-loop readability.

## Revision1

- WLED acquisition now uses finite DDP leases after explicit inherited-override reset. Read-only firmware config verifies realtime UDP and0.1–10s timeout; no persistent config writes or `live:true`. Added real loopback cadence/latest/stale/stop and lease-policy regressions.
- Canonical MACs key validation, task ownership and combined target frames; aliases cannot create competing owners. Host/ID conflicts and overlapping ranges rejected.
- Fixed non-Linux import; Windows scap dependency integration cross-check passes. Full Windows application cross-check needs absent MinGW C compiler; native Windows/macOS runtime remains unverified.
- Credential lookup runs once per startup on a blocking thread with a bounded async wait; no automatic prompt/retry thread accumulation. Injected stuck-loader test checks executor responsiveness.
- HA requires one zone for whole-path averaging. RGBW/WW native service arrays explicitly zero white channels.
- Shape reselection preserves geometry; friendly source labels and selected-route inspection added; capture/engine control loop split into readable helpers.
-36 application tests and5vendor tests pass; both independent reviewers reran verification. Formatting, strict Clippy and Linux release build pass. Reports: [architecture](reviews/revision1-architecture.md), [QA](reviews/revision1-qa.md). Native synthetic editor rendering was subsequently visually inspected from a compositor window-only screenshot. Performance and UI evidence are recorded separately.

Revision1 used1 of3 attempts and met its stopping condition at8.1/10. The user subsequently requested another hardening pass, excluding real-world and Windows/macOS tests.

One output-author follow-up later reported a model usage limit after writing its revision. Lead integration checks and both independent final reviews completed successfully; no unavailable agent result is counted. During concurrent independent tests, one reviewer encountered UDP4048 already in use; the serial rerun passed36/36. Revision2 fixes this test-harness isolation limitation.


## Revision2 — user-requested follow-up (2026-10-04)

The working directory is now `dartmoor`; the Cargo package remains `lumen-desktop`. Existing work was retained.

- Editor: preserve saved source selection; invalidate previews by frame identity and clear absent previews; ignore stale inspection results; validate additions transactionally, bound imported names, and count only successful HA additions. Keep the latest unsent Apply for retry when the bounded command queue is full.
- Engine: tag frames by session so live tuning cannot starve output; rebuild geometry and reset smoothing only when geometry changes; pair all opened capture sources with cleanup; wake low-FPS pacing on Stop; bound shutdown queueing and cancel the coordinator with worker/output stop guards.
- Outputs: serialize native credential operations across load/save and timeout/cancellation; add bounded async saves; enforce WLED physical mapping, DMX address, sequence-filter and override prerequisites; preserve the pre-acquisition restoration baseline; retain failed-release status; invalidate HA caches after changed capability/availability refresh. Loopback tests use ephemeral ports.
- Added regression tests for the above, including controlled in-flight capture/config changes and mock disconnect on WLED release. No new UI, desktop capture, native keyring, physical-light or Windows/macOS test was run.
- One engine specialist reached a usage limit after writing its changes; the lead inspected, integrated and added the missing regression tests. Two fresh GPT-6 Astra reviewers independently inspect implementation they did not author. Preliminary findings during this review (WLED DMX/sequence prerequisites and hidden stop-release failures) were fixed before final review completion.

Final Linux release, formatting and strict Clippy passed;49 application and5 vendored capture tests passed. Both reviewers independently reran the final49 application tests. Reports: [architecture](reviews/revision2-architecture.md), [QA](reviews/revision2-qa.md). The documentation drift noted during review is corrected, including obsolete fixed-port warnings and historical evidence labels.

Revision attempts used:2 of3. Final overall score:8.3/10, the lower of independent8.4 architecture and8.3 QA scores. No unresolved critical/high finding; the stopping condition is met. Remaining platform/hardware evidence gaps and protocol/OS limitations are recorded in the verification and known-issues documents.
