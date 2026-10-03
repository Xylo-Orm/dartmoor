# Dartmoor

A native Rust editor that samples one desktop/window and synchronizes WLED controllers using DDP or Home Assistant lights using conservative ambient updates. One process; closing the window stops synchronization. Minimize it to keep syncing. No daemon, webview, privileged service or Home Assistant requirement.

## Build and run

Rust 1.88+; the lockfile pins compatible dependencies. Linux requires PipeWire/SPA headers, D-Bus, libclang, Wayland, EGL and xkbcommon development libraries.

```sh
# Debian/Ubuntu
sudo apt-get install libpipewire-0.3-dev libspa-0.2-dev libdbus-1-dev libclang-dev libxkbcommon-dev libwayland-dev libegl1-mesa-dev
# Arch: pacman -S base-devel clang pipewire libpipewire dbus libxkbcommon wayland mesa
cargo build --locked --release
cargo run --locked --release
```

Windows: install Rust stable with MSVC and Visual Studio C++ build tools, then the same Cargo commands. macOS: install Xcode command-line tools and Rust stable; requires macOS 12.3+ ScreenCaptureKit. Grant Screen Recording permission in System Settings, restart after a grant if required. Native Windows/macOS runtime behavior remains unverified until tested there; CI builds do not prove capture works.

Linux Wayland needs a functioning `xdg-desktop-portal` and compositor-specific backend, PipeWire, and a logged-in graphical D-Bus session. Do not run with sudo. Start opens the portal chooser; select a single display or window. The app does not silently select a display or promise silent permission restoration. No X11 capture fallback is included. Linux source enumeration is deliberately delegated to the portal. Windows/macOS can refresh and select enumerated sources.

## First use

1. The initial source and strip are explicitly simulated. Start demonstrates the preview, placement and sampled colors without changing real lights.
2. Select Screen / window and choose your source. Allow screen sharing in the system dialog.
3. Add a WLED hostname/IP using **Inspect and add**. Inspection reads LED count, MAC identity and segment ranges. Adjust First LED/LED count to physical indices; DDP uses physical order, not arbitrary WLED segment grouping/mirroring. Streaming owns the whole controller: unmapped LEDs are black. Never mistake drawn strip geometry for per-LED capability.
4. Select a marker; drag a bulb or numbered strip points. Change its sampling extent, add/remove strip points and reverse its order. Set one logical zone for a single-color strip; multiple zones map proportionally to the controllable LED range. More zones require addressable hardware. Bulb shape samples a region regardless of the route's LED count.
5. Start, Pause or Stop. Geometry, brightness and smoothing apply between frames. Stop before changing sources, devices, output limits or hardware mappings. Save layout to persist edits; closing also saves.

WLED must support its JSON API, readable `/json/cfg`, and DDP UDP port4048. The safety check uses the WLED0.15.1 configuration schema; `/json/cfg` is not a stable public schema, so missing or incompatible fields refuse streaming with an actionable error. In WLED Sync settings, enable realtime UDP and configure a timeout between0.1 and10seconds (normally2.5seconds). The app checks these settings without changing persistent device configuration. It clears any inherited live override, then DDP acquires a finite lease; it never sends JSON `live:true`, which disables the firmware timeout. HTTP reachability is reported separately from **unconfirmed** UDP delivery; a successful UDP send is not physical-light verification. For physical-index mapping, disable WLED main-segment-only realtime mode and realtime LED remapping, and set realtime offset to0; these modes can otherwise change which LEDs receive each index.

Each controller gets one output task; non-overlapping mappings may share that task. A failed direct route never switches to HA.

Home Assistant is optional. Supply its HTTP/HTTPS base URL (for example `https://ha.example`), enter a long-lived access token, then Connect and discover. The app upgrades to `/api/websocket`, authenticates, inspects available color capabilities and subscribes to state changes. Color-capable rgb/rgbw/rgbww/hs/xy entities are added as single-color outputs; white-only/on-off lights are skipped. A strip drawn for HA still gets one color. Tokens go only into the OS credential store: Secret Service on Linux, Credential Manager on Windows, Keychain on macOS. A Linux desktop without an unlocked Secret Service will show an error; no plaintext fallback exists. Use HTTPS beyond trusted local networks. Tokens never enter layouts or logs.

HA service calls are globally spaced by at least500ms (default1000ms) and round-robin across lights, with redundant colors coalesced. This is ambient synchronization; WebSocket connectivity does not establish a light's streaming capacity. Automations and manual changes can compete with both routes. There is no protocol-level exclusive control. Stop synchronization before taking manual control; do not add both direct WLED and HA records for the same physical light without reliable identity evidence. The app never guesses HA/WLED identity matches.

WLED optionally offers best-effort previous-state restoration. It reads prior state and restores only when observable state still matches the state acquired for synchronization. WLED lacks compare-and-swap, so simultaneous manual changes cannot be perfectly protected. Without restoration, Stop releases live mode; HA remains at the last applied color. Shutdown allows bounded network cleanup; after failed cleanup or process loss, the verified finite device timeout ends this application’s lease if no other sender renews it.

## Architecture

`core` contains normalized geometry, immutable sampling weights, linear-light averaging, time-based exponential smoothing and SDR/sRGB conversion. `capture` wraps scap and a deterministic synthetic source. Frames are reduced early to at most160×90 using a fixed stratified kernel; this is approximate downsampling. Scap exposes no reliable frame color-space metadata in the chosen release, so the explicit contract is SDR/sRGB, not HDR.

`engine` owns active configuration and routing. One worker does blocking capture, normalization and sampling; it receives immutable layout updates between frames. Bounded command/event queues preserve ordering; Tokio watch channels replace old previews/colors/config snapshots. Each controller task owns connection, timing, retries and cleanup. HA cannot hold up WLED. Output keepalives run independently of capture and UI repaint. A capture gap beyond2s is conservatively treated as failure because scap cannot distinguish an unchanged desktop from a lost session. Stop sending and reacquire explicitly. Sleep/resume is handled with the same safety policy. A pending portal chooser must be answered/cancelled before restarting; output is already stopped.

`outputs` separates capabilities and conversion from sampling. Its task/channel boundary and capability descriptor are the extension point for a local Hue Entertainment adapter with adapter-owned pairing/session; ordinary Hue REST flooding is excluded. `app` owns editing state and native egui interactions. Configuration is version1 strict JSON in platform application directories, atomically replaced. Invalid/future/oversized files load safe simulated defaults and are backed up on the next save. See [configuration examples](docs/configuration.md) and [scap patch](docs/scap-patch.md).

## Diagnostics and verification

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo test --manifest-path vendor/scap/Cargo.toml --lib
cargo build --locked --release
cargo run --release -- --demo-seconds 10
cargo run --release -- --capture-probe 20
cargo run --release -- --benchmark
cargo run --release -- --ui-smoke-seconds 12
cargo run --release -- --ui-smoke-seconds 30 --real-capture
RUST_LOG=lumen_desktop=debug cargo run --release
```

Loopback networking tests need socket permission in a sandbox. Capture-probe opens a real permission dialog but routes only to mock lights. UI smoke starts synthetic capture, minimizes/restores and closes itself. Add `--real-capture` for portal capture routed only to mocks. These are developer diagnostics, not hardware verification. See [verification evidence](docs/VERIFICATION.md), [quality review log](docs/REVIEW.md) and [known limitations](docs/KNOWN_ISSUES.md). The CI matrix targets native Linux/Windows/macOS builds/tests; its runs are not claimed until executed.

Primary references: [scap source](https://github.com/helmerapp/scap), [eframe0.33.3](https://docs.rs/eframe/0.33.3/eframe/), [PipeWire Rust](https://docs.rs/pipewire/0.10.1/pipewire/), [WLED JSON](https://kno.wled.ge/interfaces/json-api/), [WLED DDP](https://kno.wled.ge/interfaces/ddp/), [DDP specification](http://www.3waylabs.com/ddp/), [HA WebSocket API](https://developers.home-assistant.io/docs/api/websocket/), [HA light capabilities](https://developers.home-assistant.io/docs/core/entity/light/), [keyring](https://docs.rs/keyring/3.6.3/keyring/).
