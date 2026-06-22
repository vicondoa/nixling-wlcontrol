# Waybar integration

`nixling-wlcontrol waybar` is a **continuous** Waybar custom module: it
loops, emits one newline-terminated JSON object per refresh, and flushes
stdout each time. Because it self-loops, it must **not** be given an
`interval`.

## Module config

Generate a starter snippet:

```bash
nixling-wlcontrol print-waybar-config
```

```jsonc
"custom/nixling-wlcontrol": {
  "exec": "nixling-wlcontrol waybar",
  "return-type": "json",
  "restart-interval": 5,
  "signal": 8,
  "on-click": "nixling-wlcontrol open",
  "on-click-right": "nixling-wlcontrol action cycle-display",
  "on-click-middle": "nixling-wlcontrol action refresh",
  "tooltip": true
}
```

The `"signal": 8` key pairs with the module's `SIGRTMIN+8` handler so
`nixling-wlcontrol action cycle-display` (and any other instance) can
refresh the bar on demand.

Add `"custom/nixling-wlcontrol"` to one of your `modules-left` /
`modules-center` / `modules-right` arrays. The `exec` / `on-click`
commands assume `nixling-wlcontrol` is installed and on your `PATH`
(see [Install](../README.md#install)).

## Output contract

Each line is a single JSON object:

| Field | Meaning |
| --- | --- |
| `text` | Compact indicator, e.g. `◆ 2/4` (running / visible), with a trailing `!` when attention is needed. |
| `class` | Array of CSS classes (see below). |
| `tooltip` | Per-VM summary (state glyph, name, env, pending-restart). |

## CSS classes

Generate a starter stylesheet:

```bash
nixling-wlcontrol print-css
```

The renderer emits these classes on `#custom-nixling-wlcontrol`:

| Class | When |
| --- | --- |
| `all-stopped` | No visible VM is running. |
| `partial-running` | Some but not all visible VMs are running. |
| `all-running` | Every visible VM is running. |
| `attention` | A VM needs attention (pending restart or unknown state). |
| `daemon-down` | nixlingd is unreachable. |
| `auth-denied` | Reachable but no authorized role. |
| `stale` | State served from cache after a failed refresh. |

The starter CSS imports nixling's generated color variables from
`/etc/nixling/ui-colors.css` and uses the state variables with fallbacks:

| Variable | Used for |
| --- | --- |
| `--nixling-state-running` | `all-running` |
| `--nixling-state-transitioning` | `partial-running` |
| `--nixling-state-pendingRestart` | `attention` |
| `--nixling-state-error` | `daemon-down` |
| `--nixling-state-denied` | `auth-denied` |
| `--nixling-state-unknown` | `all-stopped` |

The CSS artifact is normally generated at `/etc/nixling/ui-colors.css`.
If the file is absent or malformed, Waybar still loads the starter style
and uses the fallback colors embedded in each `var(...)` expression. The
rules only set state accent colors so the module inherits your bar's
font, padding, and base styling.

## Clicks

| Click | Action |
| --- | --- |
| Left | Open / focus the control center. |
| Right | Cycle compact / detail display mode. |
| Middle | Refresh now. |

> The loop supports signal-driven refresh (`SIGRTMIN+8`, paired with the
> module's `"signal": 8`), non-overlapping refresh, daemon-down backoff,
> and compact/detail display modes (toggle with
> `nixling-wlcontrol action cycle-display`).
