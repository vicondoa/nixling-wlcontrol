# Controls and action matrix

Every mutating control is gated on two things:

1. **Connectivity** — `nixlingd` must be reachable on the public socket.
2. **Authorization** — your effective role from `nixling auth status`
   (`none`, `launcher`, or `admin`). The control surface never guesses
   authorization from filesystem permissions.

When an action is unavailable, the UI says *why* (daemon down,
insufficient role, VM not in a runnable state, USB owned elsewhere, or
unsupported by the current nixling control plane) rather than failing
silently.

## Action matrix

| Action | Default | Min role | Backing surface | Notes |
| --- | --- | --- | --- | --- |
| Show declared VMs | on | none | `nixling list` | VM set, env, features, order. |
| Show per-VM runtime | on | none | `nixling status <vm>` | Runtime/readiness/pending-restart truth. |
| USB probe | on | none | `nixling usb probe` | Read-only claim/ownership view. |
| Start / Stop / Restart | on | admin | `vm start|stop|restart --apply` | Start and Restart use nixlingd `noWaitApi=true` so the UI returns once the VM process is supervised; the normal status refresh shows readiness convergence. Stop is the normal graceful guest-shutdown path when nixling supports it. |
| Force shutdown | ellipsis-expanded only | admin | pending nixling force-stop socket/CLI contract | Emergency override only; never a primary visible button. Requires destructive styling and a two-click confirmation because it skips graceful guest shutdown. |
| Store verify | advanced icon | admin | `store verify` | Check live-pool/store integrity. |
| Build | advanced icon | launcher | `nixling build <vm>` | Build/evaluate the VM toplevel without activating it. |
| Boot | advanced icon | admin | `boot --apply` | Stage the built/current closure for the next VM boot without switching the running VM. |
| Switch (activate closure) | advanced icon | admin | `switch --apply` | Activate the VM generation now; confirm if VM is running. |
| USB attach | on | admin | `usb attach --apply` | Only when unbound/ownerless for this VM. |
| USB detach | on | admin | `usb detach --apply` | Only for the owning VM. |
| Launch terminal | on | admin | `nixling vm exec -d <vm> -- <guest argv...>` | Admin-only detached guest exec; argv-only. |
| Custom quick launch | icon row | admin | `nixling vm exec -d <vm> -- <configured argv...>` | Per-VM config-driven icons such as `run-openterface`. |
| Observability portal | header | none | browser argv + Signoz URL | Opens the URL only; auto-login is not performed. |
| Audio mic / speaker / off | hidden | — | `nixling audio …` | Not rendered until nixling's audio plane is live. |
| Host install/destroy/migrate/keys | hidden | — | nixling CLI | Out of scope for a control surface. |

## Role gating

- `none` → read-only. The bar shows `auth-denied`; controls explain the
  missing authorization.
- `launcher` → build/evaluate.
- `admin` → lifecycle, USB, store verify, boot/switch, and terminal/guest exec.

## Audio is intentionally hidden

nixling's `audio mic|speaker|off|status` verbs currently return a typed
`not-yet-implemented` envelope, and nixling explicitly has no
daemon-native audio control plane yet. `nixling-wlcontrol` does not render these
controls and never edits `audio-state.json` directly. When nixling ships a
working audio surface, these controls can light up with no privileged-state
shortcuts.

The control center renders this matrix with auth-aware gating: blocked
actions are disabled with a tooltip explaining why, VM quick actions are
icon-only circular controls with hover text, and stop/restart/switch on a
running VM prompt for confirmation. The primary Stop button is the graceful
guest-shutdown path and its progress copy says so. Force shutdown is kept
behind the ellipsis-expanded controls, uses destructive styling, requires a
strong second click, and is disabled until the nixling force-stop contract is
available. Action progress and results are shown as human-readable messages
rather than raw command lines.
