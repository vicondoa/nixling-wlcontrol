# niri / Wayland integration

`nixling-wlcontrol` targets niri (and Wayland compositors generally)
natively. It makes **no XWayland assumptions** and uses:

- a Waybar custom module for the bar indicator; and
- a Quickshell layer-shell popup for the control surface.

## Popup behavior

`nixling-wlcontrol open` toggles a draggable top-right Quickshell popup:

- first invocation shows it;
- the next invocation hides it;
- the popup is a layer-shell surface, not a normal tiled window;
- drag the header/background to reposition it after opening;
- the popup fits its VM cards until it reaches about half the screen height,
  then uses a thin scrollbar for overflow; and
- no niri `window-rule` is required.

This matches Waybar click ergonomics: bind left-click to
`nixling-wlcontrol open`, click once to show controls, click again to
hide them.

## Theme

The popup and Waybar styling share nixling's generated color artifacts:

- `/etc/nixling/ui-colors.json` carries version `1`, host and state
  accents, per-env accents, and per-VM active / inactive / urgent border
  colors.
- `/etc/nixling/ui-colors.css` exposes the same palette as GTK
  `@define-color` names such as `@nixling_state_running` for Waybar.

The JSON shape is:
`{ version: 1, host: { accent }, states: { running, transitioning,
pendingRestart, error, denied, unknown }, envs: { <env>: { accent } },
vms: { <vm>: { env, border: { active, inactive, urgent } } } }`.

`nixling-wlcontrol open` accepts parsed theme data from the configured
color artifact through the status JSON or these environment variables:
`NIXLING_WLCONTROL_THEME_JSON` for the full artifact,
`NIXLING_WLCONTROL_STATE_COLORS` for the `states` object,
`NIXLING_WLCONTROL_ENV_COLORS` for env accents, and
`NIXLING_WLCONTROL_VM_COLORS` for VM border colors. The popup uses state
colors for VM dots and action feedback, env accents for card stripes, and
VM border colors when provided. Missing, invalid, or malformed color data
is ignored and the popup falls back to visible Catppuccin-like defaults
instead of crashing.
