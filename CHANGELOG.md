# Changelog

All notable changes to `d2b-wlcontrol` are documented here. The
format follows [Keep a Changelog](https://keepachangelog.com/) and the
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed

- **Fast VM lifecycle.** Start and Restart actions now send d2bd
  `noWaitApi=true`, so the control surface returns success once the VM process
  is supervised and lets the normal status refresh show api-ready/readiness
  convergence later.
- **Fast status refresh.** `status-json` now consumes d2b's unfiltered
  daemon status read model in one public-socket request instead of running
  per-VM status and deep USB probe calls during UI refresh.
- **Force shutdown affordance.** The Quickshell popup keeps force shutdown out
  of the primary VM button, scaffolds it only inside ellipsis-expanded controls
  with destructive styling and strong two-click confirmation, and distinguishes
  graceful Stop messaging from force-stop messaging while the d2b
  force-stop contract lands.
- **d2bd refresh timeout.** Raised the default public-socket operation
  timeout to tolerate slower full-host status refreshes without reporting
  `daemon-down` while d2b status probes settle.
- **Quickshell VM card colors.** VM card borders again use d2b's per-VM
  active border colors, and the old environment-colored left stripe is gone.
- **d2b-only popup accents.** The Quickshell popup keeps its neutral
  black/white/gray shell colors, but no longer ships its own colored accent
  palette; if d2b does not provide a valid color artifact, affected colored
  accent/border surfaces render without color.
- **Quickshell popup placement.** The popup now opens with a larger top/right
  margin and more header icon spacing so it reads as intentionally placed on
  niri instead of stuck to the screen edge.
- **Waybar CSS color references.** Updated the starter stylesheet and docs
  to consume d2b's generated GTK `@define-color` names
  (`@d2b_state_*`) instead of legacy CSS custom properties.

### Added

- **d2b audio controls.** The public-socket client now consumes d2b
  `AudioStatus` and dispatches daemon-native audio mutations for microphone
  toggle, speaker toggle, speaker volume, microphone gain, and all-audio-off
  without touching broker or root-owned audio state files. The Quickshell popup
  shows per-VM audio state behind expanded controls, preserves levels across
  mute toggles, sends slider changes on commit, and marks host-only,
  unsupported, degraded, or provider-misconfigured entries inline.
- **Workspace and contract.** Rust workspace with `wlcontrol-core`
  (domain model, config, reducer, action planner), `wlcontrol-d2b`
  (public-socket client), `wlcontrol-waybar` (custom-module renderer),
  `wlcontrol-ui` (Quickshell layer-shell frontend), and `wlcontrol-cli`
  (the `d2b-wlcontrol` binary).
- **Live d2bd client.** Direct public-socket client speaking the
  non-abstract `SOCK_SEQPACKET` protocol: hello/version negotiation,
  4-byte little-endian length-prefixed JSON framing, typed responses,
  and translation of `auth status` / `list` / `status` / `usb probe`
  into a reduced control-surface state. A configured broker socket path
  is refused, and a mid-refresh failure degrades to daemon-down rather
  than reporting a false-healthy view.
- **Reduced state model.** Source-precedence reducer
  (`list` -> `status` -> `usb probe` -> `auth status`) with net-VM
  detection, favorites ordering, hidden-VM filtering, and
  inconsistency -> attention mapping.
- **Waybar module.** Continuous custom JSON module with compact and
  detail display modes, state-driven CSS classes, a rich per-VM tooltip
  (env, state, pending-restart, USB ownership), signal-driven refresh
  (`SIGRTMIN+8`), non-overlapping refresh, daemon-down backoff, and
  persisted display mode.
- **Quickshell control popup.** Top-right layer-shell popup with
  per-env VM cards, one-click show/hide behavior, Material-style action
  icons, first-class USB attach/detach chips, and auth-gated controls
  for start/stop/restart, terminal launch, switch, and store verify.
- **Control popup refinements.** The popup now uses a centered `d2b`
  heading, can be dragged after opening, fits its VM list up to a half-screen
  cap with a thin scrollbar, sorts `sys-*` VMs to the bottom, shows
  human-readable action feedback, exposes verify/build/boot/switch as
  icon-only system controls, supports config-driven per-VM quick-launch icons,
  launches guest terminals via detached exec, and adds a Signoz observability
  URL button without auto-login handling.
- **d2b color artifacts.** Waybar CSS imports
  `/etc/d2b/ui-colors.css`, and the Quickshell popup consumes parsed
  state, env, and VM border colors from the d2b UI color artifact.
- **Safety model.** Public socket only (never the broker socket), no
  `sudo`, no d2b state-file mutation, argv-only command execution,
  and authorization derived from `d2b auth status`.
- **Packaging and docs.** Nix flake (package/app/devShell with
  Quickshell + Material Symbols), CI gate, starter Waybar config + CSS,
  `AGENTS.md`, and the configuration / controls / Waybar / niri /
  security documentation set.
