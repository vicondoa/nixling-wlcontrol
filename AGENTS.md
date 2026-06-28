# AGENTS.md

Operating manual for AI coding agents (Copilot CLI, GitHub Copilot,
Cursor, …) and human contributors working on
**`vicondoa/d2b-wlcontrol`**. If you are a *user* installing this
tool on your desktop, start at [README.md](./README.md) instead — this
file is for people changing the project.

This manual is adapted from the
[`vicondoa/d2b`](https://github.com/vicondoa/d2b) `AGENTS.md`,
scoped down to a desktop control app. The methodology (fleet waves,
panel review, "existing code is canon", commit/versioning conventions)
is intentionally the same shape; the architecture and roster are
tailored to this repo.

## What this is

`d2b-wlcontrol` is a clean, Waybar-styled indicator and control
center for [d2b](https://github.com/vicondoa/d2b) microVMs,
built for a niri / Wayland desktop. It shows which VMs are running and
their status, and exposes the controls a d2b operator can already
drive: lifecycle (start / stop / restart / switch), USB attach/detach,
launching a terminal into a VM, store verification, and d2b daemon-native
audio controls (mic/speaker toggles plus speaker volume / mic gain).

It is **not** a d2b replacement, a VM manager, or a privileged
tool. It is a thin, memory-safe (Rust) presentation + control surface
over surfaces d2b already exposes.

### Trust boundary (read this first)

`d2b-wlcontrol` talks **only** to the operator-facing public
surface:

- the d2bd public socket `/run/d2b/public.sock` (non-abstract
  `SOCK_SEQPACKET`, 4-byte little-endian length-prefixed JSON frames,
  `SO_PEERCRED` auth); and
- where a CLI boundary is genuinely better UX (detached guest terminal exec or
  non-shell build), the official `d2b` CLI; and
- for observability, the configured browser opener.

It MUST NEVER:

- talk to the privileged broker socket `/run/d2b/priv.sock`;
- use `sudo` or otherwise escalate privilege;
- read or write d2b's root-owned state files directly (for
  example `/var/lib/d2b/vms/<vm>/state/audio-state.json`);
- construct commands as shell strings (always argv vectors);
- assume capabilities from filesystem permissions instead of
  `d2b auth status`.

These are hard rules. See "Don'ts" below.

## Repo layout

```
.
├── README.md                 <- user-facing entry point
├── AGENTS.md                 <- this file
├── CHANGELOG.md              <- Keep a Changelog, entries under `## Unreleased`
├── LICENSE                   <- Apache-2.0
├── flake.nix / flake.lock    <- package, app, devShell
├── Cargo.toml                <- Rust workspace
├── rust-toolchain.toml       <- pinned toolchain (1.94.1)
├── crates/
│   ├── wlcontrol-core/       <- FROZEN domain contract: model, config,
│   │                            reducer, action planner (see src/model.rs)
│   ├── wlcontrol-d2b/    <- direct d2bd public-socket client + framing
│   ├── wlcontrol-waybar/     <- Waybar custom-module JSON renderer
│   ├── wlcontrol-ui/         <- Quickshell/QML layer-shell popup launcher
│   └── wlcontrol-cli/        <- `d2b-wlcontrol` binary (integration seam)
├── data/
│   ├── waybar-module.jsonc   <- starter Waybar custom-module config
│   └── style.css             <- starter, class-based CSS
├── docs/                     <- configuration / controls / waybar / niri / security
└── tests/                    <- cross-crate fixtures + integration tests
```

New behaviour belongs in the crate that owns its concern. Cross-crate
types belong in `wlcontrol-core` (see "The core contract").

## The core contract

`crates/wlcontrol-core/src/model.rs` is the **frozen cross-crate
contract**. Every other crate builds against it:

- `wlcontrol-d2b` produces `WlState` / source fragments from the
  socket.
- `wlcontrol-waybar` and `wlcontrol-ui` render `WlState`.
- `wlcontrol-cli` dispatches `PlannedAction`.

You MAY extend these types (add fields with `#[serde(default)]`, add
enum variants) but MUST NOT break published field/variant names other
crates already consume. Breaking changes go through an **integrator
prep commit** landed on `main` before dependent fleet agents branch
(the same integrator-prep-first pattern d2b uses for shared-DTO
waves).

## Build & validate

`cargo` is not always on `PATH`. Use the pinned toolchain:

```bash
export PATH="$(echo ~/.rustup/toolchains/1.94.1-*/bin):/home/paydro/.nix-profile/bin:$PATH"
export CARGO_BUILD_RUSTC_WRAPPER=''
export CARGO_TARGET_DIR=/home/paydro/.cache/d2b-wlcontrol-target
```

The PR gate is four commands:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
nix flake check --no-build --all-systems      # NIX_CONFIG='experimental-features = nix-command flakes'
```

Zero clippy warnings is a hard requirement (`-D warnings`). The visible
control popup is Quickshell/QML; the flake devShell provides
`quickshell` and the Material Symbols font (`nix develop`).

## Development workflow

### Fleet waves

Implementation runs as an **autopilot fleet**: the integrator owns
shared contracts, merge order, validation evidence, and panel gates;
fleet agents own disjoint crates/files. One git worktree/branch per
agent:

```bash
git worktree add -b wave-<n>-<scope> ../d2b-wlcontrol-<scope> main
```

Agents commit on their own branch; the integrator merges, validates,
and runs the panel. When scopes are file-disjoint, an octopus merge is
fine; otherwise merge sequentially and resolve conflicts preserving
both sides.

### Edit → commit → validate

Commit before running `nix flake check` — untracked files are invisible
to the flake's `git+file` fetcher (same caveat as d2b). For
`git+file` evals, the working tree only needs to be *committed*, not
clean.

### "Existing code is canon"

When the plan, README, or any doc disagrees with committed,
passing-test code, the code wins. Record the drift in the commit body
(`Spec correction: …`) or the session `plan.md`, don't silently
re-align code to prose. This applies to AGENTS.md too: if you change a
load-bearing behaviour described here, update this file in the same
commit.

## Panel review

Multi-wave work passes a **panel sign-off gate** at each wave boundary.
The integrator MUST NOT begin the next wave until every reviewer
returns `signoff: true`.

Per wave:

1. **Plan review** — panel reviews the wave plan; iterate to unanimous
   sign-off before dispatching implementation agents.
2. **Implementation** — dispatch fleet agents per the dependency graph.
3. **Integration** — integrator merges and validates.
4. **Work review** — panel reviews the integrated diff + the
   integrator's validation evidence; iterate via fix-agents to
   unanimous sign-off.
5. **Advance** — only now start the next wave.

Panel prompts MUST include the validation commands and pass/fail
results the integrator already ran, and MUST instruct reviewers **not**
to rerun long validations themselves — they inspect the plan/diff and
the supplied evidence, and flag missing validation as a finding. Green
tests do not waive the gate; a wave closes only on unanimous sign-off.

Panel reviewers use the operator-selected review model/tooling for the
wave. Each returns JSON:

```json
{
  "reviewer": "rust",
  "signoff": true,
  "summary": "What was reviewed and the overall posture.",
  "recommendations": []
}
```

`signoff` is `true` iff `recommendations` is `[]`. Any recommendation
becomes a tracked follow-up assigned to the most relevant scope.

### Roster

This repo does **not** reuse d2b's framework-heavy roster. Use the
desktop-control-app roster:

| Reviewer | Focus |
| --- | --- |
| `rust` | Workspace architecture, async/concurrency, ownership/lifetimes, memory safety, dependency direction, error typing. |
| `protocol` | d2bd public-socket handshake, length-prefixed JSON framing, version negotiation, response typing, auth-role mapping, CLI-fallback boundaries. |
| `wayland` | niri/Wayland behavior, layer-shell placement/toggle behavior, no XWayland assumptions. |
| `waybar` | Custom-module JSON contract, update loop, click actions, CSS classes, tooltip quality, restart/backoff. |
| `shell-ux` | Quickshell/QML layer-shell structure, keyboard/mouse ergonomics, responsiveness, action discoverability, clean visual hierarchy. |
| `security` | Public-socket-only boundary, no broker/sudo/state-file mutation, command-injection resistance, argv/log redaction, safe confirmations. |
| `test` | Fake-d2bd coverage, fixtures, reducer/golden tests, UI view-model tests, failure/timeout cases, CI sufficiency. |
| `nix-packaging` | Flake packaging, Quickshell/runtime font closure, Waybar/niri install snippets, dev shell, optional HM/NixOS module shape. |
| `product` | Control matrix, defaults, advanced-mode boundaries, not-dumbed-down UX, actionable error/remediation copy. |
| `docs` | README/config docs, Waybar and niri setup, security model, control-surface reference, troubleshooting. |

Escape hatches are narrow: trivial one-line fixes, time-critical
hotfixes (post-fix panel required), and documentation-only changes may
skip the gate unless the doc describes load-bearing behavior.

## Coding conventions

- **Rust.** Edition 2021, toolchain 1.94.1. `cargo fmt` is canonical.
  Zero clippy warnings. Prefer typed errors (`WlError`) over `unwrap`
  on fallible paths. Remove dead code as you touch an area.
- **Commands are argv, never shell.** Use `std::process::Command` with
  explicit args. No shell interpolation, ever.
- **Auth-aware controls.** Gate every mutating control on
  `d2b auth status` role, not on guesswork.
- **Waybar output.** One newline-terminated JSON object per update;
  no `interval` on the self-looping module; stable CSS classes only.
- **Quickshell popup.** Fixed-size layer-shell surface; no XWayland
  assumptions; `open` toggles show/hide from Waybar.
- **Audio.** Use only d2b's public `audio` socket surface for status and
  mutations. Keep controls disabled with a clear reason when d2b does not
  report audio for a VM; do not mutate audio state files directly.

## Versioning & changelog

[Semantic Versioning](https://semver.org/) +
[Keep a Changelog](https://keepachangelog.com/). Entries accumulate
under `## [Unreleased]` until a version is cut. Pre-release
`[Unreleased]` MAY carry wave/finding process markers; released
sections MUST be summarized for users with all internal process
markers (wave/finding/round tags) stripped.

## Commit conventions

- **Subject.** Short, imperative, area-prefixed:
  `core: freeze the WlState contract`,
  `waybar: add attention class`, `d2b: implement hello handshake`.
- **Body.** Wrap ~72 cols, explain *why*.
- **Traceability.** In-development commits on fleet branches MAY carry
  a trailing wave/finding tag, mirroring d2b's scheme:
  `( W1 )`, `( W2fu1 H3 )` (wave 2, follow-up round 1, HIGH-3). The
  severity letter comes from the reviewer JSON (`C`/`H`/`M`/`L`). These
  markers are for planning only — keep them out of shipped code/docs
  and released CHANGELOG sections.
- One logical change per commit.

Always include the `Co-authored-by: Copilot` trailer unless asked not
to.

## Don'ts (security-relevant)

- **Don't talk to the broker socket** (`/run/d2b/priv.sock`).
- **Don't use `sudo`** or any privilege escalation.
- **Don't read/write d2b's root-owned state files** directly;
  drive everything through the public socket or the `d2b` CLI.
- **Don't build commands as shell strings.** argv vectors only.
- **Don't infer authorization from filesystem permissions** — use
  `d2b auth status`.
- **Don't assume XWayland.** Target Wayland/niri natively.
- **Don't bypass d2b audio controls.** Use the public `audio` socket surface;
  never edit guest or host audio state directly.
- **Don't add a new linter/formatter/pre-commit hook** beyond
  `cargo fmt` / `cargo clippy` / `nix flake check` without being asked.
- **Don't leak internal process markers** (wave/finding tags) into
  shipped code, docs, or released CHANGELOG sections.

## References

- [README.md](./README.md) — user-facing intro + install.
- [docs/configuration.md](./docs/configuration.md) — config surface.
- [docs/controls.md](./docs/controls.md) — action matrix + auth gating.
- [docs/waybar.md](./docs/waybar.md) — Waybar module JSON + CSS.
- [docs/niri.md](./docs/niri.md) — niri / layer-shell Wayland notes.
- [docs/security.md](./docs/security.md) — trust boundary + command safety.
- [d2b `docs/reference/daemon-api.md`](https://github.com/vicondoa/d2b/blob/main/docs/reference/daemon-api.md)
  — the public-socket wire contract this client speaks.
- [d2b `docs/reference/cli-contract.md`](https://github.com/vicondoa/d2b/blob/main/docs/reference/cli-contract.md)
  — the CLI surfaces this tool mirrors.
