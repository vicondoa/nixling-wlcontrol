# d2b-wlcontrol

A clean, Waybar-styled indicator and control center for
[d2b: Double Dutch Bus](https://github.com/vicondoa/d2b), built for a
niri / Wayland desktop where multiple worlds share one desktop.

`d2b-wlcontrol` shows which d2b realms are running and surfaces the
controls a d2b operator can already drive — start / stop / restart,
detached terminal launch, attach / detach USB, verify / build / boot /
switch, and observability portal open — without ever touching anything privileged. It talks only to the
d2bd **public** socket and, where it is the better boundary, the
official `d2b` CLI.

> Status: the Waybar indicator, the live d2bd public-socket client,
> the reduced status model, auth-gated action planning, and the Quickshell
> layer-shell popup are all in place. Audio mic/speaker controls remain out of
> the popup pending a daemon-native d2b audio control plane.

## What it does

- **Glanceable status in Waybar** — a compact `◆ 2/4` style indicator
  with state-driven CSS classes (`all-running`, `partial-running`,
  `attention`, `daemon-down`, `auth-denied`) and a per-VM tooltip.
- **A Quickshell layer-shell control popup** — per-VM cards with lifecycle
  controls, graceful stop as the primary Stop action, detached terminal launch,
  USB attach/detach, store verify/build/boot/switch icons, config-driven
  quick-launch icons, and an observability portal button, all gated on your
  effective d2b authorization.
- **d2b-native colors** — Waybar CSS consumes d2b's generated
  `/etc/d2b/ui-colors.css` GTK `@define-color` names, while the popup
  keeps neutral shell colors local and consumes `/etc/d2b/ui-colors.json`
  for colored accent/border surfaces.
- **Safe by construction** — public socket only; no broker socket, no
  `sudo`, no direct state-file mutation, argv-only command execution.

Audio (mic / speaker) controls are intentionally not rendered until d2b
exposes a daemon-native audio control plane — today those d2b verbs return
`not-yet-implemented`.

## Install

`d2b-wlcontrol` is a Nix flake.

```bash
# Run it directly
nix run github:vicondoa/d2b-wlcontrol -- status-json

# Or add it as an input and install the package
#   inputs.d2b-wlcontrol.url = "github:vicondoa/d2b-wlcontrol";
# then add `d2b-wlcontrol.packages.${system}.default` to your packages.
```

For development:

```bash
nix develop          # rust toolchain + quickshell
cargo test --workspace
```

## Waybar setup

The Waybar module and its click bindings invoke the `d2b-wlcontrol`
binary by name, so **install the package** (so it is on your `PATH`)
rather than relying on `nix run` for the bar. Then print a starter
module config and CSS:

```bash
d2b-wlcontrol print-waybar-config   # add to your Waybar "modules" + a "modules-*" array
d2b-wlcontrol print-css             # append to your style.css
```

The module is a continuous custom module — it loops and emits one JSON
line per refresh, so do **not** give it an `interval`. Left-click opens
the control center, right-click cycles the display mode, middle-click
refreshes. See [docs/waybar.md](./docs/waybar.md).

## niri setup

No niri window rule is required: `d2b-wlcontrol open` uses
Quickshell's layer-shell surface as a draggable top-right popup. See
[docs/niri.md](./docs/niri.md).

## Configuration

Configuration is TOML at
`${XDG_CONFIG_HOME:-~/.config}/d2b-wlcontrol/config.toml`. All
defaults are sane; common overrides are the detached guest terminal command and
observability URL.
See [docs/configuration.md](./docs/configuration.md).

## Documentation

- [docs/configuration.md](./docs/configuration.md) — config options.
- [docs/controls.md](./docs/controls.md) — the action matrix + auth gating.
- [docs/waybar.md](./docs/waybar.md) — Waybar module + styling.
- [docs/niri.md](./docs/niri.md) — niri / Wayland integration.
- [docs/security.md](./docs/security.md) — trust boundary + command safety.
- [AGENTS.md](./AGENTS.md) — contributor / agent operating manual.

## License

[Apache-2.0](./LICENSE).
