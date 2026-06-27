# Configuration

`d2b-wlcontrol` reads TOML from
`${XDG_CONFIG_HOME:-~/.config}/d2b-wlcontrol/config.toml`. The file
is optional — every setting has a sane default. A present-but-malformed
file is a hard error (so you notice typos) rather than a silent
fallback to defaults.

## Example

```toml
# Path to the d2bd public socket.
public_socket = "/run/d2b/public.sock"

# Waybar refresh cadence (ms) and per-operation timeout (ms).
refresh_interval_ms = 2500
command_timeout_ms = 10000

# Hide framework net VMs (sys-*-net) from the compact surfaces.
hide_net_vms = true

# Show the pending-restart marker.
show_pending_restart = true

[terminal]
# Guest terminal command as an ARGV VECTOR (never a shell string). wlcontrol
# runs `d2b vm exec -d <vm> -- ${guest_argv...}`.
guest_argv = ["/run/current-system/sw/bin/foot"]

[observability]
# Signoz URL to open. Auto-login is intentionally deferred; the browser's
# existing session is used if one exists.
enabled = true
url = "http://sys-obs:8080"
browser_argv = ["xdg-open"]

[[quick_launch]]
id = "run-openterface"
vm = "work-ssd"
icon = "desktop_windows"
tooltip = "Run Openterface"
guest_argv = ["/run/current-system/sw/bin/openterface-run"]
```

## Options

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `public_socket` | string | `/run/d2b/public.sock` | d2bd public socket path. |
| `refresh_interval_ms` | integer | `2500` | Waybar poll cadence. |
| `command_timeout_ms` | integer | `10000` | Per-operation deadline. |
| `hide_net_vms` | bool | `true` | Hide `sys-*-net` VMs from compact views. |
| `show_pending_restart` | bool | `true` | Surface the pending-restart marker. |
| `favorites` | array of string | `[]` | VM names pinned first, in the given order. |
| `hidden_vms` | array of string | `[]` | VM names hidden from compact surfaces. |
| `terminal.guest_argv` | array of string | `["/run/current-system/sw/bin/foot"]` | Guest terminal argv launched detached inside the VM. |
| `terminal.guest_shell` | string | `bash` | Legacy fallback used only if `terminal.guest_argv = []`. |
| `observability.enabled` | bool | `true` | Whether to show/use the observability portal action. |
| `observability.url` | string | `http://sys-obs:8080` | Signoz portal URL opened by the header button. |
| `observability.browser_argv` | array of string | `["xdg-open"]` | Browser/open command prefix for `observability.url`. |
| `quick_launch[]` | table array | `[]` | Per-VM custom quick-launch icon. Fields: `id`, `vm`, `icon`, `tooltip`, `guest_argv`. |

## Terminal command is argv, not a shell string

The terminal command is always an **argv vector**. `d2b-wlcontrol`
spawns the official `d2b` CLI directly (via `execvp`-style process
spawning) as `d2b vm exec -d <vm> -- ${terminal.guest_argv...}`. There is no
shell, so VM names and guest argv elements can never be interpreted as shell
metacharacters. Use absolute guest paths where possible because guest-control
exec does not resolve a host shell for you.

Common guest terminal commands:

```toml
guest_argv = ["/run/current-system/sw/bin/foot"]
guest_argv = ["/run/current-system/sw/bin/ghostty"]
```

An empty terminal command is rejected at config load, and a `public_socket`
pointing at the privileged broker socket (`priv.sock`) is refused — the control
surface speaks only the public socket.

## Observability opens a URL only

The observability button opens `observability.url` with
`observability.browser_argv`. Set `observability.enabled = false` to disable the
button/action. It does not read Signoz credentials, generate cookies, or perform
auto-login; if your browser is already logged in, that session is reused by the
browser.

## Per-VM quick-launch icons

`[[quick_launch]]` entries add custom icon buttons to the always-visible icon
row before USB controls. Each entry is VM-scoped and launches a detached guest
command with `d2b vm exec -d <vm> -- ${guest_argv...}`. `icon` is a Material
Symbols name and `tooltip` is the hover text.
