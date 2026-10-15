# WLED and Home Assistant

## WLED

In **Devices → Add device**, enter a controller hostname/IP and choose **Inspect and add**. Inspection reads its identity and LED count; reconnect checks the saved MAC to guard against address reassignment. Refreshing a known controller preserves its mapped range.

Dartmoor requires the JSON API, readable `/json/cfg`, and DDP on UDP port 4048. Its checks use the WLED 0.15.1 configuration schema; missing or incompatible fields refuse streaming. Configure these in WLED before starting:

| Setting | Required value |
|---|---|
| Realtime UDP | Enabled |
| Realtime timeout | 0.1–10 seconds |
| Main-segment-only realtime (`mso`) | Disabled |
| Realtime LED remapping (`rlm`) | Disabled |
| Realtime offset | 0 |
| DMX start address | 1 |
| Skip out-of-sequence packets | Disabled |
| Realtime override (`lor`) | 0 |

Dartmoor checks these settings without changing persistent firmware configuration. It clears inherited live overrides, then DDP renews a finite lease; it never acquires an indefinite JSON `live:true` lease.

**First LED** and **LED count** use physical indices: `[start, start + count)`. Mappings sharing a canonical host or MAC are aggregated into one controller task; overlaps are rejected and the first configured endpoint is used. Host case and MAC punctuation/case normalize, but DNS aliases are not resolved to prove identity. DDP controls the whole controller and blanks unmapped LEDs. Firmware grouping, mirroring and custom maps are not translated into layout geometry.

Stop releases live mode. **Video → WLED behavior** optionally restores previous state, only when the observed state still matches the state captured before acquisition. Restoration is best effort because WLED has no atomic compare-and-swap. If cleanup fails or the process exits unexpectedly, the verified timeout ends this sender's lease unless another sender renews it. HTTP health is reported separately from UDP delivery, which has no acknowledgement.

## Home Assistant

Enter the HTTP/HTTPS base URL and a long-lived access token in **Devices → Add device → Home Assistant**, then connect and discover. The connection uses `/api/websocket`. Select available color-capable entities and choose **Add selected**; existing, unavailable and white-only lights show why they cannot be added.

Supported color modes are RGB, RGBW, RGBWW, HS and XY. Every entity receives one color, even with strip placement. RGBW/RGBWW use zero white channels. Service calls are round-robin and globally spaced by at least 500 ms (default 1,000 ms), coalescing redundant colors. More lights share that cadence. Stop leaves the last applied color.

Tokens use Secret Service on Linux, Credential Manager on Windows or Keychain on macOS. They are excluded from config and logs; there is no plaintext fallback. Credential operations share one gate and a two-second UI wait limit. A native prompt may outlive that timeout; finish or cancel it before retrying. Use HTTPS outside a trusted local network.

## Shared constraints

Each light has one active route. Dartmoor does not infer identity between WLED and HA or fail over between them. Avoid adding the same physical light through both. Automations and manual control can compete with synchronization; stop before taking manual control.

WLED JSON is capped at 1 MiB. HA messages/frames are capped at 32/16 MiB, deferred events at 256 entries/8 MiB, and discovery/cache at 10,000 lights. Oversized or malformed replies fail visibly. Large JSON parsing runs off the async executor; these limits bound ingress, not total process memory.

For directly attached bulbs, see [USB Zigbee](zigbee.md). Implementation: [WLED adapter](../src/outputs/wled.rs), [HA adapter](../src/outputs/ha.rs), and [shared output contracts](../src/outputs.rs).
