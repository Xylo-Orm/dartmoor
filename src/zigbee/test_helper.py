"""Radio-independent tests for USB filtering and actual Zigbee light commands."""
import importlib.util
from pathlib import Path
import time
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

spec = importlib.util.spec_from_file_location("helper", Path(__file__).with_name("helper.py"))
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


class DriverTests(unittest.IsolatedAsyncioTestCase):
    def make_session(self, caps=8):
        import zigpy.types as t
        from zigpy.zcl.foundation import Status
        ieee = "00:11:22:33:44:55:66:77"
        color = SimpleNamespace(get=lambda *_: caps,
                                move_to_color=AsyncMock(return_value=[7, Status.SUCCESS]),
                                move_to_hue_and_saturation=AsyncMock(return_value=[6, Status.SUCCESS]))
        level = SimpleNamespace(move_to_level_with_on_off=AsyncMock(return_value=[4, Status.SUCCESS]))
        onoff = SimpleNamespace(off=AsyncMock(return_value=[0, Status.SUCCESS]),
                                read_attributes=AsyncMock(return_value=({"on_off": 1}, {})))
        endpoint = SimpleNamespace(endpoint_id=1, in_clusters={6: onoff, 8: level, 768: color})
        device = SimpleNamespace(ieee=t.EUI64.convert(ieee), nwk=42,
                                 manufacturer="innr", model="RB 285 C", endpoints={1: endpoint})
        session = helper.Session("/unused")
        session.app = SimpleNamespace(devices={device.ieee: device})
        session.adapter = {"id": "sonoff"}
        session.connect = AsyncMock()
        return session, color, level, onoff, {"coordinator": "sonoff", "ieee_address": ieee,
                                              "endpoint": 1, "rgb": [20, 40, 80], "produced": time.time(),
                                              "transition_ms": 500, "force": False}

    async def test_innr_color_uses_xy_and_separate_brightness(self):
        session, color, level, onoff, request = self.make_session()
        self.assertEqual(await session.color(request), {"applied": True})
        x, y, transition, mask, override = color.move_to_color.call_args.args
        self.assertTrue(0 < x < 65535 and 0 < y < 65535)
        self.assertEqual(transition, 5)
        self.assertEqual((mask, override), (1, 1))
        level.move_to_level_with_on_off.assert_awaited_once_with(80, 5)
        onoff.off.assert_not_awaited()

    async def test_hue_saturation_and_black_off(self):
        session, color, level, onoff, request = self.make_session(1)
        request["rgb"] = [0, 255, 0]
        await session.color(request)
        color.move_to_hue_and_saturation.assert_awaited_once_with(85, 254, 5, 1, 1)
        request["rgb"] = [0, 0, 0]
        await session.color(request)
        onoff.off.assert_not_awaited()
        level.move_to_level_with_on_off.assert_awaited_with(0, 5)
        self.assertEqual(level.move_to_level_with_on_off.await_count, 2)

    async def test_stale_colors_never_reach_radio(self):
        session, color, level, onoff, request = self.make_session()
        request["produced"] -= 3
        self.assertEqual(await session.color(request), {"stale": True})
        color.move_to_color.assert_not_awaited()
        level.move_to_level_with_on_off.assert_not_awaited()
        onoff.off.assert_not_awaited()

    async def test_white_only_and_sensors_are_not_selectable(self):
        session, *_ = self.make_session(16)
        self.assertEqual(session.snapshot()["devices"][0]["reason"], "White-only light")
        next(iter(session.app.devices.values())).endpoints[1].in_clusters = {1026: object()}
        self.assertEqual(session.snapshot()["devices"][0]["reason"], "No RGB light control")

    async def test_bulb_rejection_is_reported(self):
        from zigpy.zcl.foundation import Status
        session, color, level, _, request = self.make_session()
        color.move_to_color.return_value = [7, Status.FAILURE]
        with self.assertRaisesRegex(ValueError, "rejected"):
            await session.color(request)
        level.move_to_level_with_on_off.assert_not_awaited()

    async def test_dimming_fixed_hue_does_not_restart_color_fade(self):
        session, color, level, _, request = self.make_session()
        await session.color(request)
        request["rgb"] = [40, 80, 160]
        await session.color(request)
        self.assertEqual(color.move_to_color.await_count, 1)
        self.assertEqual(level.move_to_level_with_on_off.await_count, 2)
        level.move_to_level_with_on_off.assert_awaited_with(159, 5)

    async def test_repeated_and_sub_level_changes_are_coalesced(self):
        session, color, level, _, request = self.make_session()
        request["rgb"] = [100, 100, 100]
        await session.color(request)
        await session.color(request)
        request["rgb"] = [101, 101, 101]
        await session.color(request)
        self.assertEqual(color.move_to_color.await_count, 1)
        self.assertEqual(level.move_to_level_with_on_off.await_count, 1)
        request["force"] = True
        await session.color(request)
        self.assertEqual(color.move_to_color.await_count, 2)
        self.assertEqual(level.move_to_level_with_on_off.await_count, 2)

    async def test_fade_to_black_can_be_retargeted_without_a_delayed_off(self):
        session, color, level, onoff, request = self.make_session()
        request["rgb"] = [0, 0, 0]
        await session.color(request)
        level.move_to_level_with_on_off.assert_awaited_once_with(0, 5)
        request["rgb"] = [0, 255, 0]
        await session.color(request)
        self.assertEqual(color.move_to_color.call_args.args[2:], (5, 1, 1))
        level.move_to_level_with_on_off.assert_awaited_with(254, 5)
        # The native level command owns off timing. No queued off callback can
        # extinguish the new target or keep the serial actor blocked for a fade.
        onoff.off.assert_not_awaited()
        self.assertFalse(session.tasks)

    async def test_completed_off_fade_seeds_color_while_dark(self):
        session, color, level, _, request = self.make_session()
        request["rgb"] = [0, 0, 0]
        with patch.object(helper.time, "monotonic", return_value=100):
            await session.color(request)
        request["rgb"] = [0, 255, 0]
        with patch.object(helper.time, "monotonic", return_value=101):
            await session.color(request)
        self.assertEqual(color.move_to_color.call_args.args[2:], (0, 1, 1))
        level.move_to_level_with_on_off.assert_awaited_with(254, 5)

    async def test_initial_off_bulb_seeds_new_color_before_fading_on(self):
        session, color, level, onoff, request = self.make_session()
        onoff.read_attributes.return_value = ({"on_off": 0}, {})
        await session.color(request)
        self.assertEqual(color.move_to_color.call_args.args[2:], (0, 1, 1))
        level.move_to_level_with_on_off.assert_awaited_once_with(80, 5)

    async def test_explicit_zero_transition_still_supports_immediate_off(self):
        session, _, level, onoff, request = self.make_session()
        request.update(rgb=[0, 0, 0], transition_ms=0)
        await session.color(request)
        onoff.off.assert_awaited_once()
        level.move_to_level_with_on_off.assert_not_awaited()

    async def test_partial_failure_does_not_cache_uncommitted_target(self):
        session, color, level, _, request = self.make_session()
        level.move_to_level_with_on_off.side_effect = TimeoutError("radio")
        with self.assertRaises(TimeoutError):
            await session.color(request)
        self.assertFalse(session.applied_targets)
        level.move_to_level_with_on_off.side_effect = None
        await session.color(request)
        self.assertEqual(color.move_to_color.await_count, 2)

    async def test_transition_duration_is_validated_before_commands(self):
        session, color, level, _, request = self.make_session()
        for invalid in [-1, 60001, "500", True]:
            request["transition_ms"] = invalid
            with self.assertRaisesRegex(ValueError, "duration"):
                await session.color(request)
        color.move_to_color.assert_not_awaited()
        level.move_to_level_with_on_off.assert_not_awaited()

    def test_discovery_does_not_open_unrelated_cp210x_uart(self):
        ports = [SimpleNamespace(product="CP2102 UART", vid=0x10C4, pid=0xEA60),
                 SimpleNamespace(product="Sonoff Zigbee 3.0 USB Dongle Plus", vid=0x10C4,
                                 pid=0xEA60, serial_number="stable-id", device="/dev/ttyUSB2")]
        with patch("serialx.list_serial_ports", return_value=ports):
            self.assertEqual(helper.adapters(), [{"id": "stable-id", "name": ports[1].product,
                                                    "path": "/dev/ttyUSB2", "supported": True}])


if __name__ == "__main__":
    unittest.main()
