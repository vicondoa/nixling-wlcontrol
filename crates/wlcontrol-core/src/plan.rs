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
        ActionKind::OpenControlCenter
        | ActionKind::OpenObservability
        | ActionKind::CycleDisplay
        | ActionKind::Refresh => return None,
        _ => {}
    }

    if state.connectivity == Connectivity::DaemonDown {
        return Some(Unavailable::DaemonDown);
    }

    let required = required_role(action);
    if !role_satisfies(state.role, required) {
        return Some(Unavailable::InsufficientRole { required });
    }
    if let Some(reason) = capability_block(action, state) {
        return Some(reason);
    }

    match action {
        ActionKind::Start { vm } => running_vm(state, vm)
            .filter(|v| v.state == RuntimeState::Running)
            .map(|_| Unavailable::VmState {
                detail: "VM is already running".into(),
            }),
        ActionKind::Stop { vm }
        | ActionKind::ForceStop { vm }
        | ActionKind::Restart { vm }
        | ActionKind::Switch { vm } => running_vm(state, vm)
            .filter(|v| v.state == RuntimeState::Stopped)
            .map(|_| Unavailable::VmState {
                detail: "VM is not running".into(),
            }),
        ActionKind::LaunchTerminal { vm } => running_vm(state, vm)
            .filter(|v| v.state != RuntimeState::Running)
            .map(|_| Unavailable::VmState {
                detail: "start the VM before opening a terminal".into(),
            }),
        ActionKind::QuickLaunch { vm, .. } => running_vm(state, vm)
            .filter(|v| v.state != RuntimeState::Running)
            .map(|_| Unavailable::VmState {
                detail: "start the VM before using quick launch".into(),
            }),
        ActionKind::UsbAttach { vm, bus_id } => usb_attach_block(state, vm, bus_id),
        ActionKind::UsbDetach { vm, bus_id } => usb_detach_block(state, vm, bus_id),
        ActionKind::StoreVerify { .. } | ActionKind::Build { .. } | ActionKind::Boot { .. } => None,
        _ => None,
    }
}

fn capability_block(action: &ActionKind, state: &WlState) -> Option<Unavailable> {
    let vm = action_target_vm(action)?;
    let target = running_vm(state, vm)?;
    let supported = match action {
        ActionKind::Start { .. } => target.capabilities.start,
        ActionKind::Stop { .. } | ActionKind::ForceStop { .. } => target.capabilities.stop,
        ActionKind::Restart { .. } => target.capabilities.restart,
        ActionKind::Switch { .. } => target.capabilities.switch,
        ActionKind::Build { .. } => target.capabilities.build,
        ActionKind::Boot { .. } => target.capabilities.boot,
        ActionKind::UsbAttach { .. } | ActionKind::UsbDetach { .. } => {
            target.capabilities.usb_hotplug
        }
        ActionKind::StoreVerify { .. } => target.capabilities.store_verify,
        ActionKind::LaunchTerminal { .. } | ActionKind::QuickLaunch { .. } => {
            target.capabilities.terminal
        }
        _ => true,
    };
    (!supported).then(|| Unavailable::Blocked {
        detail: "unsupported by this VM runtime".into(),
    })
}

fn action_target_vm(action: &ActionKind) -> Option<&str> {
    match action {
        ActionKind::Start { vm }
        | ActionKind::Stop { vm }
        | ActionKind::ForceStop { vm }
        | ActionKind::Restart { vm }
        | ActionKind::Switch { vm }
        | ActionKind::Build { vm }
        | ActionKind::Boot { vm }
        | ActionKind::QuickLaunch { vm, .. }
        | ActionKind::UsbAttach { vm, .. }
        | ActionKind::UsbDetach { vm, .. }
        | ActionKind::StoreVerify { vm }
        | ActionKind::LaunchTerminal { vm }
        | ActionKind::AudioMic { vm, .. }
        | ActionKind::AudioSpeaker { vm, .. }
        | ActionKind::AudioOff { vm } => Some(vm.as_str()),
        ActionKind::Refresh
        | ActionKind::OpenControlCenter
        | ActionKind::OpenObservability
        | ActionKind::CycleDisplay => None,
    }
}

/// Return the full per-VM action list the control center can render.
pub fn vm_actions(state: &WlState, config: &Config, vm: &str) -> Vec<ActionAvailability> {
    let mut actions = vec![
        ActionKind::Start { vm: vm.into() },
        ActionKind::Stop { vm: vm.into() },
        ActionKind::ForceStop { vm: vm.into() },
        ActionKind::Restart { vm: vm.into() },
        ActionKind::LaunchTerminal { vm: vm.into() },
        ActionKind::StoreVerify { vm: vm.into() },
        ActionKind::Build { vm: vm.into() },
        ActionKind::Boot { vm: vm.into() },
        ActionKind::Switch { vm: vm.into() },
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
        ActionKind::Stop { vm } => socket(SocketIntent::VmStop {
            vm: vm.clone(),
            force: false,
        }),
        ActionKind::ForceStop { vm } => socket(SocketIntent::VmStop {
            vm: vm.clone(),
            force: true,
        }),
        ActionKind::Restart { vm } => socket(SocketIntent::VmRestart { vm: vm.clone() }),
        ActionKind::Switch { vm } => socket(SocketIntent::Switch { vm: vm.clone() }),
        ActionKind::Build { vm } => build_argv(vm),
        ActionKind::Boot { vm } => socket(SocketIntent::Boot { vm: vm.clone() }),
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
        ActionKind::QuickLaunch { vm, id } => quick_launch_argv(vm, id, config)?,
        ActionKind::OpenObservability => observability_argv(config),
        ActionKind::AudioMic { .. }
        | ActionKind::AudioSpeaker { .. }
        | ActionKind::AudioOff { .. } => return Err(Unavailable::NotYetImplemented),
        ActionKind::OpenControlCenter | ActionKind::CycleDisplay => {
            // These are handled in-process by the UI/Waybar layers, not as a
            // d2b dispatch; planning them is a no-op socket refresh.
            return Err(Unavailable::Blocked {
                detail: "handled in-process; not a d2b dispatch".into(),
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
    if matches!(
        action,
        ActionKind::LaunchTerminal { .. } | ActionKind::QuickLaunch { .. }
    ) {
        return config.validate().err().map(|err| Unavailable::Blocked {
            detail: err.to_string(),
        });
    }
    if let ActionKind::QuickLaunch { vm, id } = action {
        if quick_launch_config(vm, id, config).is_none() {
            return Some(Unavailable::Blocked {
                detail: format!("quick launch '{id}' is not configured for {vm}"),
            });
        }
    }
    if matches!(action, ActionKind::OpenObservability) {
        if let Err(err) = config.validate() {
            return Some(Unavailable::Blocked {
                detail: err.to_string(),
            });
        }
        if !config.observability.enabled {
            return Some(Unavailable::Blocked {
                detail: "observability is disabled".into(),
            });
        }
        if config.observability.url.is_none() {
            return Some(Unavailable::Blocked {
                detail: "observability.url is not configured".into(),
            });
        }
    }
    None
}

fn required_role(action: &ActionKind) -> AuthRole {
    match action {
        ActionKind::LaunchTerminal { .. }
        | ActionKind::QuickLaunch { .. }
        | ActionKind::Start { .. }
        | ActionKind::Stop { .. }
        | ActionKind::ForceStop { .. }
        | ActionKind::Restart { .. }
        | ActionKind::Switch { .. }
        | ActionKind::Boot { .. }
        | ActionKind::UsbAttach { .. }
        | ActionKind::UsbDetach { .. }
        | ActionKind::StoreVerify { .. } => AuthRole::Admin,
        ActionKind::Build { .. } => AuthRole::Launcher,
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

/// Build the argv-only detached terminal launch command. There is no shell
/// string and no interpolation: the d2b exec invocation and guest terminal
/// command are concatenated as discrete argv elements.
fn terminal_argv(vm: &str, config: &Config) -> PlannedAction {
    let mut argv = vec![
        "d2b".to_owned(),
        "vm".to_owned(),
        "exec".to_owned(),
        "-d".to_owned(),
        vm.to_owned(),
        "--".to_owned(),
    ];
    if config.terminal.guest_argv.is_empty() {
        argv.push(config.terminal.guest_shell.clone());
    } else {
        argv.extend(config.terminal.guest_argv.clone());
    }
    PlannedAction::Process { argv, wait: true }
}

fn quick_launch_argv(vm: &str, id: &str, config: &Config) -> Result<PlannedAction, Unavailable> {
    let item = quick_launch_config(vm, id, config).ok_or_else(|| Unavailable::Blocked {
        detail: format!("quick launch '{id}' is not configured for {vm}"),
    })?;
    let mut argv = vec![
        "d2b".to_owned(),
        "vm".to_owned(),
        "exec".to_owned(),
        "-d".to_owned(),
        vm.to_owned(),
        "--".to_owned(),
    ];
    argv.extend(item.guest_argv.clone());
    Ok(PlannedAction::Process { argv, wait: true })
}

fn quick_launch_config<'a>(
    vm: &str,
    id: &str,
    config: &'a Config,
) -> Option<&'a crate::config::QuickLaunchConfig> {
    config
        .quick_launch
        .iter()
        .find(|item| item.vm == vm && item.id == id)
}

fn build_argv(vm: &str) -> PlannedAction {
    PlannedAction::Process {
        argv: vec!["d2b".to_owned(), "build".to_owned(), vm.to_owned()],
        wait: true,
    }
}

fn observability_argv(config: &Config) -> PlannedAction {
    let mut argv = config.observability.browser_argv.clone();
    if let Some(url) = &config.observability.url {
        argv.push(url.clone());
    }
    PlannedAction::Process { argv, wait: false }
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

    fn usb_claim(vm: &str, bus_id: &str, bound: bool, owner_vm: Option<&str>) -> UsbClaim {
        UsbClaim {
            vm: vm.into(),
            env: "work".into(),
            bus_id: bus_id.into(),
            bound,
            owner_vm: owner_vm.map(str::to_owned),
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
    fn role_gating_distinguishes_lifecycle_and_terminal_privileges() {
        let no_role = connected_state(AuthRole::None, vec![vm("corp-vm", RuntimeState::Stopped)]);
        let lifecycle = ActionKind::Start {
            vm: "corp-vm".into(),
        };
        assert!(matches!(
            plan(&lifecycle, &no_role, &Config::default()),
            Err(Unavailable::InsufficientRole {
                required: AuthRole::Admin
            })
        ));

        let launcher = connected_state(
            AuthRole::Launcher,
            vec![vm("corp-vm", RuntimeState::Stopped)],
        );
        assert!(matches!(
            plan(&lifecycle, &launcher, &Config::default()),
            Err(Unavailable::InsufficientRole {
                required: AuthRole::Admin
            })
        ));
        let build = ActionKind::Build {
            vm: "corp-vm".into(),
        };
        assert!(plan(&build, &launcher, &Config::default()).is_ok());

        let running_launcher = connected_state(
            AuthRole::Launcher,
            vec![vm("corp-vm", RuntimeState::Running)],
        );
        let terminal = ActionKind::LaunchTerminal {
            vm: "corp-vm".into(),
        };
        assert!(matches!(
            plan(&terminal, &running_launcher, &Config::default()),
            Err(Unavailable::InsufficientRole {
                required: AuthRole::Admin
            })
        ));

        let admin = connected_state(AuthRole::Admin, vec![vm("corp-vm", RuntimeState::Running)]);
        assert!(plan(&terminal, &admin, &Config::default()).is_ok());
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
            PlannedAction::Process { argv, wait } => {
                assert!(wait);
                assert_eq!(argv[0], "d2b");
                assert!(argv.contains(&"corp-vm".to_owned()));
                assert!(argv.contains(&"-d".to_owned()));
                assert!(!argv.contains(&"-it".to_owned()));
                assert!(argv.iter().all(|a| !a.contains("&&") && !a.contains("|")));
            }
            other => panic!("expected process, got {other:?}"),
        }
    }

    #[test]
    fn start_blocked_when_already_running() {
        let state = connected_state(AuthRole::Admin, vec![vm("corp-vm", RuntimeState::Running)]);
        let reason = block_reason(
            &ActionKind::Start {
                vm: "corp-vm".into(),
            },
            &state,
        );
        assert!(matches!(reason, Some(Unavailable::VmState { .. })));
    }

    #[test]
    fn running_state_gates_stop_restart_switch_and_terminal() {
        let stopped_admin =
            connected_state(AuthRole::Admin, vec![vm("corp-vm", RuntimeState::Stopped)]);
        for action in [
            ActionKind::Stop {
                vm: "corp-vm".into(),
            },
            ActionKind::ForceStop {
                vm: "corp-vm".into(),
            },
            ActionKind::Restart {
                vm: "corp-vm".into(),
            },
            ActionKind::Switch {
                vm: "corp-vm".into(),
            },
        ] {
            assert!(matches!(
                plan(&action, &stopped_admin, &Config::default()),
                Err(Unavailable::VmState { .. })
            ));
        }

        let terminal = ActionKind::LaunchTerminal {
            vm: "corp-vm".into(),
        };
        assert!(matches!(
            plan(&terminal, &stopped_admin, &Config::default()),
            Err(Unavailable::VmState { .. })
        ));

        let running_launcher = connected_state(
            AuthRole::Launcher,
            vec![vm("corp-vm", RuntimeState::Running)],
        );
        for action in [
            ActionKind::Stop {
                vm: "corp-vm".into(),
            },
            ActionKind::ForceStop {
                vm: "corp-vm".into(),
            },
            ActionKind::Restart {
                vm: "corp-vm".into(),
            },
            ActionKind::Switch {
                vm: "corp-vm".into(),
            },
        ] {
            assert!(matches!(
                plan(&action, &running_launcher, &Config::default()),
                Err(Unavailable::InsufficientRole {
                    required: AuthRole::Admin
                })
            ));
        }

        let running_admin =
            connected_state(AuthRole::Admin, vec![vm("corp-vm", RuntimeState::Running)]);
        assert!(plan(&terminal, &running_admin, &Config::default()).is_ok());
        for action in [
            ActionKind::Stop {
                vm: "corp-vm".into(),
            },
            ActionKind::ForceStop {
                vm: "corp-vm".into(),
            },
            ActionKind::Restart {
                vm: "corp-vm".into(),
            },
            ActionKind::Switch {
                vm: "corp-vm".into(),
            },
        ] {
            assert!(plan(&action, &running_admin, &Config::default()).is_ok());
        }
    }

    #[test]
    fn runtime_capabilities_hide_unsupported_controls() {
        let mut qemu = vm("media-vm", RuntimeState::Running);
        qemu.capabilities.terminal = false;
        qemu.capabilities.store_verify = false;
        qemu.capabilities.switch = false;
        qemu.capabilities.build = false;
        qemu.capabilities.boot = false;
        let state = connected_state(AuthRole::Admin, vec![qemu]);

        for action in [
            ActionKind::LaunchTerminal {
                vm: "media-vm".into(),
            },
            ActionKind::StoreVerify {
                vm: "media-vm".into(),
            },
            ActionKind::Switch {
                vm: "media-vm".into(),
            },
            ActionKind::Build {
                vm: "media-vm".into(),
            },
            ActionKind::Boot {
                vm: "media-vm".into(),
            },
        ] {
            assert!(matches!(
                block_reason(&action, &state),
                Some(Unavailable::Blocked { detail }) if detail == "unsupported by this VM runtime"
            ));
        }

        assert!(block_reason(
            &ActionKind::UsbAttach {
                vm: "media-vm".into(),
                bus_id: "1-2".into(),
            },
            &state
        )
        .is_none());
    }

    #[test]
    fn usb_attach_and_detach_respect_foreign_owner() {
        let mut owner = vm("dev-vm", RuntimeState::Running);
        owner
            .usb
            .push(usb_claim("dev-vm", "1-2", true, Some("dev-vm")));
        let state = connected_state(
            AuthRole::Admin,
            vec![vm("corp-vm", RuntimeState::Running), owner],
        );

        for action in [
            ActionKind::UsbAttach {
                vm: "corp-vm".into(),
                bus_id: "1-2".into(),
            },
            ActionKind::UsbDetach {
                vm: "corp-vm".into(),
                bus_id: "1-2".into(),
            },
        ] {
            assert!(matches!(
                plan(&action, &state, &Config::default()),
                Err(Unavailable::UsbOwnedElsewhere { owner }) if owner == "dev-vm"
            ));
        }

        let detach_owner = ActionKind::UsbDetach {
            vm: "dev-vm".into(),
            bus_id: "1-2".into(),
        };
        assert!(plan(&detach_owner, &state, &Config::default()).is_ok());
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

        assert_eq!(actions.len(), 14);
        assert!(matches!(&actions[0].action, ActionKind::Start { .. }));
        assert!(matches!(&actions[1].action, ActionKind::Stop { .. }));
        assert!(matches!(&actions[2].action, ActionKind::ForceStop { .. }));
        assert!(matches!(&actions[6].action, ActionKind::Build { .. }));
        assert!(matches!(&actions[7].action, ActionKind::Boot { .. }));
        assert!(matches!(&actions[8].action, ActionKind::Switch { .. }));
        assert!(matches!(&actions[9].action, ActionKind::UsbAttach { .. }));
        assert!(matches!(&actions[10].action, ActionKind::UsbDetach { .. }));
        assert!(matches!(&actions[11].action, ActionKind::AudioMic { .. }));
        assert!(matches!(
            actions[11].unavailable.as_ref(),
            Some(Unavailable::NotYetImplemented)
        ));
        assert!(matches!(
            &actions[12].action,
            ActionKind::AudioSpeaker { .. }
        ));
        assert!(matches!(
            actions[12].unavailable.as_ref(),
            Some(Unavailable::NotYetImplemented)
        ));
        assert!(matches!(&actions[13].action, ActionKind::AudioOff { .. }));
        assert!(matches!(
            actions[13].unavailable.as_ref(),
            Some(Unavailable::NotYetImplemented)
        ));
    }

    #[test]
    fn vm_actions_blocks_terminal_when_config_is_invalid() {
        let state = connected_state(AuthRole::Admin, vec![vm("corp-vm", RuntimeState::Running)]);
        let config = Config {
            terminal: crate::config::TerminalConfig {
                guest_shell: String::new(),
                guest_argv: vec![],
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
            Some(Unavailable::Blocked { detail }) if detail.contains("terminal.guest_argv")
        ));
    }

    #[test]
    fn build_and_boot_plan_to_process_and_socket() {
        let state = connected_state(AuthRole::Admin, vec![vm("corp-vm", RuntimeState::Running)]);

        let build = plan(
            &ActionKind::Build {
                vm: "corp-vm".into(),
            },
            &connected_state(
                AuthRole::Launcher,
                vec![vm("corp-vm", RuntimeState::Running)],
            ),
            &Config::default(),
        )
        .expect("build plannable");
        assert_eq!(
            build,
            PlannedAction::Process {
                argv: vec!["d2b".into(), "build".into(), "corp-vm".into()],
                wait: true,
            }
        );

        let boot = plan(
            &ActionKind::Boot {
                vm: "corp-vm".into(),
            },
            &state,
            &Config::default(),
        )
        .expect("boot plannable");
        assert_eq!(
            boot,
            PlannedAction::Socket {
                intent: SocketIntent::Boot {
                    vm: "corp-vm".into()
                }
            }
        );
    }

    #[test]
    fn stop_plans_graceful_by_default_and_force_sets_socket_flag() {
        let state = connected_state(AuthRole::Admin, vec![vm("corp-vm", RuntimeState::Running)]);

        let normal = plan(
            &ActionKind::Stop {
                vm: "corp-vm".into(),
            },
            &state,
            &Config::default(),
        )
        .expect("normal stop plannable");
        assert_eq!(
            normal,
            PlannedAction::Socket {
                intent: SocketIntent::VmStop {
                    vm: "corp-vm".into(),
                    force: false
                }
            }
        );

        let force = plan(
            &ActionKind::ForceStop {
                vm: "corp-vm".into(),
            },
            &state,
            &Config::default(),
        )
        .expect("force stop plannable");
        assert_eq!(
            force,
            PlannedAction::Socket {
                intent: SocketIntent::VmStop {
                    vm: "corp-vm".into(),
                    force: true
                }
            }
        );
    }

    #[test]
    fn process_actions_deserialize_without_wait_for_compatibility() {
        let action: PlannedAction =
            serde_json::from_str(r#"{"dispatch":"process","argv":["d2b","build","corp-vm"]}"#)
                .expect("old process action should deserialize");
        assert_eq!(
            action,
            PlannedAction::Process {
                argv: vec!["d2b".into(), "build".into(), "corp-vm".into()],
                wait: false,
            }
        );
    }

    #[test]
    fn observability_plan_opens_configured_url_without_daemon_state() {
        let planned = plan(
            &ActionKind::OpenObservability,
            &WlState::default(),
            &Config::default(),
        )
        .expect("observability plannable");

        assert_eq!(
            planned,
            PlannedAction::Process {
                argv: vec!["xdg-open".into(), "http://sys-obs:8080".into()],
                wait: false,
            }
        );
    }

    #[test]
    fn quick_launch_uses_configured_detached_guest_argv() {
        let state = connected_state(AuthRole::Admin, vec![vm("work-ssd", RuntimeState::Running)]);
        let config = Config {
            quick_launch: vec![crate::config::QuickLaunchConfig {
                id: "run-openterface".into(),
                vm: "work-ssd".into(),
                icon: "desktop_windows".into(),
                tooltip: "Run Openterface".into(),
                guest_argv: vec!["/run/current-system/sw/bin/openterface-run".into()],
            }],
            ..Default::default()
        };

        let planned = plan(
            &ActionKind::QuickLaunch {
                vm: "work-ssd".into(),
                id: "run-openterface".into(),
            },
            &state,
            &config,
        )
        .expect("quick launch plannable");

        assert_eq!(
            planned,
            PlannedAction::Process {
                argv: vec![
                    "d2b".into(),
                    "vm".into(),
                    "exec".into(),
                    "-d".into(),
                    "work-ssd".into(),
                    "--".into(),
                    "/run/current-system/sw/bin/openterface-run".into()
                ],
                wait: true,
            }
        );
    }
}
