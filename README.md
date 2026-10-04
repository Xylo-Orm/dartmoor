# Dartmoor

A native Rust editor that samples one desktop/window and synchronizes WLED controllers using DDP or Home Assistant lights using conservative ambient updates. One process; closing the window stops synchronization. Minimize it to keep syncing. No daemon, webview, privileged service or Home Assistant requirement.

## Build and run

Install Rust using rustup. The repository's `rust-toolchain.toml` selects Rust 1.99.0, rustfmt and Clippy for both development and CI; rustup installs them when needed. Run the verification commands below before publishing changes. `Cargo.toml` declares Rust 1.88 as the minimum, but that minimum is not separately verified by the current matrix. The lockfile pins dependencies. Linux requires PipeWire/SPA headers, D-Bus, libclang, Wayland, EGL and xkbcommon development libraries.

```sh
# Debian/Ubuntu
sudo apt-get install libpipewire-0.3-dev libspa-0.2-dev libdbus-1-dev libclang-dev libxkbcommon-dev libwayland-dev libegl1-mesa-dev
# Arch: pacman -S base-devel clang pipewire libpipewire dbus libxkbcommon wayland mesa
cargo build --locked --release
cargo run --locked --release
```

Windows: install rustup with the MSVC host and Visual Studio C++ build tools, then the same Cargo commands. macOS: install Xcode command-line tools and rustup; requires macOS 12.3+ ScreenCaptureKit. Grant Screen Recording permission in System Settings, restart after a grant if required. Native Windows/macOS runtime behavior remains unverified until tested there; CI builds do not prove capture works.

Linux Wayland needs a functioning `xdg-desktop-portal` and compositor-specific backend, PipeWire, and a logged-in graphical D-Bus session. Do not run with sudo. Start opens the portal chooser; select a single display or window. The app does not silently select a display or promise silent permission restoration. No X11 capture fallback is included. Linux source enumeration is deliberately delegated to the portal. Windows/macOS can refresh and select enumerated sources.

## First use

1. The initial source and strip are explicitly simulated. **Start sync** demonstrates the preview and sampled colors without changing real lights.
2. Use **Desktop source → Screen / window**, then Start and allow sharing in the system dialog.
3. Expand **+ Add WLED**, enter a hostname/IP, and choose **Inspect and add**. Inspection reads LED count, MAC identity and segment ranges. In **Light properties → Device route and mapping**, adjust First LED/LED count to physical indices. Streaming uses the whole controller; unmapped LEDs are black.
4. Select a light in the list or on the canvas. Drag a bulb or numbered strip handle, or enter normalized X/Y coordinates. Choose **Single color** or **Addressable**, adjust sampling extent and color zones, and reverse strip order when needed. The arrow shows color direction; **Show color zones** displays where ordered colors are sampled. Addressable mode requires compatible hardware.
5. Pause/Resume or Stop from the toolbar. Brightness, smoothing and placement apply during synchronization. Stop before changing sources, routes or mappings. Save layout to persist edits; closing also saves.

The setup panel is on the left, properties on the right. Narrow windows switch to **Setup / Light properties** tabs. Use **Place along desktop…** in a strip’s properties to lay it along an edge, three sides or the full perimeter; presets keep its sample extent, color direction, zones and device mapping and can be undone. Double-click the selected strip path to insert a point; use the point selector and Remove point to remove it. Undo/Redo groups each drag or focused text edit into one transaction and keeps up to 64 history entries in each direction within an approximate 8 MiB combined snapshot budget. History is available after stopping synchronization and finishing pending device connections.

Use **Layouts → Export JSON** to copy a layout and **Layouts → Import JSON** to paste one. Import validates the complete document before replacing anything, remains unsaved until Save or close, and can be undone. Device addresses are included; access tokens are excluded. Review addresses and the selected source before starting an imported layout.

Home Assistant discovery opens a searchable chooser. Select the supported, available lights you want, then **Add selected**. Existing entities, unavailable lights and lights without color control show why they cannot be added. Discovery does not add lights automatically. Stop before discovery or import. Refreshing an already verified WLED controller preserves its physical LED range; adding it again selects its existing properties.

| Shortcut | Action |
|---|---|
| Arrow keys | Move the selected handle by 0.002 of the source dimension |
| Shift + arrow keys | Move by 0.02 |
| Ctrl/Cmd + Z | Undo a layout edit when no text input is focused |
| Ctrl/Cmd + Shift + Z | Redo |
| Ctrl/Cmd + S | Save the valid layout, including while editing a text field |

Unsaved changes and layout errors are shown directly. Start is disabled until the layout is valid and contains a light. Temporary invalid edits keep the last valid live configuration until corrected. Demo sources and outputs remain explicitly labeled.

WLED must support its JSON API, readable `/json/cfg`, and DDP UDP port4048. The safety check uses the WLED0.15.1 configuration schema; `/json/cfg` is not a stable public schema, so missing or incompatible fields refuse streaming with an actionable error. In WLED Sync settings, enable realtime UDP and configure a timeout between0.1 and10seconds (normally2.5seconds). The app checks these settings without changing persistent device configuration. It clears an inherited indefinite live lease, then DDP acquires a finite lease; it never sends JSON `live:true`, which disables the firmware timeout. HTTP reachability is reported separately from **unconfirmed** UDP delivery; a successful UDP send is not physical-light verification. For physical-index mapping, disable WLED main-segment-only realtime mode and realtime LED remapping, and set realtime offset to0; these settings are checked before streaming. Also set the DMX start address to1, disable Skip out-of-sequence packets, and disable realtime override (`lor=0`). WLED applies those settings to DDP too.

Each controller gets one output task; non-overlapping mappings may share that task. A failed direct route never switches to HA.

Home Assistant is optional. Supply its HTTP/HTTPS base URL (for example `https://ha.example`), enter a long-lived access token, then Connect and discover. The app upgrades to `/api/websocket`, authenticates, inspects available color capabilities and subscribes to state changes. Choose color-capable rgb/rgbw/rgbww/hs/xy entities to add as single-color outputs; white-only/on-off and unavailable lights are disabled in the chooser. A strip drawn for HA still gets one color. Tokens go only into the OS credential store: Secret Service on Linux, Credential Manager on Windows, Keychain on macOS. Credential reads and saves have a two-second UI wait limit and share a single operation gate. If an OS prompt remains blocked, finish or cancel it before retrying; timing out cannot cancel the native operation. A Linux desktop without an unlocked Secret Service will show an error; no plaintext fallback exists. Use HTTPS beyond trusted local networks. Tokens never enter layouts or logs.

Device replies have ingress limits: WLED JSON1MiB; HA messages32MiB/frames16MiB; deferred HA events256 entries/8MiB; discovery/cache10,000 lights. Oversized replies produce an error rather than silently importing a partial list. Large JSON parsing runs off the Tokio executor. These are raw-response/retention guards, not a universal process-memory budget.

HA service calls are globally spaced by at least500ms (default1000ms) and round-robin across lights, with redundant colors coalesced. This is ambient synchronization; WebSocket connectivity does not establish a light's streaming capacity. Automations and manual changes can compete with both routes. There is no protocol-level exclusive control. Stop synchronization before taking manual control; do not add both direct WLED and HA records for the same physical light without reliable identity evidence. The app never guesses HA/WLED identity matches.

WLED optionally offers best-effort previous-state restoration. It reads prior state and restores only when observable state still matches the original state read before acquisition. WLED lacks compare-and-swap, so simultaneous manual changes cannot be perfectly protected. Without restoration, Stop releases live mode; HA remains at the last applied color. Shutdown allows bounded network cleanup; after failed cleanup or process loss, the verified finite device timeout ends this application’s lease if no other sender renews it.

## Architecture

`editor` contains bounded history and pure path/placement operations. `core` contains normalized geometry, compact immutable sampling weights, shared per-frame linear-light row sums, time-based exponential smoothing and SDR/sRGB conversion. `capture` wraps scap and a deterministic synthetic source. Frames are reduced early to at most160×90 using a fixed stratified kernel; this is approximate downsampling. Scap exposes no reliable frame color-space metadata in the chosen release, so the explicit contract is SDR/sRGB, not HDR.

`engine` owns active configuration and routing. One worker does blocking capture, normalization and sampling; it receives immutable layout updates between frames. Bounded command/event queues preserve ordering; Tokio watch channels replace old previews/colors/config snapshots. Each controller task owns connection, timing, retries and cleanup. HA cannot hold up WLED. Output keepalives run independently of capture and UI repaint. Explicit macOS Idle notifications let the worker reuse an unchanged image. Silence without an image or explicit Idle for more than2s is treated as failure: stop sending and reacquire explicitly. Linux/Windows currently have no equivalent idle health notification. Sleep/resume after a long interruption also requires restart. A pending portal chooser must be answered/cancelled before restarting; output is already stopped.

`outputs` separates device inspection and conversion from sampling. Its active extension boundary is task-owned adapters with latest-value targets, stop signals and status snapshots. A local Hue Entertainment adapter would own pairing/session and join this routing boundary; ordinary Hue REST flooding is excluded. `app` owns editing state and native egui interactions, with panels and the discovery/interchange dialogs in child modules. Configuration is version1 strict JSON in platform application directories, atomically replaced. Invalid/future/oversized files load safe simulated defaults and are backed up on the next save. See [configuration examples](docs/configuration.md) and [scap patch](docs/scap-patch.md).

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
cargo run --release --example sampling_stress -- broad # synthetic 2048-zone stress
cargo run --release -- --ui-smoke-seconds 12
cargo run --release -- --ui-smoke-seconds 30 --real-capture
RUST_LOG=lumen_desktop=debug cargo run --release
```

Loopback networking tests need socket permission in a sandbox. Capture-probe opens a real permission dialog but routes only to mock lights. UI smoke starts synthetic capture, minimizes/restores and closes itself. Add `--real-capture` for portal capture routed only to mocks. These are developer diagnostics, not hardware verification. See [verification evidence](docs/VERIFICATION.md), [quality review log](docs/REVIEW.md) and [known limitations](docs/KNOWN_ISSUES.md). The native Linux/Windows/macOS CI matrix previously passed for commit `ae6c0f1`. The newer editor and lifecycle changes have Linux/headless verification; consult the latest workflow run for subsequent native CI results. CI builds do not establish capture or physical-light runtime support.

Primary references: [scap source](https://github.com/helmerapp/scap), [eframe0.33.3](https://docs.rs/eframe/0.33.3/eframe/), [PipeWire Rust](https://docs.rs/pipewire/0.10.1/pipewire/), [WLED JSON](https://kno.wled.ge/interfaces/json-api/), [WLED DDP](https://kno.wled.ge/interfaces/ddp/), [DDP specification](http://www.3waylabs.com/ddp/), [HA WebSocket API](https://developers.home-assistant.io/docs/api/websocket/), [HA light capabilities](https://developers.home-assistant.io/docs/core/entity/light/), [keyring](https://docs.rs/keyring/3.6.3/keyring/).
