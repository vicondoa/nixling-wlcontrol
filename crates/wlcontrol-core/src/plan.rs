//! Action availability gating and argv/intent planning.
//!
//! Owning wave: **Wave 1 — Core model agent**. Wave 0 ships a working baseline
//! covering role gating, daemon-down gating, and argv-only terminal planning.
//! The Wave 1 agent extends per-action VM-state rules, USB ownership rules, and
//! the full advanced-controls matrix from the plan.

use crate::config::Config;
use crate::model::{
    ActionAvailability, ActionKind, AuthRole, Connectivity, PlannedAction, RuntimeState,
    SocketIntent, Unavailable, Vm, WlState,
};

/// Returns `Some(reason)` when `action` cannot currently be invoked.
pub fn block_reason(action: &ActionKind, state: &WlState) -> Option<Unavailable> {
    if matches!(
        action,
        ActionKind::AudioMic { .. } | ActionKind::AudioSpeaker { .. } | ActionKind::AudioOff { .. }
    ) {
        return Some(Unavailable::NotYetImplemented);
    }

    // Display-only actions are always available.
    match action {
        ActionKind::OpenControlCenter | ActionKind::CycleDisplay | ActionKind::Refresh => {
            return None;
        }
        _ => {}
    }

    if state.connectivity == Connectivity::DaemonDown {
        return Some(Unavailable::DaemonDown);
    }

    let required = required_role(action);
    if !role_satisfies(state.role, required) {
        return Some(Unavailable::InsufficientRole { required });
    }

    match action {
        ActionKind::Start { vm } => running_vm(state, vm)
            .filter(|v| v.state == RuntimeState::Running)
            .map(|_| Unavailable::VmState {
                detail: "VM is already running".into(),
            }),
        ActionKind::Stop { vm } | ActionKind::Restart { vm } | ActionKind::Switch { vm } => {
            running_vm(state, vm)
                .filter(|v| v.state == RuntimeState::Stopped)
                .map(|_| Unavailable::VmState {
                    detail: "VM is not running".into(),
                })
        }
        ActionKind::LaunchTerminal { vm } => running_vm(state, vm)
            .filter(|v| v.state != RuntimeState::Running)
            .map(|_| Unavailable::VmState {
                detail: "start the VM before opening a terminal".into(),
            }),
        ActionKind::UsbAttach { vm, bus_id } => usb_attach_block(state, vm, bus_id),
        ActionKind::UsbDetach { vm, bus_id } => usb_detach_block(state, vm, bus_id),
        ActionKind::StoreVerify { .. } => None,
        _ => None,
    }
}

/// Return the full per-VM action list the control center can render.
pub fn vm_actions(state: &WlState, config: &Config, vm: &str) -> Vec<ActionAvailability> {
    let mut actions = vec![
        ActionKind::Start { vm: vm.into() },
        ActionKind::Stop { vm: vm.into() },
        ActionKind::Restart { vm: vm.into() },
        ActionKind::Switch { vm: vm.into() },
        ActionKind::LaunchTerminal { vm: vm.into() },
        ActionKind::StoreVerify { vm: vm.into() },
    ];

    if let Some(target) = running_vm(state, vm) {
        for claim in &target.usb {
            actions.push(ActionKind::UsbAttach {
                vm: vm.into(),
                bus_id: claim.bus_id.clone(),
            });
            actions.push(ActionKind::UsbDetach {
                vm: vm.into(),
                bus_id: claim.bus_id.clone(),
            });
        }
    }

    actions.extend([
        ActionKind::AudioMic {
            vm: vm.into(),
            on: true,
        },
        ActionKind::AudioSpeaker {
            vm: vm.into(),
            on: true,
        },
        ActionKind::AudioOff { vm: vm.into() },
    ]);

    actions
        .into_iter()
        .map(|action| availability(action, state, config))
        .collect()
}

/// Plan a concrete dispatch for an action, or return why it is blocked.
pub fn plan(
    action: &ActionKind,
    state: &WlState,
    config: &Config,
) -> Result<PlannedAction, Unavailable> {
    if let Some(reason) = block_reason(action, state) {
        return Err(reason);
    }
    if let Some(reason) = config_block_reason(action, config) {
        return Err(reason);
    }

    let dispatch = match action {
        ActionKind::Start { vm } => socket(SocketIntent::VmStart { vm: vm.clone() }),
        ActionKind::Stop { vm } => socket(SocketIntent::VmStop { vm: vm.clone() }),
        ActionKind::Restart { vm } => socket(SocketIntent::VmRestart { vm: vm.clone() }),
        ActionKind::Switch { vm } => socket(SocketIntent::Switch { vm: vm.clone() }),
        ActionKind::UsbAttach { vm, bus_id } => socket(SocketIntent::UsbAttach {
            vm: vm.clone(),
            bus_id: bus_id.clone(),
        }),
        ActionKind::UsbDetach { vm, bus_id } => socket(SocketIntent::UsbDetach {
            vm: vm.clone(),
            bus_id: bus_id.clone(),
        }),
        ActionKind::StoreVerify { vm } => socket(SocketIntent::StoreVerify { vm: vm.clone() }),
        ActionKind::Refresh => socket(SocketIntent::List),
        ActionKind::LaunchTerminal { vm } => terminal_argv(vm, config),
        ActionKind::AudioMic { .. }
        | ActionKind::AudioSpeaker { .. }
        | ActionKind::AudioOff { .. } => return Err(Unavailable::NotYetImplemented),
        ActionKind::OpenControlCenter | ActionKind::CycleDisplay => {
            // These are handled in-process by the UI/Waybar layers, not as a
            // nixling dispatch; planning them is a no-op socket refresh.
            return Err(Unavailable::Blocked {
                detail: "handled in-process; not a nixling dispatch".into(),
            });
        }
    };
    Ok(dispatch)
}

fn availability(action: ActionKind, state: &WlState, config: &Config) -> ActionAvailability {
    let unavailable = block_reason(&action, state).or_else(|| config_block_reason(&action, config));
    ActionAvailability {
        action,
        unavailable,
    }
}

fn config_block_reason(action: &ActionKind, config: &Config) -> Option<Unavailable> {
    if matches!(action, ActionKind::LaunchTerminal { .. }) {
        return config.validate().err().map(|err| Unavailable::Blocked {
            detail: err.to_string(),
        });
    }
    None
}

fn required_role(action: &ActionKind) -> AuthRole {
    match action {
        ActionKind::LaunchTerminal { .. } => AuthRole::Admin,
        ActionKind::Start { .. }
        | ActionKind::Stop { .. }
        | ActionKind::Restart { .. }
        | ActionKind::Switch { .. }
        | ActionKind::UsbAttach { .. }
        | ActionKind::UsbDetach { .. }
        | ActionKind::StoreVerify { .. } => AuthRole::Launcher,
        _ => AuthRole::None,
    }
}

fn role_satisfies(have: AuthRole, need: AuthRole) -> bool {
    rank(have) >= rank(need)
}

fn rank(role: AuthRole) -> u8 {
    match role {
        AuthRole::None => 0,
        AuthRole::Launcher => 1,
        AuthRole::Admin => 2,
    }
}

fn running_vm<'a>(state: &'a WlState, name: &str) -> Option<&'a Vm> {
    state.vms.iter().find(|v| v.name == name)
}

fn usb_attach_block(state: &WlState, vm: &str, bus_id: &str) -> Option<Unavailable> {
    let claim = state
        .vms
        .iter()
        .flat_map(|v| v.usb.iter())
        .find(|c| c.bus_id == bus_id);
    match claim {
        Some(c) if c.bound => match &c.owner_vm {
            Some(owner) if owner != vm => Some(Unavailable::UsbOwnedElsewhere {
                owner: owner.clone(),
            }),
            _ => None,
        },
        _ => None,
    }
}

fn usb_detach_block(state: &WlState, vm: &str, bus_id: &str) -> Option<Unavailable> {
    let claim = state
        .vms
        .iter()
        .flat_map(|v| v.usb.iter())
        .find(|c| c.bus_id == bus_id);
    match claim {
        Some(c) if c.bound => match &c.owner_vm {
            Some(owner) if owner == vm => None,
            Some(owner) => Some(Unavailable::UsbOwnedElsewhere {
                owner: owner.clone(),
            }),
            None => None,
        },
        _ => Some(Unavailable::VmState {
            detail: "device is not bound".into(),
        }),
    }
}

fn socket(intent: SocketIntent) -> PlannedAction {
    PlannedAction::Socket { intent }
}

/// Build the argv-only terminal launch command. There is no shell string and no
/// interpolation: the terminal argv prefix, the nixling exec invocation, and the
/// guest shell are concatenated as discrete argv elements.
fn terminal_argv(vm: &str, config: &Config) -> PlannedAction {
    let mut argv = config.terminal.argv.clone();
    argv.extend([
        "nixling".to_owned(),
        "vm".to_owned(),
        "exec".to_owned(),
        "-it".to_owned(),
        vm.to_owned(),
        "--".to_owned(),
        config.terminal.guest_shell.clone(),
    ]);
    PlannedAction::Process { argv }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::UsbClaim;

    fn connected_state(role: AuthRole, vms: Vec<Vm>) -> WlState {
        WlState {
            connectivity: Connectivity::Connected,
            role,
            vms,
            stale: false,
            note: None,
        }
    }

    fn vm(name: &str, state: RuntimeState) -> Vm {
        Vm {
            name: name.into(),
            state,
            ..Default::default()
        }
    }

    #[test]
    fn daemon_down_blocks_lifecycle() {
        let state = WlState {
            connectivity: Connectivity::DaemonDown,
            ..Default::default()
        };
        let reason = block_reason(
            &ActionKind::Start {
                vm: "corp-vm".into(),
            },
            &state,
        );
        assert!(matches!(reason, Some(Unavailable::DaemonDown)));
    }

    #[test]
    fn terminal_requires_admin() {
        let state = connected_state(
            AuthRole::Launcher,
            vec![vm("corp-vm", RuntimeState::Running)],
        );
        let reason = block_reason(
            &ActionKind::LaunchTerminal {
                vm: "corp-vm".into(),
            },
            &state,
        );
        assert!(matches!(
            reason,
            Some(Unavailable::InsufficientRole {
                required: AuthRole::Admin
            })
        ));
    }

    #[test]
    fn terminal_argv_has_no_shell_string() {
        let state = connected_state(AuthRole::Admin, vec![vm("corp-vm", RuntimeState::Running)]);
        let config = Config::default();
        let planned = plan(
            &ActionKind::LaunchTerminal {
                vm: "corp-vm".into(),
            },
            &state,
            &config,
        )
        .expect("plannable");
        match planned {
            PlannedAction::Process { argv } => {
                assert_eq!(argv[0], "foot");
                assert!(argv.contains(&"corp-vm".to_owned()));
                assert!(argv.iter().all(|a| !a.contains("&&") && !a.contains("|")));
            }
            other => panic!("expected process, got {other:?}"),
        }
    }

    #[test]
    fn start_blocked_when_already_running() {
        let state = connected_state(
            AuthRole::Launcher,
            vec![vm("corp-vm", RuntimeState::Running)],
        );
        let reason = block_reason(
            &ActionKind::Start {
                vm: "corp-vm".into(),
            },
            &state,
        );
        assert!(matches!(reason, Some(Unavailable::VmState { .. })));
    }

    #[test]
    fn audio_actions_are_not_yet_implemented_even_when_daemon_is_down() {
        let state = WlState {
            connectivity: Connectivity::DaemonDown,
            ..Default::default()
        };
        let actions = [
            ActionKind::AudioMic {
                vm: "corp-vm".into(),
                on: true,
            },
            ActionKind::AudioSpeaker {
                vm: "corp-vm".into(),
                on: true,
            },
            ActionKind::AudioOff {
                vm: "corp-vm".into(),
            },
        ];

        for action in actions {
            assert!(matches!(
                block_reason(&action, &state),
                Some(Unavailable::NotYetImplemented)
            ));
            assert!(matches!(
                plan(&action, &state, &Config::default()),
                Err(Unavailable::NotYetImplemented)
            ));
        }
    }

    #[test]
    fn vm_actions_returns_lifecycle_usb_terminal_store_and_audio() {
        let mut target = vm("corp-vm", RuntimeState::Running);
        target.usb.push(UsbClaim {
            vm: "corp-vm".into(),
            env: "work".into(),
            bus_id: "1-2".into(),
            bound: false,
            owner_vm: None,
        });
        let state = connected_state(AuthRole::Admin, vec![target]);

        let actions = vm_actions(&state, &Config::default(), "corp-vm");

        assert_eq!(actions.len(), 11);
        assert!(matches!(&actions[0].action, ActionKind::Start { .. }));
        assert!(matches!(&actions[6].action, ActionKind::UsbAttach { .. }));
        assert!(matches!(&actions[7].action, ActionKind::UsbDetach { .. }));
        assert!(matches!(&actions[8].action, ActionKind::AudioMic { .. }));
        assert!(matches!(
            actions[8].unavailable.as_ref(),
            Some(Unavailable::NotYetImplemented)
        ));
        assert!(matches!(
            &actions[9].action,
            ActionKind::AudioSpeaker { .. }
        ));
        assert!(matches!(
            actions[9].unavailable.as_ref(),
            Some(Unavailable::NotYetImplemented)
        ));
        assert!(matches!(&actions[10].action, ActionKind::AudioOff { .. }));
        assert!(matches!(
            actions[10].unavailable.as_ref(),
            Some(Unavailable::NotYetImplemented)
        ));
    }

    #[test]
    fn vm_actions_blocks_terminal_when_config_is_invalid() {
        let state = connected_state(AuthRole::Admin, vec![vm("corp-vm", RuntimeState::Running)]);
        let config = Config {
            terminal: crate::config::TerminalConfig {
                argv: vec![],
                ..Default::default()
            },
            ..Default::default()
        };

        let actions = vm_actions(&state, &config, "corp-vm");

        let terminal = actions
            .iter()
            .find(|entry| matches!(&entry.action, ActionKind::LaunchTerminal { .. }))
            .expect("terminal action");
        assert!(matches!(
            &terminal.unavailable,
            Some(Unavailable::Blocked { detail }) if detail.contains("terminal.argv")
        ));
    }
}
