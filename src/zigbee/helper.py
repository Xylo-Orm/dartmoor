"""Dartmoor-owned USB Zigbee session. JSON lines are the only stdout protocol."""
import asyncio
import colorsys
import json
import logging
import os
from pathlib import Path
import sys
import time


def adapters():
    from serialx import list_serial_ports
    result = []
    for port in list_serial_ports():
        # CP210x alone is not a Zigbee identity: never probe unrelated UARTs.
        product = (port.product or "").lower()
        if "sonoff" in product and "zigbee" in product:
            result.append({"id": port.serial_number or port.device,
                           "name": port.product, "path": port.device,
                           "supported": port.vid == 0x10C4 and port.pid == 0xEA60
                           and "plus v2" not in product})
    return result


def light_target(rgb, caps):
    """Translate the renderer's RGB frame into chromaticity and a native level.

    Brightness is separated before conversion, so dimming a fixed hue does not
    also move the bulb's color or require resending its chromaticity command.
    """
    if len(rgb) != 3 or any(type(c) is not int or not 0 <= c <= 255 for c in rgb):
        raise ValueError("Invalid RGB command")
    brightness = max(rgb)
    if brightness == 0:
        return None, 0
    normalized = [c / brightness for c in rgb]
    level = max(1, round(brightness * 254 / 255))
    if caps & 0x08:
        linear = [((c + .055) / 1.055) ** 2.4 if c > .04045 else c / 12.92 for c in normalized]
        r, g, b = linear
        x = .664511 * r + .154324 * g + .162028 * b
        y = .283881 * r + .668433 * g + .047685 * b
        z = .000088 * r + .072310 * g + .986039 * b
        total = x + y + z
        return ("xy", round(x / total * 65535), round(y / total * 65535)), level
    if caps & 0x01:
        h, saturation, _ = colorsys.rgb_to_hsv(*normalized)
        return ("hs", round(h * 254), round(saturation * 254)), level
    raise ValueError("Bulb has no supported RGB color control")


def same_chromaticity(previous, target):
    if previous is None or target is None or previous[0] != target[0]:
        return previous == target
    tolerance = 64 if target[0] == "xy" else 1
    return all(abs(a - b) <= tolerance for a, b in zip(previous[1:], target[1:]))


def emit(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


class Session:
    def __init__(self, data):
        self.data = Path(data)
        self.app = None
        self.adapter = None
        self.status = "Not connected"
        self.capabilities = {}
        self.tasks = set()
        self.join_until = 0
        self.disconnected = False
        self.applied_targets = {}
        self.off_until = {}

    def changed(self):
        emit({"event": "devices", "snapshot": self.snapshot()})

    def device_initialized(self, device):
        task = asyncio.create_task(self.inspect(device))
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)

    def device_joined(self, device):
        self.changed()

    def device_left(self, device):
        self.changed()

    def connection_lost(self, exc):
        self.disconnected = True
        self.status = "USB disconnected. Reconnect the coordinator and press Discover."
        self.changed()

    async def inspect(self, device):
        for endpoint in device.endpoints.values():
            clusters = getattr(endpoint, "in_clusters", {})
            color = clusters.get(0x0300)
            if color is not None:
                try:
                    attrs, _ = await asyncio.wait_for(color.read_attributes(["color_capabilities"]), 5)
                    self.capabilities[(str(device.ieee), endpoint.endpoint_id)] = int(attrs.get("color_capabilities", 0))
                except Exception:
                    self.capabilities[(str(device.ieee), endpoint.endpoint_id)] = 0
        self.changed()

    def snapshot(self):
        rows = []
        if self.app:
            for device in list(self.app.devices.values()):
                if device.nwk == 0:
                    continue
                for endpoint in device.endpoints.values():
                    clusters = getattr(endpoint, "in_clusters", {})
                    if not clusters:
                        continue
                    caps = self.capabilities.get((str(device.ieee), endpoint.endpoint_id), 0)
                    # Read the cached attribute on a restored device before re-interview.
                    if not caps and 0x0300 in clusters:
                        caps = int(clusters[0x0300].get("color_capabilities", 0) or 0)
                    reason = None
                    if not all(c in clusters for c in (0x0006, 0x0008, 0x0300)):
                        reason = "No RGB light control"
                    elif not caps & 0x09:
                        reason = "Checking color capability" if not caps else "White-only light"
                    rows.append({"ieee_address": str(device.ieee), "endpoint": endpoint.endpoint_id,
                                 "name": " ".join(filter(None, [device.manufacturer, device.model])) or str(device.ieee),
                                 "reason": reason})
        return {"adapter": self.adapter, "status": self.status,
                "pairing": time.monotonic() < self.join_until, "devices": rows[:1024]}

    async def connect(self, coordinator=None):
        if self.app and self.disconnected:
            await self.app.shutdown()
            self.app = None
        if self.app:
            if coordinator and coordinator != self.adapter["id"]:
                raise ValueError("A different coordinator is already connected")
            return
        found = adapters()
        if coordinator:
            found = [p for p in found if p["id"] == coordinator]
        supported = [p for p in found if p["supported"]]
        if not supported:
            raise ValueError("No supported Sonoff USB Zigbee coordinator found. Connect a Sonoff Dongle-P.")
        if len(supported) != 1:
            raise ValueError("Multiple Sonoff coordinators found. Leave the one you want connected.")
        adapter = supported[0]
        if os.name != "nt" and not os.access(adapter["path"], os.R_OK | os.W_OK):
            raise PermissionError(f"USB access needed for {adapter['path']}. Grant your account access to the dialout group, then sign out and back in.")
        from zigpy_znp.zigbee.application import ControllerApplication
        # A database per physical USB identity survives tty number changes.
        import hashlib
        directory = self.data / hashlib.sha256(adapter["id"].encode()).hexdigest()[:24]
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.status = "Connecting to " + adapter["name"]
        self.adapter = adapter
        self.changed()
        app = await ControllerApplication.new({
            "device": {"path": adapter["path"], "baudrate": 115200},
            "database_path": str(directory / "devices.sqlite"),
            "ota": {"enabled": False},
        }, auto_form=False, start_radio=False)
        self.app = app
        self.applied_targets.clear()
        self.off_until.clear()
        self.disconnected = False
        app.add_listener(self)
        try:
            # Reuse existing network settings. Form only when the radio has no
            # network; never reset or overwrite an existing coordinator network.
            await asyncio.wait_for(app.startup(auto_form=True), 60)
            await asyncio.wait_for(app.load_network_info(load_devices=True), 30)
            for ieee, nwk in app.state.network_info.nwk_addresses.items():
                if ieee not in app.devices and int(nwk) < 0xFFF8:
                    device = app.add_device(ieee, nwk)
                    device.schedule_initialize()
            for device in list(app.devices.values()):
                if device.nwk != 0:
                    self.device_initialized(device)
            self.status = "Connected to " + adapter["name"]
            self.changed()
        except BaseException:
            await app.shutdown()
            self.app = None
            raise

    async def discover(self, coordinator=None):
        await self.connect(coordinator)
        await self.app.permit(60)
        self.join_until = time.monotonic() + 60
        async def finish_pairing():
            await asyncio.sleep(60)
            if time.monotonic() >= self.join_until:
                await self.app.permit(0)
                self.status = "Connected · discovery finished"
                self.changed()
        task = asyncio.create_task(finish_pairing())
        self.tasks.add(task)
        task.add_done_callback(self.tasks.discard)
        self.status = "Connected · discovering for 60 seconds"
        # Refresh existing endpoints; newly joined devices arrive via listeners.
        for device in list(self.app.devices.values()):
            if device.nwk != 0:
                if not device.is_initialized:
                    device.schedule_initialize()
                else:
                    self.device_initialized(device)
        self.changed()
        return self.snapshot()

    async def color(self, request):
        import zigpy.types as t
        from zigpy.zcl.foundation import Status
        await self.connect(request["coordinator"])
        # Commands that spent too long queued must never replay stale colors.
        if time.time() - request["produced"] > 2:
            return {"stale": True}
        device = self.app.devices.get(t.EUI64.convert(request["ieee_address"]))
        if device is None:
            raise ValueError("Saved Zigbee device is not on this coordinator. Press Discover.")
        endpoint = device.endpoints[request["endpoint"]]
        clusters = endpoint.in_clusters
        caps = self.capabilities.get((str(device.ieee), endpoint.endpoint_id), 0)
        caps = caps or int(clusters[0x0300].get("color_capabilities", 0) or 0)
        target_color, target_level = light_target(request["rgb"], caps)
        transition_ms = request.get("transition_ms", 500)
        if type(transition_ms) is not int or not 0 <= transition_ms <= 60000:
            raise ValueError("Invalid Zigbee transition duration")
        # ZCL transition time is in tenths of a second, not milliseconds.
        transition = (transition_ms + 50) // 100
        key = (str(device.ieee), endpoint.endpoint_id)
        previous = self.applied_targets.get(key)
        force = request.get("force", True)
        old_color, old_level = previous if previous is not None else (None, None)

        async def command(awaitable):
            result = await asyncio.wait_for(awaitable, 5)
            result_status = getattr(result, "status", None)
            if result_status is None and isinstance(result, (tuple, list)) and result:
                result_status = result[-1] if isinstance(result[-1], Status) else None
            if result_status is not None and result_status != Status.SUCCESS:
                raise ValueError("Bulb rejected Zigbee command: " + str(result_status))

        try:
            if target_level == 0:
                if force or old_level != 0:
                    if transition:
                        # Native level-zero transition fades AND switches off.
                        # No delayed off callback can race a newer color target.
                        await command(clusters[0x0008].move_to_level_with_on_off(0, transition))
                    else:
                        await command(clusters[0x0006].off())
                    self.off_until[key] = time.monotonic() + transition / 10
                self.applied_targets[key] = (old_color, 0)
            else:
                # A requested level-zero target can still be fading. Retarget
                # its live color smoothly until the off transition has finished.
                was_off = old_level == 0 and time.monotonic() >= self.off_until.get(key, 0)
                if previous is None:
                    try:
                        attributes, _ = await asyncio.wait_for(clusters[0x0006].read_attributes(["on_off"]), 2)
                        was_off = attributes.get("on_off") == 0
                    except Exception:
                        pass
                # A fade from black seeds chromaticity while invisible, then
                # fades the level up. ExecuteIfOff avoids an old-color flash.
                color_transition = 0 if was_off else transition
                if force or not same_chromaticity(old_color, target_color):
                    kind, first, second = target_color
                    if kind == "xy":
                        await command(clusters[0x0300].move_to_color(first, second, color_transition, 1, 1))
                    else:
                        await command(clusters[0x0300].move_to_hue_and_saturation(first, second, color_transition, 1, 1))
                    old_color = target_color
                if force or old_level is None or old_level == 0 or abs(old_level - target_level) > 1:
                    await command(clusters[0x0008].move_to_level_with_on_off(target_level, transition))
                    old_level = target_level
                self.applied_targets[key] = (old_color, old_level)
                self.off_until.pop(key, None)
        except BaseException:
            # A partial command sequence is not a committed target. Retry both
            # channels after errors rather than trusting stale cached state.
            self.applied_targets.pop(key, None)
            self.off_until.pop(key, None)
            raise
        return {"applied": True}

    async def close(self):
        for task in self.tasks:
            task.cancel()
        await asyncio.gather(*self.tasks, return_exceptions=True)
        if self.app:
            try:
                await asyncio.wait_for(self.app.permit(0), 2)
            finally:
                await self.app.shutdown()
                self.app = None


async def main():
    if "--scan" in sys.argv:
        emit({"adapters": adapters()})
        return
    session = Session(sys.argv[1])
    try:
        while line := await asyncio.to_thread(sys.stdin.readline):
            request = json.loads(line)
            try:
                op = request["op"]
                if op == "discover":
                    result = await session.discover(request.get("coordinator"))
                elif op == "connect":
                    await session.connect(request["coordinator"])
                    result = session.snapshot()
                elif op == "color":
                    result = await session.color(request)
                elif op == "snapshot":
                    result = session.snapshot()
                elif op == "shutdown":
                    break
                else:
                    raise ValueError("Unknown Zigbee operation")
                emit({"id": request["id"], "result": result})
            except Exception as exc:
                session.status = str(exc)
                session.changed()
                emit({"id": request["id"], "error": str(exc)})
    finally:
        await session.close()


if __name__ == "__main__":
    logging.basicConfig(stream=sys.stderr, level=logging.ERROR)
    asyncio.run(main())
