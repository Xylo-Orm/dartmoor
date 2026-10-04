# Independent UX revision review

Reviewed 2026-10-04 against the current uncommitted UX changes in `src/app.rs`, `src/editor.rs`, and the persistence test seam in `src/config.rs`. The reviewer changed only this report, not implementation code. No commit or push was performed.

**Score: 8.5/10. Accept the editor improvements within the verified scope.** No unresolved critical, high, or medium defect was identified in the final reviewed delta. This is a source and headless-interaction assessment, not certification of native rendering, accessibility, capture, or physical light behavior.

| Dimension | Weight | Score | Assessment |
| --- | ---: | ---: | --- |
| Correctness | 30% | 8.7 | Editing transactions, designated-segment insertion, geometry, and keyboard focus behavior have relevant passing regressions. |
| Architecture | 20% | 8.5 | Pure geometry/history operations are separated from egui; setup, inspector, canvas, and rendering have distinct responsibilities. Some UI and discovery branches remain dense. |
| Resilience | 20% | 8.5 | Bounded history, coordinate limits, transactional additions, validation feedback, and preservation of valid loaded values reduce accidental changes. |
| Verification | 20% | 8.2 | Independently reproduced 16 relevant tests, including four egui headless input/layout tests. Native presentation and platform behavior were deliberately excluded. |
| Usability | 10% | 8.5 | Setup and properties separation, compact tabs, visible state/save feedback, exact coordinates, undo/redo, and path controls improve discoverability and precision. |

Weighted total: 8.50. Scores assess the implemented change and available evidence, not effort or historical platform checks.

## Findings resolved during review

1. **Insertion into retraced paths:** the initial inspector computed a midpoint on the selected segment and then globally selected the nearest segment again. Overlapping segments could redirect insertion and leave the wrong point selected. Both inspector and canvas now use `insert_path_point_at`, retaining the selected/hit segment identity. The designated insertion regression also checks invalid segment indices, endpoints, zero-length segments, non-finite coordinates, and the point limit.
2. **Save shortcut while editing:** Ctrl/Cmd+S was initially blocked whenever an input had focus. Save handling now operates outside the text-focus guard, while nudges and global undo remain guarded. A headless test focuses the name input, proves that a cursor arrow does not move the light, and saves to a temporary file using the shortcut.
3. **Silent mutation of loaded settings:** inherited slider ranges could clamp valid configuration merely by rendering. Smoothing and HA interval controls now cover their validated upper ranges; radius sliders use `SliderClamping::Edits`. The layout test renders a configuration containing smoothing 4900 ms, HA interval 59000 ms, and radius 0.95 without changing it. The HA section is collapsed in that test; its expanded full-range control was verified by source inspection.
4. **Compact properties access:** the initial compact layout put properties after the entire setup and light list. Setup/Light properties tabs now give them separate access and scroll state, and selecting a light in the list or canvas opens its properties. The headless bounds test checks both 1080×700 and 720×480 layouts; it does not establish every tab interaction or every populated-list layout.
5. **Undo/redo recording:** undo and redo must not be recorded as fresh edits, which would invalidate the redo branch. The explicit history-change guard prevents that. Headless keyboard interaction checks nudge → undo → redo, and a separate multi-frame pointer drag test verifies a single undo transaction.
6. **Path instruction precision:** the canvas double-click hint now specifies the selected path, matching the interaction's scope.
7. **Undo during asynchronous discovery:** a second reviewer identified that restoring configuration history while HA discovery was pending could change the target server. The final toolbar, keyboard, and internal undo/redo guards reject history navigation while busy. HA results also carry their originating URL and are ignored if the current server differs. This reviewer inspected those guards and independently reran the regression that attempts keyboard undo during discovery, then injects a stale result and confirms that no light is imported and the busy state clears.

## Independent checks

- `cargo test --locked --offline --lib app::tests`: **8 passed, 0 failed**, including four headless tests covering layout/configuration preservation, keyboard focus/save/undo/redo, multi-frame dragging, and discovery/history isolation with stale-result rejection.
- `cargo test --locked --offline --lib editor::tests`: **8 passed, 0 failed**, covering bounded history and branches, physical-aspect-ratio zone placement and reversal, duplicate handles, projection/insertion, designated segment identity, removal constraints, and clamped nudges.
- Read the final editing, rendering, validation, persistence, and test paths. Confirmed explicit test saves target a temporary directory and automatic persistence is suppressed for headless test instances.

These tests run egui frame/input processing without creating a native window or starting capture. The reviewer did not rerun the whole application suite, formatting, linting, release builds, or platform CI; separate lead verification owns those results. Earlier native CI results concern an earlier committed revision and do not establish native verification of these uncommitted changes.

## Remaining limits and low-priority follow-ups

- Native visual inspection is still needed to assess text wrapping, long names, dense paths, large light lists, scaling, contrast in actual captured scenes, and assistive-technology behavior. Shape output and canvas bounds are meaningful layout evidence but do not prove those presentation qualities.
- Coincident handles remain visually and spatially ambiguous: nearest-handle hit testing can favor another light at the same position. The light list and exact coordinate controls provide a recovery path; preferring the selected light on equal-distance hits would improve this edge case.
- Several discovery and device-route UI branches still contain long expressions. Small named helpers would make future validation and asynchronous-state changes easier to review.
- Real capture, native window lifecycle, OS keyring, physical WLED/HA output, and Windows/macOS execution were excluded. No claims about those behaviors are derived from this review, and no additional native or hardware testing is requested in this iteration.
