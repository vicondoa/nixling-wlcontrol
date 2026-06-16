use wlcontrol_core::model::{
    ActionKind, AuthRole, Connectivity, RuntimeState, Unavailable, UsbClaim, Vm, WlState,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VmGroup {
    pub(crate) env: String,
    pub(crate) vms: Vec<Vm>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BadgeSpec {
    pub(crate) label: &'static str,
    pub(crate) css_class: &'static str,
}

pub(crate) fn visible_vm_groups(state: &WlState, show_internal: bool) -> Vec<VmGroup> {
    let mut groups = Vec::<VmGroup>::new();

    for vm in state
        .vms
        .iter()
        .filter(|vm| show_internal || (!vm.is_net_vm && !vm.hidden))
    {
        let env = vm.env.as_deref().unwrap_or("default").to_owned();
        if let Some(group) = groups.iter_mut().find(|group| group.env == env) {
            group.vms.push(vm.clone());
        } else {
            groups.push(VmGroup {
                env,
                vms: vec![vm.clone()],
            });
        }
    }

    groups
}

pub(crate) fn state_badge(state: RuntimeState) -> BadgeSpec {
    match state {
        RuntimeState::Running => BadgeSpec {
            label: "Running",
            css_class: "state-running",
        },
        RuntimeState::Stopped => BadgeSpec {
            label: "Stopped",
            css_class: "state-stopped",
        },
        RuntimeState::Starting => BadgeSpec {
            label: "Starting",
            css_class: "state-progress",
        },
        RuntimeState::Stopping => BadgeSpec {
            label: "Stopping",
            css_class: "state-progress",
        },
        RuntimeState::Unknown => BadgeSpec {
            label: "Unknown",
            css_class: "state-unknown",
        },
    }
}

pub(crate) fn unavailable_tooltip(reason: &Unavailable) -> String {
    match reason {
        Unavailable::DaemonDown => "nixlingd is unreachable".to_owned(),
        Unavailable::InsufficientRole { required } => {
            format!("requires {}", role_name(*required))
        }
        Unavailable::VmState { detail } => detail.clone(),
        Unavailable::UsbOwnedElsewhere { owner } => format!("USB owned by {owner}"),
        Unavailable::NotYetImplemented => {
            "unsupported by the current nixling audio control plane".to_owned()
        }
        Unavailable::Blocked { detail } => detail.clone(),
    }
}

pub(crate) fn action_label(action: &ActionKind) -> String {
    match action {
        ActionKind::Refresh => "Refresh".to_owned(),
        ActionKind::Start { .. } => "Start".to_owned(),
        ActionKind::Stop { .. } => "Stop".to_owned(),
        ActionKind::Restart { .. } => "Restart".to_owned(),
        ActionKind::Switch { .. } => "Switch".to_owned(),
        ActionKind::UsbAttach { bus_id, .. } => format!("USB attach {bus_id}"),
        ActionKind::UsbDetach { bus_id, .. } => format!("USB detach {bus_id}"),
        ActionKind::StoreVerify { .. } => "Store verify".to_owned(),
        ActionKind::LaunchTerminal { .. } => "Launch terminal".to_owned(),
        ActionKind::AudioMic { .. } => "Mic".to_owned(),
        ActionKind::AudioSpeaker { .. } => "Speaker".to_owned(),
        ActionKind::AudioOff { .. } => "Audio off".to_owned(),
        ActionKind::OpenControlCenter => "Open control center".to_owned(),
        ActionKind::CycleDisplay => "Cycle display".to_owned(),
    }
}

pub(crate) fn role_name(role: AuthRole) -> &'static str {
    match role {
        AuthRole::None => "none",
        AuthRole::Launcher => "launcher",
        AuthRole::Admin => "admin",
    }
}

pub(crate) fn role_indicator(state: &WlState) -> String {
    match state.connectivity {
        Connectivity::DaemonDown => "daemon down".to_owned(),
        Connectivity::AuthDenied => "role: none".to_owned(),
        Connectivity::Connected => format!("role: {}", role_name(state.role)),
    }
}

pub(crate) fn action_vm_name(action: &ActionKind) -> Option<&str> {
    match action {
        ActionKind::Start { vm }
        | ActionKind::Stop { vm }
        | ActionKind::Restart { vm }
        | ActionKind::Switch { vm }
        | ActionKind::UsbAttach { vm, .. }
        | ActionKind::UsbDetach { vm, .. }
        | ActionKind::StoreVerify { vm }
        | ActionKind::LaunchTerminal { vm }
        | ActionKind::AudioMic { vm, .. }
        | ActionKind::AudioSpeaker { vm, .. }
        | ActionKind::AudioOff { vm } => Some(vm.as_str()),
        ActionKind::Refresh | ActionKind::OpenControlCenter | ActionKind::CycleDisplay => None,
    }
}

pub(crate) fn needs_confirmation(action: &ActionKind, vm: &Vm) -> bool {
    matches!(
        action,
        ActionKind::Stop { .. } | ActionKind::Restart { .. } | ActionKind::Switch { .. }
    ) && vm.state == RuntimeState::Running
}

pub(crate) fn vm_subtitle(vm: &Vm, show_pending_restart: bool) -> String {
    let mut parts = Vec::new();

    if let Some(ip) = &vm.static_ip {
        parts.push(format!("IP {ip}"));
    } else {
        parts.push("IP —".to_owned());
    }

    if vm.readiness.is_empty() {
        parts.push("readiness not reported".to_owned());
    } else {
        parts.push(format!("ready: {}", vm.readiness.join(", ")));
    }

    let usb = usb_summary(&vm.usb);
    if !usb.is_empty() {
        parts.push(usb);
    }

    if show_pending_restart && vm.pending_restart {
        parts.push("pending restart".to_owned());
    }
    if vm.is_net_vm {
        parts.push("net VM".to_owned());
    }
    if vm.hidden {
        parts.push("hidden".to_owned());
    }

    parts.join(" • ")
}

pub(crate) fn usb_summary(claims: &[UsbClaim]) -> String {
    match claims {
        [] => String::new(),
        [claim] => format!("USB {}", usb_claim_summary(claim)),
        claims => {
            let bound = claims.iter().filter(|claim| claim.bound).count();
            format!("USB {} claim(s), {bound} bound", claims.len())
        }
    }
}

pub(crate) fn usb_claim_summary(claim: &UsbClaim) -> String {
    match (&claim.bound, &claim.owner_vm) {
        (true, Some(owner)) => format!("{} bound to {owner}", claim.bus_id),
        (true, None) => format!("{} bound", claim.bus_id),
        (false, _) => format!("{} available", claim.bus_id),
    }
}

pub(crate) fn empty_group_message(show_internal: bool) -> &'static str {
    if show_internal {
        "No VMs were reported by nixlingd."
    } else {
        "No visible VMs. Enable “Internal” to show hidden and net VMs."
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wlcontrol_core::model::{VmFeatures, WlState};

    fn vm(name: &str, env: Option<&str>, state: RuntimeState) -> Vm {
        Vm {
            name: name.to_owned(),
            env: env.map(str::to_owned),
            state,
            is_net_vm: false,
            hidden: false,
            pending_restart: false,
            features: VmFeatures::default(),
            static_ip: None,
            readiness: vec![],
            usb: vec![],
        }
    }

    #[test]
    fn groups_visible_vms_by_env_and_hides_internal_by_default() {
        let mut hidden = vm("hidden", Some("work"), RuntimeState::Stopped);
        hidden.hidden = true;
        let mut net = vm("sys-work-net", Some("work"), RuntimeState::Running);
        net.is_net_vm = true;
        let state = WlState {
            connectivity: Connectivity::Connected,
            role: AuthRole::Admin,
            vms: vec![
                vm("work-a", Some("work"), RuntimeState::Running),
                vm("personal-a", Some("personal"), RuntimeState::Stopped),
                hidden,
                net,
            ],
            stale: false,
            note: None,
        };

        let groups = visible_vm_groups(&state, false);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].env, "work");
        assert_eq!(groups[0].vms.len(), 1);
        assert_eq!(groups[1].env, "personal");

        let groups = visible_vm_groups(&state, true);
        assert_eq!(groups[0].vms.len(), 3);
    }

    #[test]
    fn maps_unavailable_to_human_tooltips() {
        assert_eq!(
            unavailable_tooltip(&Unavailable::InsufficientRole {
                required: AuthRole::Admin
            }),
            "requires admin"
        );
        assert_eq!(
            unavailable_tooltip(&Unavailable::UsbOwnedElsewhere {
                owner: "corp-vm".to_owned()
            }),
            "USB owned by corp-vm"
        );
        assert_eq!(
            unavailable_tooltip(&Unavailable::NotYetImplemented),
            "unsupported by the current nixling audio control plane"
        );
    }

    #[test]
    fn badge_labels_cover_runtime_states() {
        assert_eq!(state_badge(RuntimeState::Running).label, "Running");
        assert_eq!(
            state_badge(RuntimeState::Stopped).css_class,
            "state-stopped"
        );
        assert_eq!(state_badge(RuntimeState::Starting).label, "Starting");
        assert_eq!(state_badge(RuntimeState::Stopping).label, "Stopping");
        assert_eq!(
            state_badge(RuntimeState::Unknown).css_class,
            "state-unknown"
        );
    }

    #[test]
    fn only_destructive_running_vm_actions_need_confirmation() {
        let running = vm("corp-vm", Some("work"), RuntimeState::Running);
        let stopped = vm("corp-vm", Some("work"), RuntimeState::Stopped);

        assert!(needs_confirmation(
            &ActionKind::Stop {
                vm: "corp-vm".to_owned()
            },
            &running
        ));
        assert!(needs_confirmation(
            &ActionKind::Switch {
                vm: "corp-vm".to_owned()
            },
            &running
        ));
        assert!(!needs_confirmation(
            &ActionKind::Stop {
                vm: "corp-vm".to_owned()
            },
            &stopped
        ));
        assert!(!needs_confirmation(
            &ActionKind::Start {
                vm: "corp-vm".to_owned()
            },
            &running
        ));
    }

    #[test]
    fn vm_subtitle_includes_ip_readiness_usb_and_pending_restart() {
        let mut target = vm("corp-vm", Some("work"), RuntimeState::Running);
        target.static_ip = Some("10.42.0.12".to_owned());
        target.readiness = vec!["api-ready".to_owned()];
        target.pending_restart = true;
        target.usb.push(UsbClaim {
            vm: "corp-vm".to_owned(),
            env: "work".to_owned(),
            bus_id: "1-2".to_owned(),
            bound: true,
            owner_vm: Some("corp-vm".to_owned()),
        });

        let subtitle = vm_subtitle(&target, true);
        assert!(subtitle.contains("10.42.0.12"));
        assert!(subtitle.contains("api-ready"));
        assert!(subtitle.contains("USB 1-2 bound to corp-vm"));
        assert!(subtitle.contains("pending restart"));
    }
}
