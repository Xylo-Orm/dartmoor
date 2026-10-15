# Dartmoor

A native Rust desktop app that synchronizes lights with a screen/window or music. It supports addressable WLED strips over DDP, Home Assistant color lights, and USB Zigbee color bulbs. Video and Music share one layout and run one session at a time. Minimize to keep syncing; closing saves configuration and stops synchronization.

## Build and run

Install Rust through rustup. [rust-toolchain.toml](rust-toolchain.toml) pins the development and CI compiler; `Cargo.lock` pins dependencies. The declared Rust 1.88 minimum is not separately tested.

On Debian/Ubuntu, install the native libraries first:

```sh
sudo apt-get install libpipewire-0.3-dev libspa-0.2-dev libdbus-1-dev libclang-dev libxkbcommon-dev libwayland-dev libegl1-mesa-dev
cargo run --locked --release
```

On Windows, install Visual Studio C++ build tools and rustup's MSVC toolchain. On macOS, install Xcode command-line tools and rustup; capture requires macOS 12.3+ and Screen Recording permission. Use the same Cargo command.

Linux capture requires PipeWire, `xdg-desktop-portal`, the compositor's portal backend, and a graphical D-Bus session. Run as your desktop user. Start opens a chooser for one display or window; X11 capture is not implemented. Windows/macOS use native source selection. Build support does not establish native capture or hardware behavior; see [limitations](docs/KNOWN_ISSUES.md).

## First use

1. In **Devices → Add device**, inspect a WLED host, connect to Home Assistant and select discovered lights, or discover USB Zigbee bulbs. See [WLED and Home Assistant setup](docs/devices.md) or [USB Zigbee setup](docs/zigbee.md).
2. In **Layout**, position bulbs and strip handles over the sampling canvas. Adjust sampling extent, color zones, strip direction, and optional physical LED counts per path segment.
3. In **Video**, select a source and adjust Brightness, Color strength, Response, and dark-scene controls. For music, open **Music** and select Linux system playback or test audio, then choose Pulse or Alternating.
4. Use the bottom **Start Video/Music** button. Navigating workspaces keeps the session running; **Switch** explicitly changes its mode. **Stop** is available from every workspace. **Studio** is a placeholder.

**Save** persists all settings. **Configuration → Export configuration… / Import configuration…** copies or accepts JSON; credentials are excluded. Import can be undone and is saved only on Save or close. Invalid edits show an error and leave the active session on its last valid configuration.

## Documentation and checks

The [documentation index](docs/README.md) links configuration, device setup, architecture, and development guides. Run `bash scripts/check.sh` for local checks; see [contributing](docs/contributing.md) for dependencies, optional driver tests, and reproducible probes.
