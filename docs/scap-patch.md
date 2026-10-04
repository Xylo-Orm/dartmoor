# Local scap patch

`vendor/scap` contains the MIT-licensed source of `scap` 0.0.8 from crates.io,
with its license and upstream README retained. The upstream example binary is
omitted. The application's dependency points at this directory so a registry
update cannot silently remove these changes. It still uses scap's native
ScreenCaptureKit, Windows Graphics Capture, and portal/PipeWire backends.

## Changes

- Linux/Windows use a one-slot synchronous frame channel. Native callbacks
  call `try_send`; when occupied the incoming frame is discarded. macOS uses
  a one-slot overwrite mailbox: new images replace older samples, and Idle
  notifications cannot replace a pending image or invalidation. Native sample
  destruction happens outside its short mutex section. No callback waits for
  queue capacity or converts pixels; the worker owns conversion. Backlog is bounded.
- `Capturer::get_next_frame_timeout(Duration)` returns
  `Result<Option<Frame>, std::sync::mpsc::RecvTimeoutError>`. An elapsed timeout
  returns `Ok(None)`; producer disconnection returns `Err(Disconnected)`.
  Linux/Windows check the queued frame once for a newer arrival, without
  indefinitely draining an active producer. macOS takes the mailbox's selected
  sample without draining a later Idle over it. One deadline applies across
  skipped samples. Native macOS errors also end ordinary and timed reads.
  The original blocking API remains available.
- Engine start/stop is guarded; dropping an engine stops an active capture.
  Windows stop accepts an already stopped capture. Linux also stops and joins
  its worker when dropped before it has ever started. A stopped Linux capturer
  is terminal; create a fresh capturer for the next session.
- Linux capture phase and stream-error flags belong to each instance, avoiding
  cross-session global state. Portal cancellation/setup errors propagate through
  `CapturerBuildError::Backend(String)` instead of panicking in `build`.
- Linux obtains the authorized `OpenPipeWireRemote` file descriptor from the
  portal and transfers its ownership to PipeWire. It keeps the D-Bus connection
  and session alive and explicitly closes the session on drop or setup failure.
- Linux accepts only formats its callback handles (RGB, RGBx, xBGR, BGRx).
  It validates null pointers, dimensions, buffer/chunk bounds and positive row
  stride, then copies rows without padding and with the chunk offset applied.
  Invalid, truncated, wrapping, negative-stride and unsupported frames are
  dropped. Every dequeued buffer is returned, and null buffers are not queued.
  Header timestamps are read only from a sufficiently sized, non-null metadata
  block. Negotiation permits dimensions up to 16384 pixels per axis.
- Windows' uncropped path also strips native row padding. With no explicit crop,
  it uses each arriving frame's actual dimensions rather than a static window
  rectangle. Explicit crops are checked and clamped to the current frame, and
  metadata is derived from the resulting buffer. Invalid packed lengths fail
  the callback instead of mislabeling frame data.
- Windows requires `windows-capture` 1.5.0 and supplies its additional secondary
  window, update interval and dirty-region settings. The FPS interval is set
  only when supported by the running Windows version; otherwise the OS default
  applies. The native format is always BGRA to match the emitted frame variant.
  `BGR0` in scap's macOS backend contains three bytes per pixel after alpha
  removal, despite its variant name.
- macOS sample conversion propagates missing/invalid pixel buffers instead of
  unwrapping conversion failures. A guard unlocks each successfully locked
  CoreVideo buffer, including early error returns. Active converters reject
  null pointers, failed locks, invalid dimensions/strides and length overflow.
  ScreenCaptureKit Idle samples retain their zero-sized BGRA marker; the
  application treats only this exact marker as confirmed unchanged-image
  liveness. Raw status metadata must contain a dictionary and numeric, known
  status; the wrapper's missing-metadata default of Idle is not trusted.
  Failed conversion and invalid/missing/blank/suspended/stopped statuses
  invalidate cached-image reuse until a valid image arrives. A timeout is
  still silence. These changes were inspected against
  local CoreVideo bindings and Apple documentation; their macOS compilation
  and runtime have not been tested in this local increment.
- The Linux dependency is `pipewire` 0.10.1 (Rust 1.80 minimum) with corresponding
  ownership/timeout API changes. The original 0.8 bindings fail against this
  machine's PipeWire 1.6.8 headers: generated `spa_pod_builder` is opaque while
  libspa accesses its fields. The maintained bindings compile with these headers.

## Verification

On Linux with PipeWire 1.6.8:

```sh
cargo test --manifest-path vendor/scap/Cargo.toml --lib
```

Eighteen tests pass: five existing image/layout helper tests plus thirteen
shared mailbox/frame-state tests compiled on Linux. They cover priority/latest
delivery, image and invalidation protection against Idle, timeout/disconnection,
waiter wakeups, safe sample destruction and cached-image eligibility. Image tests
cover an offset plus row padding, truncated chunks, out-of-map offsets,
short/negative strides, overflow and empty dimensions. These shared tests do not
compile or run macOS framework calls. Historically,
`cargo check --locked --offline -p scap --target x86_64-pc-windows-gnu`
also passes from Linux against locked `windows-capture` 1.5.0. Linux
`cargo check -p scap` and `cargo clippy -p scap -- -D warnings` pass without
warnings. Full application cross-compilation additionally needs a Windows C
compiler for `ring`; this machine lacks `x86_64-w64-mingw32-gcc`. Native
Windows/macOS runtime behavior and macOS compilation still require their
respective operating systems. No live desktop permission request
is initiated by these unit tests.

## Remaining upstream limitations

Portal selection is synchronous inside `build` and can wait for the chooser
response (up to the upstream timeout). The application's worker must own this
operation so the GUI stays responsive; the frame timeout does not cancel an
open chooser. Portal response subscription behavior is otherwise upstream.
Linux options such as explicit target/crop/output resolution remain limited by
scap's implementation; selection is delegated to the portal. Other native
construction/start paths can still panic; callers should contain those failures
at their worker boundary. Backend stop errors are best effort during teardown.
Linux/Windows one-slot drop-on-full channels bound memory, but an occupied slot
retains the older frame until consumed. Their next-frame draining mitigates this
when another image arrives; they do not currently provide explicit Idle liveness.
