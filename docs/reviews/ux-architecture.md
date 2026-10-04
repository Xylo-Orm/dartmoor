# Independent UX architecture and correctness review

Reviewed 2026-10-04 against the uncommitted UX changes in `/home/florian/Projects/dartmoor`: `src/app.rs`, the new `src/editor.rs`, the crate export and crate-visible configuration save helper. This reviewer changed only this report. No commit or push was performed.

**Score: 8.4/10. Accepted: the UX work fits the existing application architecture; no unresolved critical, high or medium implementation defect found.** The medium asynchronous discovery/history interaction identified during review was corrected and independently retested. Runtime and visual confidence remains limited by the deliberate exclusion of real UI/capture/hardware testing.

| Dimension | Weight | Score | Assessment |
|---|---:|---:|---|
| Correctness and scope | 30% | 8.5 | Placement, selection, validated live edits, source preservation and configuration history are coherent; pending discovery now guards Undo/Redo and rejects stale-server results. |
| Architecture | 20% | 8.7 | Pure geometry/history helpers are separated from egui; existing capture/output and persistence boundaries remain intact. |
| Resilience and concurrency | 20% | 8.3 | History is bounded, pending live edits coalesce, stale WLED route inspections remain guarded; HA discovery/history race corrected. |
| Verification and platform | 20% | 8.1 | Independently passed 61 tests, formatting and strict Clippy; synthetic egui input exercises actual render handlers. No native UI or platform claim is added. |
| Usability and efficiency | 10% | 8.4 | Clear setup/properties grouping, responsive inspector, path direction/zones, numerical placement, keyboard movement, dirty status and coalesced drag undo. Native visual inspection remains outstanding. |

Weighted total: 8.41, rounded to 8.4. This score concerns the authorized UX increment and its integration, not physical output or cross-platform certification.

## Findings and disposition

1. **Medium, resolved — Undo/Redo during HA discovery could change the destination server.** Initially, a user could edit the HA URL from A to B, start discovery against B, and undo the URL to A before B's untagged result arrived. The final implementation blocks history operations while `self.busy` in the toolbar, keyboard path and `App::undo` itself. `JobResult::Ha` additionally carries the originating URL, and `poll_jobs` discards results whose server differs from the current trimmed URL. The new `discovery_cannot_be_retargeted_by_undo_or_a_stale_reply` test exercises the real headless keyboard handler, then independently injects a stale result and proves no configuration mutation. I inspected both defenses and independently passed the final 61-test suite. No real-device incident or credential leakage occurred during testing.
2. **Low — Headless rendering does not establish native visual quality.** The tests exercise responsive geometry at the selected viewport sizes and real egui input handling, but do not inspect compositor rendering, DPI/font behavior, accessibility, unusual long device messages or all panel-resize combinations. Preserve that distinction in the delivery. A later authorized native walkthrough should cover double-click insertion, long lists, narrow windows and route-validation recovery.
3. **Low — Full configuration snapshots have a count bound rather than a byte bound.** `EditHistory` retains at most 64 snapshots in each direction and coalesces pointer-held edits. This is reasonable for validated configurations, but text edits can temporarily exceed validation limits before saving/applying is refused. Very large pasted names or URLs can therefore make history more expensive. This does not create a new remote input pathway or require redesign for the current scope; input length limits are a future small improvement.

## Architecture and regression assessment

- `src/editor.rs` contains pure history, projection, insertion/removal, nudge and zone-placement operations. The 64-entry history cap, redo invalidation on new edits and no-op handling are explicit. Path insertion preserves the selected segment identity, including retraced paths. Removal preserves at least two points and a nonzero path. Zone visualization uses the canvas aspect ratio for physical path lengths, matching the sampling model.
- `App::finish_edit` records the pre-interaction configuration and waits until neither a pointer interaction nor keyboard focus remains, so a drag creates one Undo step and focused field edits are grouped. `App::undo` flushes an open edit before moving history, and the toolbar recognizes an open transaction. Undo/Redo are intentionally unavailable while synchronization runs or a connection job is pending, preventing historical source/device changes from bypassing those boundaries.
- `App::render` compares the complete before/after configuration, avoiding missed updates from individual widget response flags. Only valid live edits enter `pending_apply`; a full engine command queue retains the newest pending state. Source, output cadence and hardware mapping controls remain disabled during capture. Sampling/output implementations are unchanged by this UX increment.
- Text-entry focus suppresses geometry arrow keys and global Undo/Redo. Ctrl/Cmd+S remains available while entering text and checks configuration validity before persistence. Live invalid geometry is shown as a layout error and does not enter the engine. Undo provides recovery after invalid edits.
- Dirty state compares the current configuration with the last successful save. The injected `config_path` directs headless saves to a temporary directory. `config::save_to` changes only visibility; its validation, atomic replacement and preservation of malformed existing files remain unchanged. Close still performs the existing best-effort autosave. No migration or credentials-in-config change is introduced.
- The access token stays outside `Config`, so history, dirty comparisons and saved JSON do not copy it. No new network transport, credential API or backend privilege is introduced. Existing masked input, async credential gate and destination validation remain in use.
- New tests use an egui context and synthetic input without constructing a native window or capture source. They exercise layout preservation, nudge/undo/redo, text-focus isolation, save to an isolated temporary path, and an actual multi-frame drag followed by a single undo. The helper tests additionally cover bounds, duplicate/retraced points and wide-screen zone positions.

## Independent verification and limits

- `cargo test --locked`: **61 passed, 0 failed** on the final rerun, with authorized local mock socket access. The intermediate 60-test source also passed before the discovery-race correction.
- `cargo fmt --all -- --check`: **passed**.
- `cargo clippy --locked --all-targets -- -D warnings`: **passed**.

This pass did not launch the native UI, capture a desktop, invoke real keyring operations, contact physical WLED/HA devices, or perform Windows/macOS builds or execution. Vendor capture code was unchanged; its prior five-test result was not rerun for this UX-only review. No performance or accessibility certification is implied. Confidence is high in the inspected pure/editor integration behavior and mock regressions, moderate in native usability until a separately authorized visual walkthrough.
