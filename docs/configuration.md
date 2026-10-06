# Configuration and device setup

`config.json` is version1 JSON. Linux normally stores it under `$XDG_CONFIG_HOME/lumen-desktop` (or `~/.config/lumen-desktop`); macOS under `~/Library/Application Support/org.Lumen.lumen-desktop`; Windows under the user's application-data directory chosen by `directories::ProjectDirs`. The UI Save action and closing write atomically through a same-directory tempfile. No credentials are in this file. Bad/newer/oversized files are retained in timestamped `config.invalid-*.json` backups on recovery save. There is no automatic downgrade of unknown schema versions.

The **Layouts** menu exports this same document to the clipboard or imports pasted JSON. Imports reject unknown fields, repeated fields, unsupported versions, documents over 1 MiB, invalid geometry and conflicting device mappings. Validation finishes before any layout replacement. Import is an Undo transaction; it does not save immediately. An exported layout contains addresses and entity IDs, so review it before sharing. Access tokens remain in the OS credential store and are never exported. Importing an HA server address does not transfer its credentials.

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
      "shape": { "kind": "strip", "points": [{"x":0,"y":1},{"x":0,"y":0},{"x":1,"y":0},{"x":1,"y":1}], "radius":0.04,"reverse":false,"segment_leds":[15,30,15] },
      "route": { "kind":"wled", "host":"192.168.1.51", "start":0,"count":60,"device_id":"112233445566" }
    }
  ],
  "fps": 30,
  "smoothing_ms": 120,
  "brightness": 0.7,
  "black_bar_detection": false,
  "soft_ambience": { "enabled": false, "strength": 0.6, "color_emphasis": 0.5, "vibrancy": 0.2 },
  "dark_zones": { "enabled": false, "threshold": 8 },
  "mode": "video",
  "music": { "detector": "advanced", "subbass_weight": 0.2, "sparkle_amount": 0.35, "follow_beat": true, "input": "playback", "device": "", "sensitivity": 0.5, "attack_ms": 20, "decay_ms": 240, "brightness": 0.7, "color": [100,170,255] },
  "ha_url": "http://homeassistant.local:8123",
  "ha_interval_ms": 1000,
  "restore_wled_state": false
}
```

Coordinates normalize to the selected source. Extent is relative to its shorter dimension. Without `segment_leds`, path zones divide equal arc lengths. With explicit counts, each segment gets its own sampling zones and physical LED range. Reversal changes logical ordering. LED ranges use physical indices, with an exclusive end of `start+count`. WLED segment offsets are displayed during inspection; grouping, skipping, mirrored segments and custom LED mapping are not reinterpreted as logical coordinates. DDP uses the controller's physical buffer. Host aliases and MAC punctuation/case normalize to one controller owner; all its ranges are combined, and overlaps are rejected. The first configured endpoint is used. Total supported layout:64 records,256 zones each,2048 total zones; one record may map at most4096 LEDs.

## Soft ambience

Enable **Soft ambience** under Synchronization to favor meaningful colorful areas
within each light's own sampling region. It defaults off. The checkbox reveals
three sliders, each from 0–100% (stored as finite numbers from 0 to 1):

| Control | Default | Behavior |
|---|---|---|
| Ambience strength | 60% | Blend accurate linear-light sampling with the enhanced result; 0% is exactly accurate sampling. |
| Color emphasis | 50% | Increase colorful pixels' influence relative to neutrals, with a bounded area-sensitive weight. |
| Vibrancy | 20% | Add up to 50% saturation around linear-sRGB Rec.709 luminance, reduced as needed to fit the RGB gamut. |

Changes apply on subsequent frames without restarting capture. Brightness and
temporal smoothing still operate afterward. Save, export/import and Undo/Redo
include all four settings. The `soft_ambience` object and its missing members
default as shown above, so older layouts load with the mode disabled. Unknown and
repeated fields are rejected; slider ranges are validated even when disabled.

The sampler decodes SDR/sRGB pixels into linear RGB before averaging. For each
pixel, let M and m be the largest and smallest linear channels. Its extra color
influence is `(M-m)²/M` (zero for black): relative chroma times absolute chroma
suppresses near-black noise. The color weight is
`1 + 4 × color_emphasis × influence`, between 1 and 5. The enhanced mean
multiplies this weight by the existing region feather/geometry weights and divides
by their weighted total. It inspects colors before averaging; it can favor a broad
colorful area mixed with gray, which a saturation boost of the accurate average
alone cannot do. One colorful pixel has at most five pixels' influence, so a tiny
detail cannot take over a broad neutral region.

Vibrancy scales the enhanced mean's displacement from its luminance-neutral axis
using one common gamut-limited scale, preserving hue direction and luminance.
Strength blends accurate and enhanced colors in linear RGB. Neutral scenes retain
their neutral character; black remains black. This is an ambient representative
mean, not a dominant-hue picker: equally influential opposing colors can still
cancel or mix into a hue between them. Continuous weights avoid histogram-bin or
winner-switch jumps, but do not guarantee cinematic grading or recover detail
already lost in capture downsampling. Color emphasis may change average luminance
when colorful areas and neutrals have different brightness.

Both sampling paths use the same active picture after black-bar cropping and the
same zone ordering, reversal and LED mapping. Bulbs broadcast one enhanced region
color, and single-color strips sample the whole path. All output routes share this
pipeline. Disabled or zero-strength mode uses the original efficient sampler.
Enabled mode adds one shared four-channel row-prefix table (at most 463,680 bytes),
one image pass, and bounded work per stored run/feather boundary; it does not scan
all pixels or allocate a histogram for every zone. It also temporarily retains the
accurate zone colors while producing enhanced colors. Maximum layouts still cost
more than typical edge strips; sampling benchmarks exclude capture, UI, networking
and physical-light latency (see [verification](VERIFICATION.md)).

## Black bar detection

Enable **Black bar detection** in the Synchronization panel to exclude stable
letterbox (top/bottom) and pillarbox (left/right) bars from color sampling.
The preview shows the same active picture used by the lights; normalized light
positions follow this picture rather than the original capture edges. Switching
off restores the full source immediately. The toggle works during synchronization
and persists through Save, export/import and Undo/Redo. `black_bar_detection`
defaults to `false`, including when absent from older layouts.

Detection runs on the small capture image, allowing near-black pixels and a
small amount of noise. Opposing bars must have similar widths, each less than
30% of that image dimension, and remain consistent for at least 350 ms and
three observations. Fully dark scenes retain the established crop; source
resizing resets it. Asymmetric edges, subtitles/logos in bars, and dark content
can limit detection. Disable the feature when the chosen crop is unsuitable.

## Physical LEDs per path segment

In a strip's properties, enable **Set LEDs per segment**. Each row corresponds to two adjacent numbered handles. Screen-edge paths label those sections Left, Top, Right and Bottom. Custom paths use Segment labels. Counts are positive integers and their sum must be at most4096; segments with LEDs must have nonzero length. WLED's LED count is derived from this sum while First LED remains editable. Changes to counts require stopping synchronization. These are geometry sections of one strip, independent of WLED firmware segments.

For a full perimeter starting at bottom-left and following left → top → right → bottom, `"segment_leds":[24,40,25,38]` describes127 LEDs. Set the WLED route's `count` to127. The first24 physical LEDs sample the left edge, the next40 the top, then25 the right and38 the bottom. With `reverse:true`, physical order becomes bottom38 → right25 → top40 → left24, and sampling reverses within each edge too.

Color zones remain separate from physical LED counts. Addressable mode requires at least one zone per path segment and allows at most the LED total or256 zones, whichever is smaller. The editor reduces the zone count when a lower LED total requires it. Remaining zones are apportioned by LED count using highest averages; each segment's zones divide that segment's length evenly. Colors are expanded to LEDs within each segment, so no rounding at one side spills into the next. Set zones to the sum of LED counts for one sample per LED when that sum is at most256. Larger strips share samples within each section. Single-color mode still samples the entire path once.

The optional `shape.segment_leds` array has one entry per adjacent pair of points. An omitted or empty array retains automatic whole-path mapping, including in older layouts. Counts persist through Save, export/import and Undo/Redo. Point insertion splits the existing count in proportion to the inserted point's position, preserving the total; one-LED sections cannot be split. Removing an interior point merges adjacent counts. Removing an endpoint transfers its LEDs to the neighboring segment. Presets preserve the total and distribute LEDs evenly across the new sections; adjust these initial counts to match the physical installation. Presets requiring more segments than available LEDs are disabled.

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

Strip presets in **Light properties → Place along desktop…** generate normalized edge paths inset 0.04 from the source boundary. Top, right, bottom and left follow clockwise screen order. Three sides begins at bottom-left and ends at bottom-right via the top edge; full perimeter closes back at bottom-left. Reverse color order swaps sampled output order and, with segment counts enabled, physical segment order. Presets preserve radius, reversal and route; they retain color zones unless an increase is needed to give each segment a zone. Segment LED counts are redistributed as described above. Any handle remains editable, and the replacement is one Undo transaction. Unicode text editing respects the persisted field byte limits without splitting a character. Merely displaying a loaded value never shortens it.

Device response limits are separate from the1MiB layout-file limit. WLED JSON bodies are bounded incrementally at1MiB, including chunked responses. HA accepts messages up to32MiB, with individual frames up to16MiB; a request may defer at most256 events totaling8MiB, including events retained by earlier requests. Discovery/cache accepts at most10,000 light records, entity IDs256 bytes, friendly names1,024 bytes, and32 capability strings of at most64 bytes each. Unknown short modes remain discoverable but do not gain unsupported color control. Larger or malformed replies fail visibly and can be retried after the endpoint is corrected; unusually large HA installations may exceed these supported limits. JSON parsing of64KiB or more moves an owned buffer into a blocking task, keeping the networking executor responsive. Parsed JSON heap size can exceed raw bytes, so these limits do not promise a fixed application RSS.

## Turn off dark zones

Enable **Turn off dark zones** under Video → Synchronization to send exact RGB
zero for confirmed dark sampling regions. It defaults off. **Black sensitivity**
ranges from 0 to 64 SDR/sRGB code values and defaults to 8; higher values also
classify more shadows as black. Zero detects exact black. Changes apply live and
persist through Save, layout interchange and Undo/Redo. Missing new fields use
these defaults; unknown fields and invalid ranges remain errors.

The detector uses each zone's accurate, spatially weighted linear-light average
from the same active picture as Soft ambience, before enhancement, brightness or
smoothing. All three channels must be below the converted threshold for 80 ms.
An off zone wakes when any channel exceeds threshold + 4. This hysteresis avoids
rapid switching; short black flashes do not cut the lights. Confirmed darkness
clears both output and smoothing history. Changes to settings, geometry or crop
reset confirmation. Disabled mode retains the original pipeline without extra
sampling. This is a region decision: a black pixel inside a mixed bright region
will not turn its entire zone off. Large sampling footprints can retain nearby
color, and raising sensitivity can erase intentionally dim content.

Without this toggle, accurate black samples already encode as RGB zero; smoothing
can retain the preceding scene's color briefly. The sampler does not substitute
white for black. The Video diagnostics show accurate and emitted RGB for the
selected light's first zone, plus the number of zones gated off. If emitted RGB
is zero but a physical strip remains white, inspect the output/device behavior.
Physical black-to-off behavior has not been verified in this increment.

## Music workspace

The **Video | Music** tabs change the editing workspace. They do not change the
active source until **Start music** or **Start video** is pressed. Starting the
other mode releases the previous output session and waits for its worker to stop
before opening the new source. Stop a pending portal chooser in its system dialog
before switching. Both workspaces share lights, routes and physical LED mappings;
Video geometry is retained.

On Linux, choose **System playback** to capture the default PipeWire playback
sink's monitor, then play desktop audio. The optional **Playback device** field
accepts a PipeWire sink node name; empty means the default. Source/device changes
require stopping Music. **Demo pulses** is a clearly labeled synthetic source
available on all platforms. Native system playback on Windows/macOS is deferred.

Choose **Advanced** for midbass attacks, separate upper-frequency sparkles and
confidence-based beat following. **Classic** retains the previously tested
180 Hz bass-energy Pulse detector for comparison. Advanced is the default,
including when the new fields are absent in an older layout. Existing saved
sensitivity, envelope, brightness and color values are retained.

| Control | Range | Default | Behavior |
|---|---|---|---|
| Detector | Classic / Advanced | Advanced | Switch analysis without reopening audio. |
| Sensitivity | 0–100% | 50% | Higher values detect smaller spectral changes and quieter attacks. |
| Subbass weight | 0–100% | 20% | Supporting low-frequency influence on main attacks; Advanced only. |
| Sparkle amount | 0–100% | 35% | Upper-frequency accent brightness; zero removes accents; Advanced only. |
| Follow beat | Checkbox | Enabled | Predict weaker pulses when timing evidence is strong; Advanced only. |
| Attack | 0–500 ms | 20 ms | Rise of the main pulse. |
| Decay | 20–2,000 ms | 240 ms | Main pulse fade. |
| Brightness | 0–100% | 70% | Overall Music output intensity. |
| Color | SDR/sRGB RGB | 100, 170, 255 | Palette for pulses and accents. |

All effect controls update live and persist with mode/source through Save,
layout interchange and Undo/Redo. Detector changes replace analysis state;
Follow beat changes reset the tracker. Unknown/duplicate fields, non-finite
values and out-of-range numbers are rejected. Subbass weight and Sparkle amount
are finite 0–1 values; detector serializes as `classic` or `advanced`, and
`follow_beat` is boolean. Missing fields receive the table defaults.

### Advanced analysis and rhythm

Stereo channels are transformed separately and their power combined, preserving
opposite-phase energy. The analyser uses a preplanned RustFFT transform with
2,048-sample Hann windows at 48 kHz, advanced every 480 samples (10 ms). This is
42.7 ms of audio context with approximately 23.4 Hz bin spacing. Windows, FFT
scratch, band weights and prior spectra are reused. Bands have smooth overlapping
edges around 35–90 Hz (subbass), 90–300 Hz (midbass), 300 Hz–4 kHz (body/percussion)
and 4–12 kHz (air). These are frequency cues, not isolated instruments; the coarse
low-end resolution and overlap mean a sound can contribute to adjacent bands.

Positive changes in log-compressed spectral magnitude create a novelty signal
for each band. Comparing against neighboring prior bins reduces triggers from
small pitch movements, following the principle of [SuperFlux](https://github.com/CPJKU/SuperFlux),
without claiming equivalence to its complete algorithm or published accuracy.
Per-band exponential novelty baselines, bounded relative scores and absolute
energy floors adapt the threshold. A band must contain a meaningful share of
window energy: sharp upper-frequency attacks should not become main pulses
merely through small lower-band leakage. Midbass has full main-attack influence,
body supports it and subbass adds the configured contribution. Main attacks have
120 ms minimum spacing; sparkle attacks have independent 60 ms spacing.

A main attack holds its target for 60 ms before ordinary attack/decay. Sparkles
have a 50 ms hold, 5 ms attack and 75 ms fade. They add bounded intensity in the
selected color without substituting white. Addressable strips use deterministic
logical-zone placement with smaller neighboring accents, then existing reversal
and segment/physical LED expansion. Bulbs broadcast one color even with multiple
logical zones; single-color strips receive a restrained whole-light accent.
Zero Sparkle amount restores uniform rendering. Main brightness and accents are
combined and clamped in linear light before shared SDR/sRGB encoding.

The tracker retains at most 96 main events and 96 upper-band events, expiring
those older than eight seconds. Every 250 ms of eligible analysis it evaluates
60–200 BPM candidates by timing alignment, weighted beat coverage, competing
candidates and continuity. Upper events contribute at most 3% of candidate score
as subdivision evidence; hats alone cannot establish a main tempo. Following
requires at least five main events spanning two seconds, repeated candidate
agreement and internal confidence of at least 65%. The meter describes algorithm
support, not measured recognition accuracy.

**Listening** uses direct reactions. **Following beat** can fill missing attacks
with a 65%-intensity target; near real attacks merge with predictions.
**Reacquiring** continues direct reactions after confidence drops. At most two
predicted pulses can occur without another main attack, and predictions require
audible input. Quiet spaces between beats briefly retain the tempo estimate;
prolonged quiet, missing evidence or stream discontinuities clear following.
Capture idle/timeouts never generate predictions. Tempo changes can need several
seconds to displace older evidence. Half/double-tempo ambiguity, syncopation,
speech, cymbal washes, compression and sounds outside the search range can still
cause missed/extra events or uncertain tempo. This first version does not identify
instruments, downbeats or time signatures.

### Timing, diagnostics and limits

Audio polling and analysis run on the worker independently of 30 Hz light output.
Sample counts determine event timing; no audio buffer arrival timing is claimed
to be a calibrated playback timestamp. PipeWire copying remains bounded. Dropped
or renegotiated blocks invalidate analysis continuity and increment a stream-reset
indicator (coalesced discontinuities, not an exact dropped-sample count).
Ordinary timeouts shorter than 100 ms retain analysis windows; a longer absence
starts fading and clears analysis/tracking. Explicit negotiated idle fades output
while retaining connection freshness. Two seconds without samples or explicit
idle stops with an error. Queueing, window context, attack envelopes, output
cadence and device response all contribute to observed latency.

The workspace shows RMS, pulse, main/sparkle counts, per-band meters, predicted
pulse count, stream resets, tracking state, confidence and BPM when following.
Band meters use a square-root display scale; tooltips give raw RMS. The strip
preview shows individual logical zones. Processing time is accumulated analysis
and rendering between output frames, excluding audio wait, UI and device latency.

Classic uses the previous stereo 180 Hz bass filter, fixed 10 ms energy windows,
relative bass threshold of 2.1–1.12× (about 1.41× at 50% sensitivity), absolute
noise gates, positive-rise check, 120 ms spacing and 60 ms target hold. Its own
Attack/Decay and Music brightness remain effective.

WLED uses the shared output path. Home Assistant retains its conservative update
interval, suitable for slower ambience rather than beat timing. The user's real
playback/WLED feedback verified the earlier Classic behavior; this Advanced
increment has deterministic analysis, headless UI and mock-device coverage only.
No physical Home Assistant testing has been performed. New desktop-audio and
physical-light evaluation remain with the user; see [verification](VERIFICATION.md)
for exact tests and release analysis/rendering measurements.
