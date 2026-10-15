# Vendored capture backend

[Cargo.toml](../Cargo.toml) uses the local [scap 0.0.8](../vendor/scap/) source. Its MIT license and upstream README are retained; the upstream example binary is omitted. Keep the local changes when updating this dependency.

## Local changes

- **Bounded delivery:** Windows uses a one-slot channel; Linux reserves a single pending slot before processing pixels. macOS uses an overwrite mailbox that protects images/invalidations from Idle notifications. Callbacks never wait for queue capacity.
- **Timed reads and lifecycle:** `get_next_frame_timeout` distinguishes timeout from disconnection. Start/stop/drop are guarded. A stopped Linux capturer is terminal and must be recreated. Linux errors and phase state belong to each instance.
- **Portal ownership:** Linux opens PipeWire through the portal's authorized remote descriptor, retains the D-Bus session and explicitly closes it on teardown/setup failure. Cancellation/setup errors propagate as backend errors.
- **Linux frames and pacing:** RGB, RGBx, xBGR and BGRx rows are checked for dimensions, pointers, positive stride, chunk bounds and metadata size. Negotiation bounds fixed/max FPS; callback pacing skips excess frames without catch-up bursts. PipeWire uses the 0.10.1 bindings.
- **Mapped processing:** the Linux-only `Options::mapped_frame_processor` borrows validated rows before returning the PipeWire buffer. Dartmoor reduces these to at most 160×90 cells with 4×4 color taps, avoiding a full-resolution allocation/copy. The default scap path still produces full-resolution frames. Diagnostics report negotiated format/rate, incoming/delivered FPS and callback processing time.
- **Windows conversion:** removes row padding, uses arriving dimensions, clamps explicit crops and validates lengths. The backend uses `windows-capture` 1.5.0 with BGRA and supported update-interval settings.
- **macOS conversion:** guards CoreVideo locks, dimensions, pointers and strides. Only validated ScreenCaptureKit Idle status permits cached-image reuse; invalid/missing/blank/suspended/stopped states invalidate it. Conversion errors propagate. The `BGR0` variant contains three bytes per pixel after alpha removal.

## Maintenance and limits

Run vendor library tests on every platform; Linux also runs standalone formatting and strict lint through [the check script](../scripts/check.sh) and [CI](../.github/workflows/ci.yml). Shared mailbox/layout tests do not exercise macOS frameworks or native Windows capture.

Portal selection remains synchronous and timed frame reads cannot cancel it. Linux delegates selection to the portal rather than enumerating sources or providing an X11 fallback. Linux/Windows lack explicit Idle liveness. CPU mapping still requires compositor GPU readback; GPU reduction is not implemented. Native startup can panic, so the application contains capture failures at its worker boundary. Stop errors are best effort during teardown.
