# Limitations

## Capture and color

- Capture uses one display/window. Linux requires a portal/PipeWire session; X11 capture, multi-monitor composition and GPU/DMA-BUF reduction are not implemented.
- The portal chooser blocks inside scap. Stop ends output but cannot dismiss it; answer or cancel the chooser before restarting.
- Two seconds without an image or explicit Idle notification stops Video. A gap over five seconds requires reacquisition even if frames resume. Only macOS supplies explicit unchanged-image Idle events; an unchanged Linux/Windows source can trigger the cutoff.
- Processing assumes SDR/sRGB. HDR, display profiles and physical LED calibration are unsupported. Bounded sampling can alias fine detail; regional dark detection can suppress intentional dim colors. Equal competing color families favor the established selection.
- CPU-mapped Linux capture still depends on compositor GPU readback. Extreme layouts can take time to compile during edits. Sampling benchmarks exclude capture, UI, network and physical-light latency.

## Devices and music

- WLED requires the validated firmware settings in [device setup](devices.md). UDP sends have no delivery acknowledgement. State restoration is best effort and can race manual changes.
- Home Assistant and Zigbee are single-color ambient outputs. They retain the last color/transition on Stop and cannot reproduce addressable WLED frames or fast music flashes. Other controllers and automations can compete with all routes.
- USB Zigbee supports Dongle-P/TI Z-Stack only. Native USB/capture behavior needs testing on each target OS; the CI build matrix is not runtime certification.
- System playback capture is Linux-only. Music does not infer instruments, downbeats or meter. Syncopation, speech, compressed mixes and tempo changes can produce missed/extra attacks or half/double-tempo ambiguity. Physical audio-to-light latency is not calibrated.

## Lifecycle and resources

- There is no tray service or background daemon. Minimizing keeps synchronization running; closing stops it. Studio is a placeholder.
- Configuration, replies and event queues have bounds; see [configuration](configuration.md) and [device limits](devices.md#shared-constraints). Parsed data and native GL/windowing resources can exceed those raw-byte limits.
- Already-running blocking JSON/keyring calls cannot be interrupted. A credential operation can finish after the UI times out; its shared gate prevents overlapping requests.

Automated checks cover synthetic inputs, local protocol mocks and radio-independent driver behavior. Current runtime support must be assessed with the [manual probes](contributing.md#runtime-probes) on the intended desktop and devices.
