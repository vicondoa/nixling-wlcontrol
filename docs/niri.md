# niri / Wayland integration

`d2b-wlcontrol` targets niri (and Wayland compositors generally)
natively. It makes **no XWayland assumptions** and uses:

- a Waybar custom module for the bar indicator; and
- a Quickshell layer-shell popup for the control surface.

## Popup behavior

`d2b-wlcontrol open` toggles a draggable top-right Quickshell popup:

- first invocation shows it;
- the next invocation hides it;
- the popup is a layer-shell surface, not a normal tiled window;
- drag the header/background to reposition it after opening;
- the popup fits its VM cards until it reaches about half the screen height,
  then uses a thin scrollbar for overflow; and
- no niri `window-rule` is required.

This matches Waybar click ergonomics: bind left-click to
`d2b-wlcontrol open`, click once to show controls, click again to
hide them.

## Theme

The popup and Waybar styling share d2b's generated color artifacts:

- `/etc/d2b/ui-colors.json` carries version `1`, host and state
  accents, per-env accents, and per-VM active / inactive / urgent border
  colors.
- `/etc/d2b/ui-colors.css` exposes the same palette as GTK
  `@define-color` names such as `@d2b_state_running` for Waybar.

The JSON shape is:
`{ version: 1, host: { accent }, states: { running, transitioning,
pendingRestart, error, denied, unknown }, envs: { <env>: { accent } },
vms: { <vm>: { env, border: { active, inactive, urgent } } } }`.

`d2b-wlcontrol open` accepts parsed theme data from the configured
color artifact through the status JSON or these environment variables:
`D2B_WLCONTROL_THEME_JSON` for the full artifact,
`D2B_WLCONTROL_STATE_COLORS` for the `states` object, and
`D2B_WLCONTROL_ENV_COLORS` for env accents. The popup uses state
colors for VM dots and action feedback, and env accents for card borders
and stripes. VM border colors in the artifact remain reserved for the
compositor/window-border surface. Missing, invalid, or malformed color
data is ignored and the popup falls back to visible Catppuccin-like
defaults instead of crashing.
