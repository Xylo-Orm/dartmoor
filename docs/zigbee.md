# USB Zigbee

## Setup

Use a **Sonoff Zigbee 3.0 USB Dongle Plus Dongle-P (TI Z-Stack)**. Dongle-E/Ember is not supported. Install Python 3.12+ with `venv`; first use downloads the pinned [zigpy/zigpy-znp dependencies](../src/zigbee/requirements.txt).

1. Open **Devices → Add device → Zigbee → Discover**.
2. Put new bulbs into their manufacturer's pairing mode during the 60-second joining window. Existing paired devices load from the coordinator/database.
3. Select **Add** beside a color bulb, place it in Layout, and start Video or Music. White-only bulbs and sensor endpoints show an explanation instead.

No broker or Home Assistant is required. Bulbs on another network must be paired to this coordinator. The existing network is reused; one is formed only if none exists. Dartmoor does not flash firmware or reset an existing network. Keep the dongle exclusive to Dartmoor while connected.

On Linux, the desktop user needs read/write access to the serial device, usually through its device group (commonly `dialout`; sign out/in after changing membership). Do not run Dartmoor as root.

## Runtime and behavior

USB serial numbers identify saved coordinators, allowing tty numbering to change. Saved lights reconnect on startup. The coordinator stays open after Stop; closing Dartmoor closes joining, flushes the database and releases USB.

Zigbee receives one ambient color per light, at most twice per second, with matching 500 ms color/brightness transitions. Redundant commands are coalesced; fixed-hue dimming does not restart color transitions. Black fades to level zero. Fading on seeds color before raising brightness. Targets older than two seconds are discarded; Stop lets the last transition finish. **Test warm white** and **Turn off** are available in device properties while stopped.

The isolated Python runtime and database live under the OS-local data directory selected by `ProjectDirs::from("org", "dartmoor", "dartmoor")`, inside `zigbee/`. Debug builds can reuse `target/zigbee-runtime/bin/python`. Once dependencies are installed, connections work offline; a changed dependency list triggers installation again. `DARTMOOR_ZIGBEE_PYTHON` selects a preinstalled interpreter and bypasses automatic environment setup.

Radio-independent tests and installation commands are in [development](contributing.md). They do not establish USB compatibility or physical transitions on each platform. Implementation: [Rust session and scheduling](../src/zigbee.rs), [Python driver](../src/zigbee/helper.py).
