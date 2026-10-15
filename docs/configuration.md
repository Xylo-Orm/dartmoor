# Configuration

## Workspaces and editing

**Devices** owns discovery, names, connections, output type, LED ranges and color-zone counts. **Layout** owns placement, paths, sampling extent and direction. **Video** and **Music** own their source and effect settings. **Studio** is a placeholder. Navigation does not change the active session; the bottom transport starts, switches or stops it. Devices, Layout and Studio can stop a session but cannot start one.

Drag bulbs or numbered strip handles to snap to a 2% grid; hold Alt for free placement. Numeric coordinates remain exact. Double-click a selected strip to insert a point. Narrow windows combine the light list and placement inspector. Video's preview is read-only; **Edit placement** returns to Layout.

| Shortcut | Action |
|---|---|
| Arrow / Shift + arrow | Move the selected handle by 0.002 / 0.02 of the source dimension. |
| Ctrl/Cmd + Z / Ctrl/Cmd + Shift + Z | Undo / redo when text input is not focused. |
| Ctrl/Cmd + S | Save, including while editing text. |

A drag or focused text edit forms one undo transaction. History is bounded to 64 entries each way and an approximate 8 MiB snapshot budget. Stop synchronization and finish pending connections before Undo/Redo, import, discovery or hardware changes. Invalid drafts retain the last valid live configuration; starting requires a valid layout with at least one light.

## Placement and physical LEDs

Coordinates range from 0 to 1 across the active picture; sampling radius is relative to its shorter dimension. Without explicit segment counts, strip zones divide the path by arc length. Single-color output samples the whole path once; Home Assistant and Zigbee always receive one color per light.

**Set LEDs per segment** assigns positive physical LED counts to each adjacent pair of strip handles. The sum becomes the mapped LED count. Insertion splits a count, removal merges/transfers it, and one-LED segments cannot be split. These sections describe physical geometry, independently of WLED firmware segments.

Addressable output requires at least one zone per segment, up to the smaller of 256 or the LED count. Extra zones are apportioned by segment LED count, then expanded within each segment. **Reverse** reverses segment order and samples within each segment. For example, counts `[24,40,25,38]` around left → top → right → bottom map 127 LEDs; reverse maps bottom 38 → right 25 → top 40 → left 24.

Limits: 64 lights, 256 zones per light, 2,048 total zones, 128 points per strip, and 4,096 physical LEDs per WLED mapping. IDs must be unique. See [device mapping](devices.md#wled) for controller-wide ownership and overlap rules.

## Soft ambience

Video runs at 30 FPS in the editor and always uses Soft ambience.

| Control | Range | Default |
|---|---|---|
| Brightness | 0–100% | 100% |
| Color strength | 0–100% | 50% |
| Response | 0–1,500 ms | 50 ms |
| Black bar detection | On/off | On |
| Turn off dark zones | On/off | On |
| Black sensitivity | 0–64 | 50 |

Soft favors substantial colorful regions, resists short competing flashes, and controls color and brightness separately. Zero Color strength retains color-family selection. At zero Response, selected targets apply immediately; selection confirmation and device cadence still apply. Home Assistant and Zigbee use slower envelopes.

For bulbs, **Layout → Placement → Video response** inherits the global Response. Uncheck **Use global Response** for a per-bulb override. It applies live, persists as `video_response_ms`, and is retained but inactive if the shape changes to a strip. Music controls are independent.

### Black bar detection

Stable opposing letterbox/pillarbox bars are excluded from sampling; the preview and normalized placement follow the same active picture. Detection requires similar widths, each below 30% of that dimension, consistent for at least 350 ms and three observations. Dark scenes retain the established crop; resizing resets it. Disable detection if subtitles, logos, asymmetric bars or dark content produce an unsuitable crop.

### Turn off dark zones

Each zone uses its accurate linear-light regional average before enhancement or brightness. All channels must remain at or below the converted Black sensitivity threshold for 80 ms to emit exact black and clear smoothing history. Any channel above threshold + 4 wakes the zone. Higher sensitivity also removes more dim content; zero detects exact black. Geometry, crop and setting changes reset confirmation. The Video panel shows how many zones are off.

## Saved files and interchange

Configuration is strict version-2 JSON, capped at 1 MiB. **Save** and closing replace it atomically. Locations retain the historical application name:

- Linux: `$XDG_CONFIG_HOME/lumen-desktop/config.json`, normally `~/.config/lumen-desktop/config.json`.
- macOS: `~/Library/Application Support/org.Lumen.lumen-desktop/config.json`.
- Windows: the config directory returned by `directories::ProjectDirs::from("org", "Lumen", "lumen-desktop")`.

Invalid, oversized or unsupported-version files load fallback settings and are preserved as timestamped `config.invalid-*.json` backups on recovery save. Version 1 is not migrated. The interactive editor selects desktop capture, removes mock lights, fixes capture to 30 FPS and enables Soft ambience.

Clipboard import validates the complete document before replacement, is undoable, and does not immediately save. Addresses and device IDs are exported; access tokens are held separately in the OS keyring. Unknown/duplicate fields, invalid values, overlapping mappings and dangling music light references are rejected, except explicitly supported retired music fields.

Saved-file recovery removes retired MQTT, Matter and TP-Link/Tapo lights and their music references, with a notice and backup on save. Clipboard import rejects those routes. Retired music controls are discarded on load/save; old `traveling_wave`, `spectrum` and `ambient` effects load as Pulse. Existing credentials and pairing data are left in place.

### Minimal example

A Home Assistant bulb (connect its server separately to store credentials):

```json
{
  "version": 2,
  "source": { "kind": "desktop", "id": null },
  "lights": [{
    "id": "desk", "name": "Desk", "zones": 1,
    "shape": { "kind": "bulb", "center": { "x": 0.5, "y": 0.5 }, "radius": 0.08 },
    "route": { "kind": "home_assistant", "entity_id": "light.desk" }
  }],
  "fps": 30,
  "smoothing_ms": 120,
  "brightness": 1.0,
  "ha_url": "http://homeassistant.local:8123",
  "ha_interval_ms": 1000
}
```

Optional Video/Music fields use their defaults. `smoothing_ms` is retained by the lower-level accurate sampling path; Soft uses its own Response. Use the UI export for a complete current document. Schema and validation live in [config.rs](../src/config.rs), [core.rs](../src/core.rs) and [music/settings.rs](../src/music/settings.rs).
