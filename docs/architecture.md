# Architecture

Dartmoor has one native egui UI, one active Video or Music session, a blocking capture/analysis worker, and independently scheduled output tasks. Workspace navigation edits the UI state; Start/Switch/Stop controls the session.

## Source map

| Module | Responsibility |
|---|---|
| [`app.rs`](../src/app.rs) | UI state, editing transactions, background job results and lifecycle. |
| [`app/`](../src/app/) | Workspace panels, transport, discovery/import dialogs and diagnostics export. |
| [`config.rs`](../src/config.rs) | Strict versioned persistence, validation, load migration and atomic saves. |
| [`editor.rs`](../src/editor.rs) | Bounded Undo/Redo, placement and strip-path editing. |
| [`capture.rs`](../src/capture.rs) | Capture abstraction, frame normalization and synthetic source. |
| [`core.rs`](../src/core.rs) | Geometry, immutable sampling plans, LED mapping, linear-light math and encoding. |
| [`video.rs`](../src/video.rs) | Regional color selection, confirmation and response envelopes. |
| [`black_bars.rs`](../src/black_bars.rs) | Stable active-picture crop detection. |
| [`dark_zones.rs`](../src/dark_zones.rs) | Per-zone darkness confirmation and exact-black gating. |
| [`music.rs`](../src/music.rs), [`music/`](../src/music/) | Audio sources, spectral features, onset/tempo analysis, Pulse/Alternating rendering. |
| [`engine.rs`](../src/engine.rs), [`engine/music.rs`](../src/engine/music.rs) | Session ownership, worker updates, routing and freshness. |
| [`outputs.rs`](../src/outputs.rs), [`outputs/`](../src/outputs/) | Shared output contracts, WLED DDP, Home Assistant, credentials and cleanup. |
| [`zigbee.rs`](../src/zigbee.rs), [`zigbee/`](../src/zigbee/) | USB coordinator lifecycle, Python driver protocol and slow bulb updates. |

The larger UI, configuration, engine and output test suites live in their modules' `tests.rs` files. They remain unit tests with access to private helpers. Algorithm-focused tests also live alongside their implementations.

## Color and output boundaries

Capture produces bounded SDR/sRGB frames and retained color samples. Immutable sampling plans translate saved placement into regions. Video processing selects regional color, applies the saved response and dark-zone behavior, and hands linear colors to the engine. Physical LED distribution, reversal and final encoding remain downstream. Capture retains 16 unaveraged taps per cell. A bounded 12-family hue analysis selects substantial regional colors, confirms competitors over time, and transitions RGB with separate saturation/brightness envelopes. Dark-zone decisions use the accurate regional average. See [Video controls](configuration.md#soft-ambience) and [capture notes](scap-patch.md).

The engine gives each WLED controller one task and a latest-value target. That task owns DDP cadence, finite realtime leases, stale-frame handling, retries and cleanup. Non-overlapping mappings share the controller; slower Home Assistant/Zigbee calls do not block its sender. HTTP health and UDP delivery are reported separately.

Music uses the Advanced analyzer with Pulse or Alternating effects. Capture/device changes require a restart; effect and dynamics changes apply between audio blocks. Home Assistant and Zigbee use slower energy-based updates. The [music map](music.md) describes sample clocks, events and diagnostics.

## Configuration and editing

Configuration is strict version-2 JSON. The loader retains specific compatibility fields for removed music controls and rejects unrelated unknown fields. Those compatibility fields discard their values and are omitted from saved output. Removed effect names load as Pulse. Retired output-device migration is separate from strict clipboard import.

UI drafts may temporarily be invalid. The running session keeps its last valid configuration until edits are corrected. Undo history has entry and approximate memory limits, including cloned music strings and collection entries. Credentials stay in the OS keyring and are excluded from configuration snapshots and exports.
