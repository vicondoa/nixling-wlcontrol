# Changelog

All notable changes to `nixling-wlcontrol` are documented here. The
format follows [Keep a Changelog](https://keepachangelog.com/) and the
project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- **Workspace and contract.** Rust workspace with `wlcontrol-core`
  (domain model, config, reducer, action planner), `wlcontrol-nixling`
  (public-socket client), `wlcontrol-waybar` (custom-module renderer),
  `wlcontrol-ui` (Quickshell layer-shell frontend), and `wlcontrol-cli`
  (the `nixling-wlcontrol` binary).
- **Live nixlingd client.** Direct public-socket client speaking the
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
  Audio controls remain hidden until nixling exposes a daemon-native audio
  control plane.
- **Control popup refinements.** The popup now uses a centered `nixling`
  heading, can be dragged after opening, fits its VM list up to a half-screen
  cap with a thin scrollbar, sorts `sys-*` VMs to the bottom, shows
  human-readable action feedback, exposes verify/build/boot/switch as
  icon-only system controls, supports config-driven per-VM quick-launch icons,
  launches guest terminals via detached exec, and adds a Signoz observability
  URL button without auto-login handling.
- **nixling color artifacts.** Waybar CSS imports
  `/etc/nixling/ui-colors.css`, and the Quickshell popup consumes parsed
  state, env, and VM border colors from the nixling UI color artifact with
  visible fallbacks for missing or malformed data.
- **Safety model.** Public socket only (never the broker socket), no
  `sudo`, no nixling state-file mutation, argv-only command execution,
  and authorization derived from `nixling auth status`.
- **Packaging and docs.** Nix flake (package/app/devShell with
  Quickshell + Material Symbols), CI gate, starter Waybar config + CSS,
  `AGENTS.md`, and the configuration / controls / Waybar / niri /
  security documentation set.
