# Security model

`d2b-wlcontrol` is a presentation + control surface. It holds no
privilege of its own and is designed so a bug cannot escalate into a
host compromise.

## Trust boundary

The tool talks **only** to operator-facing surfaces:

- the d2bd **public** socket `/run/d2b/public.sock`
  (non-abstract `SOCK_SEQPACKET`, 4-byte little-endian length-prefixed
  JSON frames, `SO_PEERCRED` authorization); and
- the official `d2b` CLI, used only where it is the better boundary
  (detached guest terminal exec and non-shell build); and
- the configured browser opener for the observability URL.

Authorization is whatever d2bd grants the calling user via
`SO_PEERCRED` + group membership. `d2b-wlcontrol` adds **no**
privilege and enforces no policy of its own beyond hiding controls the
daemon would reject anyway.

## Hard rules

- **No broker socket.** Never connects to `/run/d2b/priv.sock`.
- **No privilege escalation.** Never uses `sudo` or setuid paths.
- **No direct state mutation.** Never reads or writes d2b's
  root-owned state files (e.g. `audio-state.json`); all state changes go
  through the public socket or the `d2b` CLI.
- **argv-only execution.** Every spawned process is an argv vector. No
  shell, no string interpolation, so VM names / bus ids / shell paths
  can never become shell metacharacters.
- **Auth from the daemon, not the filesystem.** Control availability is
  derived from `d2b auth status`, never from inspecting file
  permissions.
- **No XWayland assumptions.**
- **No observability credential handling.** The Signoz button opens a URL only;
  auto-login/token/cookie handling is out of scope.

## Failure posture

- `d2bd` unreachable → `daemon-down` state; mutating controls
  disabled, not errored mid-flight.
- Reachable but unauthorized → `auth-denied` state; read-only.
- A failed refresh reuses the last state marked `stale` rather than
  flapping to a false-healthy or empty view.
- d2b typed errors and remediation text are surfaced to the
  operator; raw command output and any secrets are never logged.

## Reporting

Security concerns about `d2b-wlcontrol` should be reported privately
to the repository owner. Issues in d2b itself belong in the
[d2b](https://github.com/vicondoa/d2b) project.
