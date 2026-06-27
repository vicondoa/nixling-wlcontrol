# Waybar integration

`d2b-wlcontrol waybar` is a **continuous** Waybar custom module: it
loops, emits one newline-terminated JSON object per refresh, and flushes
stdout each time. Because it self-loops, it must **not** be given an
`interval`.

## Module config

Generate a starter snippet:

```bash
d2b-wlcontrol print-waybar-config
```

```jsonc
"custom/d2b-wlcontrol": {
  "exec": "d2b-wlcontrol waybar",
  "return-type": "json",
  "restart-interval": 5,
  "signal": 8,
  "on-click": "d2b-wlcontrol open",
  "on-click-right": "d2b-wlcontrol action cycle-display",
  "on-click-middle": "d2b-wlcontrol action refresh",
  "tooltip": true
}
```

The `"signal": 8` key pairs with the module's `SIGRTMIN+8` handler so
`d2b-wlcontrol action cycle-display` (and any other instance) can
refresh the bar on demand.

Add `"custom/d2b-wlcontrol"` to one of your `modules-left` /
`modules-center` / `modules-right` arrays. The `exec` / `on-click`
commands assume `d2b-wlcontrol` is installed and on your `PATH`
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
d2b-wlcontrol print-css
```

The renderer emits these classes on `#custom-d2b-wlcontrol`:

| Class | When |
| --- | --- |
| `all-stopped` | No visible VM is running. |
| `partial-running` | Some but not all visible VMs are running. |
| `all-running` | Every visible VM is running. |
| `attention` | A VM needs attention (pending restart or unknown state). |
| `daemon-down` | d2bd is unreachable. |
| `auth-denied` | Reachable but no authorized role. |
| `stale` | State served from cache after a failed refresh. |

The starter CSS imports d2b's generated GTK color definitions from
`/etc/d2b/ui-colors.css` and uses the state color names:

| GTK color | Used for |
| --- | --- |
| `@d2b_state_running` | `all-running` |
| `@d2b_state_transitioning` | `partial-running` |
| `@d2b_state_pendingRestart` | `attention` |
| `@d2b_state_error` | `daemon-down` |
| `@d2b_state_denied` | `auth-denied` |
| `@d2b_state_unknown` | `all-stopped` |

The CSS artifact is normally generated at `/etc/d2b/ui-colors.css`.
It defines those names with GTK `@define-color`, for example
`@define-color d2b_state_running #a6e3a1;`. The rules only set state
accent colors so the module inherits your bar's font, padding, and base
styling.

## Clicks

| Click | Action |
| --- | --- |
| Left | Open / focus the control center. |
| Right | Cycle compact / detail display mode. |
| Middle | Refresh now. |

> The loop supports signal-driven refresh (`SIGRTMIN+8`, paired with the
> module's `"signal": 8`), non-overlapping refresh, daemon-down backoff,
> and compact/detail display modes (toggle with
> `d2b-wlcontrol action cycle-display`).
