# Controls and action matrix

Every mutating control is gated on two things:

1. **Connectivity** — `d2bd` must be reachable on the public socket.
2. **Authorization** — your effective role from `d2b auth status`
   (`none`, `launcher`, or `admin`). The control surface never guesses
   authorization from filesystem permissions.

When an action is unavailable, the UI says *why* (daemon down,
insufficient role, VM not in a runnable state, USB owned elsewhere, or
unsupported by the current d2b control plane) rather than failing
silently.

## Action matrix

| Action | Default | Min role | Backing surface | Notes |
| --- | --- | --- | --- | --- |
| Show declared VMs | on | none | d2bd public socket `list` | VM set, env, features, order. |
| Show per-VM runtime | on | none | d2bd public socket `status` | Runtime/readiness/pending-restart truth. |
| USB probe | on | none | d2bd status read model / `usbipProbe` | Read-only claim/ownership view. |
| Start / Stop / Restart | on | admin | `vm start|stop|restart --apply` | Start and Restart use d2bd `noWaitApi=true` so the UI returns once the VM process is supervised; the normal status refresh shows readiness convergence. Stop is the normal graceful guest-shutdown path when d2b supports it. |
| Force shutdown | ellipsis-expanded only | admin | pending d2b force-stop socket/CLI contract | Emergency override only; never a primary visible button. Requires destructive styling and a two-click confirmation because it skips graceful guest shutdown. |
| Store verify | advanced icon | admin | `store verify` | Check live-pool/store integrity. |
| Build | advanced icon | launcher | `d2b build <vm>` | Build/evaluate the VM toplevel without activating it. |
| Boot | advanced icon | admin | `boot --apply` | Stage the built/current closure for the next VM boot without switching the running VM. |
| Switch (activate closure) | advanced icon | admin | `switch --apply` | Activate the VM generation now; confirm if VM is running. |
| USB attach | on | admin | `usb attach --apply` | Only when unbound/ownerless for this VM. |
| USB detach | on | admin | `usb detach --apply` | Only for the owning VM. |
| Launch terminal | on | admin | `d2b vm exec -d <vm> -- <guest argv...>` | Admin-only detached guest exec; argv-only. |
| Custom quick launch | icon row | admin | `d2b vm exec -d <vm> -- <configured argv...>` | Per-VM config-driven icons such as `run-openterface`. |
| Observability portal | header | none | browser argv + Signoz URL | Opens the URL only; auto-login is not performed. |
| Audio mic / speaker / off | ellipsis-expanded when reported | admin | d2bd public socket `audio` | Toggles route through d2b's daemon-native audio plane; qemu host-only/provider-degraded states are explained inline. |
| Speaker volume / mic gain | ellipsis-expanded when reported | admin | d2bd public socket `audio` | Drag sliders preview locally and send one committed mutation on release; arrow keys adjust 5%, PageUp/PageDown adjust 10%. |
| Host install/destroy/migrate/keys | hidden | — | d2b CLI | Out of scope for a control surface. |

## Role gating

- `none` → read-only. The bar shows `auth-denied`; controls explain the
  missing authorization.
- `launcher` → build/evaluate.
- `admin` → lifecycle, USB, store verify, boot/switch, and terminal/guest exec.

## Audio controls

`d2b-wlcontrol` consumes `AudioStatus` from the public socket and renders audio
controls only for VMs that d2b reports as audio-capable. The control center
shows microphone and speaker toggles plus speaker-volume and mic-gain sliders.
Muting a channel disables the slider without resetting the stored level, so
unmuting restores the previous value reported by d2b.

All audio mutations go back through the d2bd public `audio` operation. The
control center never talks to the broker, never uses `sudo`, and never reads or
writes `audio-state.json` directly. Provider-specific states are rendered as
operator-facing badges: qemu host-only enforcement remains controllable when
d2b supports the host side, while unsupported, provider-misconfigured, or
degraded entries stay disabled with d2b's remediation text.

The control center renders this matrix with auth-aware gating: blocked
actions are disabled with a tooltip explaining why, VM quick actions are
icon-only circular controls with hover text, and stop/restart/switch on a
running VM prompt for confirmation. The primary Stop button is the graceful
guest-shutdown path and its progress copy says so. Force shutdown is kept
behind the ellipsis-expanded controls, uses destructive styling, requires a
strong second click, and is disabled until the d2b force-stop contract is
available. Action progress and results are shown as human-readable messages
rather than raw command lines.
