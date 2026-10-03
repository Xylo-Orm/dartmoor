# Known limitations and deferred features

- Native Windows/macOS capture runtime and physical WLED/HA light behavior need tests on real systems. Current verified desktop is Hyprland Wayland; GNOME/KDE/other wlroots environments are untested.
- Portal chooser is synchronous inside scap. Stop ends output but cannot immediately dismiss a pending chooser; cancel/select in the system dialog. Restart is refused while that worker is pending. Closing exits the process.
- Scap cannot reliably distinguish unchanged desktop from a disconnected capture. A two-second frame gap stops streaming conservatively and requires Start. Some compositors may stop producing idle frames, making this policy inconvenient; future capture health information should resolve it.
- Direct WLED requires readable device configuration, realtime UDP enabled and a verified0.1–10s device timeout. Devices with inaccessible configuration or unsupported settings are refused until corrected. The check uses WLED0.15.1 configuration fields; `/json/cfg` is not a stable public schema.
- Direct WLED owns the whole controller and blanks unmapped LEDs. Disable main-segment-only realtime mode and realtime LED remapping and use realtime offset0 for physical-index semantics. Segment grouping/mirroring/custom hardware mappings are not inferred. UDP has no acknowledgement. Do not interpret HTTP reachability as verified light output.
- HA discovery skips white-only/on-off and unavailable lights. RGBW/RGBWW service calls set explicit RGBW/RGBWW arrays with white channels zero; no white-channel calibration. HA retains the last color after Stop. Manual changes/automations can compete with synchronization; ownership is advisory, restoration best effort.
- No automatic cross-route identity guess or failover. Re-enter route identifiers when switching; keep one record for each reliably identified physical device.
- Small-image stratified downsampling approximates whole-source averages and can alias very fine patterns. SDR/sRGB assumption only, without HDR/tone mapping.
- Native UI memory includes GL/windowing driver costs; measured synthetic UI RSS is significantly greater than headless processing. No general low-overhead claim is made.
- No tray/background mode: minimizing keeps syncing; closing stops. No inaccessible process is left running.

Deferred: Hue Entertainment adapter (local bridge, pairing/session ownership inside adapter), other native vendors, HDR, multi-monitor composition, remote frontends, audio reactivity, plugins, Xorg-specific capture. No custom capture backend or GPU processing has been introduced.

Test harness limitation: the finite-DDP integration test uses loopback UDP4048, so do not run multiple copies of the suite simultaneously. A concurrent-review bind conflict was reported, then the isolated full run passed36/36.
