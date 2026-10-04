# Bounded quality-review log

Rubric: correctness/scope30%, architecture20%, resilience/concurrency20%, verification/platform20%, usability/efficiency10%. Overall is the lower independent score; critical findings block approval. Initial implementation plus at most three revisions. Authors were delegated core/config and outputs (GPT-6.1 Sol), capture/platform fixes (GPT-6 Astra); two separate GPT-6 Astra agents independently reviewed the actual code and reran checks. Reviewers did not author implementation. Lead owns integration/editor/engine/docs.

| Iteration | Architecture | QA/platform | Overall | Result |
|---|---:|---:|---:|---|
| Initial |7.1|6.8|6.8|Changes required|
| Revision1 |8.2|8.1|8.1|Accepted; no critical/high findings|
| Revision2 |8.4|8.3|8.3|Accepted after user-requested hardening|
| Revision3 (UX) |8.4|8.5|8.4|Accepted; local changes retained without commit/push|
| Continuous follow-up |8.4|8.5|8.4|Accepted after one corrective pass; local only|

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


## Revision3 — user-requested UI/UX work

The user expanded the work to improving UI/UX and explicitly requested no commit/push. The earlier CI repair commit `ae6c0f1` completed before this instruction and passed the native matrix; all subsequent UX changes remain local.

Implemented separate setup/properties panels with compact tabs, input guidance and validation, ordered zone/direction overlays, numeric and keyboard placement, point insertion/removal, 64-entry undo/redo with coalesced interactions, unsaved-state feedback and Save shortcuts. Pure editor helpers were delegated to GPT-6.1 Sol; the lead owns UI integration. Two separate GPT-6 Astra agents independently reviewed actual code and ran relevant tests.

Review fixes: insertion keeps its hit-tested segment on retraced paths; Save works while text is focused; showing sliders preserves valid saved values; compact properties have direct tabs; undo does not create a new history entry; pending HA discovery blocks history and rejects results from a changed server. No critical/high/medium implementation finding remains. Low limits: native presentation/DPI/accessibility remain untested, overlapping handles can be ambiguous, and history is bounded by entries rather than bytes.

Verification:61 application tests, formatting, strict Clippy and the local Linux release build pass. Reports: [architecture](reviews/ux-architecture.md), [UX](reviews/ux-review.md). Overall score8.4/10 is the lower of8.4 architecture and8.5 UX. Revision attempts used:3 of3; acceptance threshold met. No native UI, real capture, device, keyring or new Windows/macOS test was performed for the local UX increment.


## Continuous follow-up — user-requested ongoing improvements

The original bounded loop completed its three revision attempts and met its acceptance threshold. The user then explicitly requested ongoing improvements until the usage limit, retaining the no-commit/no-push and no-real-world-testing constraints; later authorization also permits optional future features. This follow-up extends the existing implementation rather than restarting it.

Implemented strict clipboard layout import/export with atomic validation and Undo, an explicit searchable/virtualized HA discovery chooser, preserved verified WLED mappings, an approximate 8 MiB combined history bound and input limits, clearer app child-module separation, IPv6 WLED address handling, cancellation-owned HTTP health tasks, retry reset after recovered streaming, and explicit capture Idle liveness. Serde tagged unit variants receive additional strict-field checks. Dependencies and lockfile remain unchanged.

Requested GPT-6 Astra reviewers hit their model usage limits before producing reviews. Two separate available GPT-6.1 Sol agents independently inspected actual code and reran checks; neither authored the implementation. Their completed reports are [architecture](reviews/continuous-architecture.md) and [QA/platform/UX](reviews/continuous-qa.md). Failed Astra turns are not counted as reviews.

One corrective pass addressed review findings before final acceptance:

- **Medium, resolved:** an Idle-occupied macOS drop queue could lose the only changed image and refresh obsolete colors indefinitely. A bounded priority overwrite mailbox preserves the latest pending image/invalidation against Idle; retrieval no longer drains a later Idle over an image. Strict raw metadata, failed-conversion invalidation and native backend-error checks prevent false freshness. Thirteen portable queue/state tests exercise the actual shared policy on Linux. Mac framework calls remain source-inspected, not native compiled or executed.
- **Medium, resolved:** bulb zones cloned and repeatedly averaged identical sampling regions. One region and average per bulb now broadcasts to its controllable zones. Independent public-core probes confirm the resource reduction while checking cardinality/colors; strip ordering and invalid-frame output remain tested.
- **Verification repair:** standalone vendor all-target Clippy exposed eleven inherited style lints not covered by application dependency linting. Equivalent map/borrow/tail/if-let cleanups fixed them; failure and passing evidence are preserved.

Final independent architecture **8.4/10** (weighted 8.43) and QA **8.5/10** (weighted 8.53); overall **8.4/10**, the lower score. Both accepted the coherent initial scope, with no unresolved critical/high/medium implementation blocker. Final mandatory checks pass: 88 application tests, 18 vendor tests, both formatting and strict all-target Clippy scopes, Linux release build, and diff whitespace checks. Native platform/physical-light certification is not implied.

Remaining low issues: costly maximum broad strip layouts; coincident-handle selection ambiguity; multibyte names use a character input cap and byte validation; an unused future capability sketch and some dense output control flow. Current native UI/DPI/accessibility, macOS/Windows integration, compositor differences, real devices and native credentials remain unverified. The accepted original three-revision loop is historical; this separately authorized follow-up used one corrective pass, within its bounded review allowance. All new work remains uncommitted and unpushed.


## Local fixes and placement presets — latest user-requested follow-up

The previous accepted review’s four remaining low implementation findings are addressed: compact broad-strip sampling, active coincident-handle preference, persisted-byte text limits, and removal of the unused adapter-capability sketch with clearer output-task helpers. An additional audit fixed OS thread/runtime creation panics and stale canvas click drag state. New edge/three-sided/perimeter strip presets retain output and sampling settings and integrate with existing Undo. Dependencies/lockfile remain unchanged, with all work local and uncommitted.

Initial mandatory checks pass:101 application tests,18 vendor tests, both formatting/strict lint scopes, Linux release bins/examples and whitespace validation. No native UI/capture/device/keyring/macOS/Windows verification was performed. Both requested GPT-6 Astra reviewers reached their model usage limit before returning reports; those failed turns do not count as completed reviews. Two independent GPT-6.1 Sol fallback agents are reviewing architecture/correctness and QA/platform/UX on the actual source; neither authored these changes. Initial reviews completed at architecture8.5/10 and QA8.6/10 (overall8.5). Architecture independently reproduced QA’s transformed-degenerate strip case and classified it as a medium localized correctness defect; QA ranked the extreme input edge low. Both reports agree it samples the wrong source corner for valid geometry. QA additionally identified optional ingress-resource hardening based on the actual locked HTTP/WebSocket library defaults. A single narrow corrective pass addresses both findings before final acceptance; initial scores/findings remain in the reports. This is a separately requested follow-up with at most three corrective passes; the original accepted three-revision history is unchanged.


Corrective pass1 preserves transformed point placement for collapsed pixel paths and hardens remote-response ingress. All107 application tests, formatting/strict lint, Linux release bins/examples and whitespace checks pass. Both independent fallback reviewers completed focused re-review; their initial reports/scores remain visible with corrective disposition addenda. No production rewrite, dependency change or scope removal was used.


Correction1 re-review independently passed107 tests and all mandatory checks, and confirmed the original geometry/resource findings resolved. Both reviewers held acceptance at provisional8.5 because the new offloaded parser introduced a medium cancellation race: the HA cadence timer could drop a read after consuming a large availability/capability event. QA reproduced it with a held blocking worker; architecture independently confirmed the ownership boundary. The historical scheduling probe is preserved at `evidence/local-fixes-canceled-parse-probe.rs`.

Correction2 selects raw message receipt before committed decoding, permitting only terminal Stop to cancel an owned decode. Abort-on-drop ownership also cancels queued parser jobs; running bounded parses finish because Tokio cannot abort started blocking work. This is a narrow change to the same integration, not a rewrite. Final checks and independent disposition are recorded below. Two of three corrective attempts were used in this follow-up.


### Final acceptance

Final independent **architecture8.6/10** (weighted8.64) and **QA8.7/10** (weighted8.68); overall **8.6/10**, the lower score. Both GPT-6.1 Sol reviewers accepted the initial scope with no unresolved critical, high or medium implementation blocker. They authored only their reports/probes, independently reran109 application tests, formatting, strict lint, Linux release bins/examples and whitespace checks, and confirmed that restoring the old timer race makes the behavioral regression fail. QA also reran18 unchanged vendored helper tests and both vendor format/lint checks; architecture retains its initial independently passing vendor evidence. The attempted Astra reviews failed at model quota and supplied no scores.

Reports: [architecture](reviews/local-fixes-architecture.md#corrective-pass-2--final-acceptance), [QA](reviews/local-fixes-qa.md). Initial and corrective scores/findings remain visible in those reports. Two of three corrective attempts were used in this separately requested follow-up. The stopping condition is met; no further feature work is added to this batch. At final review, all source/evidence changes were uncommitted and unpushed, based on HEAD ae6c0f1.

Remaining limits: current native GUI/display/permission/keyring behavior, real capture, physical WLED/HA output and current macOS/Windows builds/runtime are unverified here; earlier native CI concerns ae6c0f1 only. Extreme layout compilation can briefly pause frames, response limits can reject huge HA installations, parsed JSON can exceed raw byte sizes, and running blocking parsers cannot be interrupted. Protocol-level delivery, concurrent manual control and restoration retain their documented best-effort limits. No known fixable critical/high/medium implementation blocker remains in the permitted environment.


Publication requested2026-10-05: the user authorized committing and pushing the accepted batch. The recorded review/test evidence above remains the verification of that reviewed source. Trimming blank lines at the end of saved logs and clarifying publication wording does not change application behavior. New native CI results must be assessed separately; no fresh native runtime or physical-light evidence is implied by publishing.
