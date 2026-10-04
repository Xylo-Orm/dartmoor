# Configuration and device setup

`config.json` is version1 JSON. Linux normally stores it under `$XDG_CONFIG_HOME/lumen-desktop` (or `~/.config/lumen-desktop`); macOS under `~/Library/Application Support/org.Lumen.lumen-desktop`; Windows under the user's application-data directory chosen by `directories::ProjectDirs`. The UI Save action and closing write atomically through a same-directory tempfile. No credentials are in this file. Bad/newer/oversized files are retained in timestamped `config.invalid-*.json` backups on recovery save. There is no automatic downgrade of unknown schema versions.

Example with a single-color strip and an addressable strip on separate controllers (replace both MACs and hosts with actual inspected devices):

```json
{
  "version": 1,
  "source": { "kind": "desktop", "id": "portal" },
  "lights": [
    {
      "id": "desk-ambient", "name": "Single-color strip", "zones": 1,
      "shape": { "kind": "strip", "points": [{"x":0.1,"y":0.9},{"x":0.9,"y":0.9}], "radius":0.05,"reverse":false },
      "route": { "kind":"wled", "host":"192.168.1.50", "start":0,"count":60,"device_id":"aabbccddeeff" }
    },
    {
      "id": "screen-edge", "name": "Addressable strip", "zones": 60,
      "shape": { "kind": "strip", "points": [{"x":0,"y":1},{"x":0,"y":0},{"x":1,"y":0},{"x":1,"y":1}], "radius":0.04,"reverse":false },
      "route": { "kind":"wled", "host":"192.168.1.51", "start":0,"count":60,"device_id":"112233445566" }
    }
  ],
  "fps": 30,
  "smoothing_ms": 120,
  "brightness": 0.7,
  "ha_url": "http://homeassistant.local:8123",
  "ha_interval_ms": 1000,
  "restore_wled_state": false
}
```

Coordinates normalize to the selected source. Extent is relative to its shorter dimension. Path zones divide equal arc lengths; reversal changes logical ordering. LED ranges use physical indices, with an exclusive end of `start+count`. WLED segment offsets are displayed during inspection; grouping, skipping, mirrored segments and custom LED mapping are not reinterpreted as logical coordinates. DDP uses the controller's physical buffer. Host aliases and MAC punctuation/case normalize to one controller owner; all its ranges are combined, and overlaps are rejected. The first configured endpoint is used. Total supported layout:64 records,256 zones each,2048 total zones; one record may map at most4096 LEDs.

Bulb shape:
`{"kind":"bulb","center":{"x":0.5,"y":0.5},"radius":0.08}`.
HA route:
`{"kind":"home_assistant","entity_id":"light.desk"}` with one zone.
Mock route:
`{"kind":"mock"}`. Synthetic source:
`{"kind":"synthetic"}`. Windows/macOS source IDs use `display:<id>` or `window:<id>`; unavailable saved targets require reselection.

The selected light's route dropdown changes its one active route. Re-enter the same physical device's identifiers when changing between HA and direct control; inactive route credentials/mappings are not exported or stored. HA entity IDs alone cannot reliably establish a WLED MAC match, so identity matches are never guessed. Before changing route, Stop. A direct route checks the saved MAC against WLED JSON on connection to protect against DHCP address reassignment.

To create a Home Assistant long-lived access token, use the HA user's security/profile token controls. Discovery requires an OS credential-store backend. Do not place tokens in URLs, JSON files, environment log dumps or source control. Both discovery and control use WebSocket API, including correlated service-call results, state updates and conservative scheduling. No token is needed for WLED.

WLED finite control requires readable `/json/cfg`, realtime UDP enabled, and a device realtime timeout of0.1–10seconds. Set these in WLED Sync settings. The application reads them and refuses unsafe or unverifiable leases; it does not overwrite persistent firmware configuration. JSON `live:false` clears inherited overrides; DDP packets renew the finite lease.

The safety read uses the WLED0.15.1 `/json/cfg` schema, which upstream does not promise to keep stable. Missing/incompatible fields fail closed. Physical-index mapping requires verified main-segment-only realtime mode (`mso`) disabled, realtime LED-map mode (`rlm`) disabled, and realtime offset0. Configure these in WLED; the application does not translate those modes into strip geometry or silently change them.

The application also requires DMX start address1 (`if.live.dmx.addr`), disabled out-of-sequence filtering (`if.live.dmx.seqskip=false`), and no realtime override (`state.lor=0`). The pinned WLED receiver applies its DMX address to DDP and its optional sequence filter assumes small frames, so incompatible settings are refused with an actionable message. These checks follow [WLED v0.15.1 receiver source](https://github.com/Aircoookie/WLED/blob/v0.15.1/wled00/e131.cpp) and [configuration serialization](https://github.com/Aircoookie/WLED/blob/v0.15.1/wled00/cfg.cpp). They are source and mock verified, not hardware verified.

Credential reads and saves share one process-wide gate. The UI stops waiting after two seconds; an OS operation may still complete later. Subsequent credential operations are refused while it remains active, preventing repeated Start/Connect from accumulating blocked threads. No plaintext fallback is used.
