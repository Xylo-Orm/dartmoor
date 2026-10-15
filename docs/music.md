# Music

## Sources and controls

Open **Music**, select a source, then **Start Music** (or **Switch to Music**). Lights and routes are shared with Video; video placement remains saved. Source/device changes require Stop; effects and dynamics apply live.

**System playback** captures a Linux PipeWire sink monitor. Use the default sink, **Refresh devices** to select another, or the expert node-name field. A missing saved sink produces an error. Windows/macOS have test audio only. **Test audio** supplies straight, hats, swing, tempo-change, quiet and silence-gap scenarios; changing a scenario requires restarting.

**Pulse** reacts to main attacks. **Alternating** cycles lights by stable ID or a saved explicit order. Sparkles add independent upper-band accents, using the main or an optional accent color. A second palette color supports Alternating and slow outputs. Per-light overrides provide enable, gain (0–2) and an optional effect. HA/Zigbee receive slow, smoothed energy updates rather than rapid flashes.

| Control | Range | Default |
|---|---|---|
| Sensitivity | 0–100% | 50% |
| Subbass weight | 0–100% | 20% |
| Sparkle amount | 0–100% | 35% |
| Brightness | 0–100% | 70% |
| Attack / decay | 0–500 / 20–2,000 ms | 20 / 240 ms |
| Auto tempo range | 40–240 BPM, min < max | 60–200 BPM |
| Manual BPM | 40–240 | 120 |
| Phase nudge | −0.5 to +0.5 beat | 0 |
| Follow beat | On/off | On |

**Auto** follows a confidence-gated beat grid. **Manual / Tap** uses the saved BPM; at least three taps establish a recent interval. Half/double, phase nudge and Resync adjust timing. Tap history and Resync are transient. Manual predictions arm from a real attack and stop after two unsupported predictions; silence and idle suppress predictions in both modes.

Classic detection, presets, timing offsets and Wave/Spectrum/Ambient effects are retired. See [configuration migration](configuration.md#saved-files-and-interchange).

## Analysis and diagnostics

The Advanced analyzer uses stereo spectral power from a 2,048-sample Hann FFT at 48 kHz, with a 480-frame (10 ms) hop. Bands cover approximately 35–90 Hz, 90–300 Hz, 300 Hz–4 kHz and 4–12 kHz. Lower bands drive main attacks; air drives sparkles. Repeated aligned attacks support tempo tracking; off-grid attacks still react directly. Frequency bands do not identify instruments, and confidence is not measured recognition accuracy.

Events carry sample time, observed/predicted provenance and visual beat identity, so a late attack can confirm a prediction without duplicating its flash. The event queue holds 128 entries and drops the oldest on overflow. Source gaps reset continuity and advance the capture epoch. Queue age measures time since callback receipt, not calibrated playback latency. Output defaults to 30 Hz; saved `output_fps` also accepts 60, subject to device sender rates.

The workspace shows features, tracking state, events and light colors. JSON/CSV diagnostics exports contain no audio samples. Ownership: [settings](../src/music/settings.rs), [PipeWire capture](../src/music/linux.rs), [spectral features](../src/music/spectral.rs), [analyzer](../src/music/advanced.rs), [onsets](../src/music/onset.rs), [beat tracker](../src/music/beat.rs), [effects](../src/music/effects.rs), [worker](../src/engine/music.rs), [diagnostics export](../src/app/music_diagnostics.rs).

## Offline replay

The [music_replay](../src/bin/music_replay.rs) binary opens no capture or output devices:

```sh
cargo run --locked --release --bin music_replay -- --synthetic straight
cargo run --locked --release --bin music_replay -- --demo hats
cargo run --locked --release --bin music_replay -- recording.wav annotations.json
```

Synthetic scenarios: `straight`, `quiet`, `compressed`, `stereo`, `sustained`, `tempo_step`, `missing`, `swing`, `silence`. Demo scenarios: `straight`, `hats`, `swing`, `tempo_step`, `quiet`, `silence_gap`.

WAV input supports 48 kHz mono/stereo PCM16 or F32, capped at 128 MiB. Annotations are JSON arrays of seconds, for example `{"onsets":[0.5,1.0],"beats":[0.5,1.0]}`. Reports include events, features, one-to-one metrics and render timing. They do not measure device latency or establish real-music accuracy from synthetic fixtures.
