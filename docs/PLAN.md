# Implementation plan

One Rust crate, functional sampling core; one capture/processing thread; Tokio tasks own each output connection; UI runs on main thread. Commands are bounded, frames/colors/status latest-value. Capture source is identified in versioned config. Scope SDR/sRGB, CPU image <=160x90, linear-light sampling, time-based smoothing.

1. Inspect and validate scap; patch narrow upstream queue, timeout and Linux safety/lifecycle defects behind capture interface.
2. Capture -> immutable sampling plan -> independent WLED DDP/mock outputs.
3. Complete native placement editor, HA WebSocket/keyring adapter, atomic JSON persistence and lifecycle.
4. Tests, formatting, lint, release build, runtime probes and measurements.
5. Two independent reviewers; lower score controls acceptance, at most three revisions.

Assumptions: no physical lights or HA credentials supplied. Manual WLED address addition meets discovery-or-add requirement. Linux selection uses the desktop portal, rather than claiming scap enumerates Linux displays. Closing editor shuts down synchronization; minimizing keeps sync independent of repaint. No inaccessible background process. Network dependency downloads require sandbox elevation. Linux runtime is this Wayland desktop; other OS evidence comes from CI, not implied runtime testing.

Dependencies: stable scap 0.0.8 selected over beta; eframe 0.33.3 MSRV1.88 works with installed Rust1.96; Tokio1, Serde1, reqwest0.12, keyring3.6. Primary sources recorded in README. Vendored scap fixes documented separately.
