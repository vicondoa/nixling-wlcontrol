//! State reduction and precedence.
//!
//! Owning wave: **Wave 1 — Core model agent**. Wave 0 ships a correct baseline
//! so downstream surfaces have real data to render; the Wave 1 agent hardens
//! the precedence rules, net-VM detection, and inconsistency → attention
//! mapping per the plan's "State model" section.
//!
//! Precedence contract:
//! 1. `inventory` (`nixling list`) defines the declared VM set, env, features,
//!    static IP, and default order.
//! 2. `statuses` (`nixling status <vm>`) override runtime state, readiness, and
//!    pending-restart.
//! 3. `usb` (`nixling usb probe`) attaches USB claims.
//! 4. `auth` (`nixling auth status`) sets the effective role.
//! 5. Missing/inconsistent inputs reduce to `Unknown`, never false-healthy.

use std::collections::HashSet;

use crate::config::Config;
use crate::model::{AuthRole, Connectivity, RuntimeState, UsbClaim, Vm, WlState};
use crate::sources::{InventoryVm, ReduceInput, VmStatus};

/// Reduce a bundle of source fragments into the aggregate [`WlState`].
pub fn reduce(input: ReduceInput) -> WlState {
    reduce_with_config(input, &Config::default())
}

/// Reduce source fragments with user configuration for ordering and visibility.
pub fn reduce_with_config(input: ReduceInput, config: &Config) -> WlState {
    let ReduceInput {
        connectivity,
        auth,
        inventory,
        statuses,
        usb,
    } = input;

    if connectivity != Connectivity::Connected {
        return WlState {
            connectivity,
            role: AuthRole::None,
            vms: Vec::new(),
            stale: false,
            note: None,
        };
    }

    let role = auth.map(|a| a.role).unwrap_or(AuthRole::None);
    let connectivity = if role == AuthRole::None {
        Connectivity::AuthDenied
    } else {
        Connectivity::Connected
    };

    let inventory = inventory.unwrap_or_default();
    let usb_claims = usb.map(|u| u.claims).unwrap_or_default();
    let hidden_names = config
        .hidden_vms
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();

    let vms = ordered_inventory(inventory.vms, &config.favorites)
        .into_iter()
        .map(|inv| build_vm(inv, &statuses, &usb_claims, &hidden_names))
        .collect();

    WlState {
        connectivity,
        role,
        vms,
        stale: false,
        note: None,
    }
}

fn build_vm(
    inv: InventoryVm,
    statuses: &[VmStatus],
    usb_claims: &[UsbClaim],
    hidden_names: &HashSet<&str>,
) -> Vm {
    let status = statuses.iter().find(|s| s.name == inv.name);
    let is_net_vm = inv.is_net_vm || is_framework_net_vm_name(&inv.name);
    let hidden = hidden_names.contains(inv.name.as_str());
    let coarse = coarse_state(inv.coarse_status.as_deref());

    let state = reduce_state(coarse, status.map(|s| s.state));

    let usb = usb_claims
        .iter()
        .filter(|c| c.vm == inv.name)
        .cloned()
        .collect();

    Vm {
        name: inv.name,
        env: inv.env,
        state,
        is_net_vm,
        hidden,
        pending_restart: status.map(|s| s.pending_restart).unwrap_or(false),
        features: inv.features,
        static_ip: inv.static_ip,
        readiness: status.map(|s| s.readiness.clone()).unwrap_or_default(),
        usb,
    }
}

fn ordered_inventory(vms: Vec<InventoryVm>, favorites: &[String]) -> Vec<InventoryVm> {
    let mut remaining = vms.into_iter().map(Some).collect::<Vec<_>>();
    let mut ordered = Vec::with_capacity(remaining.len());

    for favorite in favorites {
        if let Some((idx, _)) = remaining.iter().enumerate().find(|(_, vm)| {
            vm.as_ref()
                .is_some_and(|candidate| candidate.name == favorite.as_str())
        }) {
            if let Some(vm) = remaining[idx].take() {
                ordered.push(vm);
            }
        }
    }

    ordered.extend(remaining.into_iter().flatten());
    ordered
}

fn reduce_state(coarse: RuntimeState, status: Option<RuntimeState>) -> RuntimeState {
    match status {
        Some(status_state) if state_conflicts(coarse, status_state) => RuntimeState::Unknown,
        Some(status_state) => status_state,
        None if coarse == RuntimeState::Running => RuntimeState::Unknown,
        None => coarse,
    }
}

fn state_conflicts(coarse: RuntimeState, status: RuntimeState) -> bool {
    match coarse {
        RuntimeState::Running => status == RuntimeState::Stopped,
        RuntimeState::Stopped => matches!(
            status,
            RuntimeState::Running | RuntimeState::Starting | RuntimeState::Stopping
        ),
        RuntimeState::Starting | RuntimeState::Stopping | RuntimeState::Unknown => false,
    }
}

fn coarse_state(s: Option<&str>) -> RuntimeState {
    match s {
        Some(v) if v.starts_with("running") => RuntimeState::Running,
        Some(v) if v.starts_with("stopped") => RuntimeState::Stopped,
        Some(_) => RuntimeState::Unknown,
        None => RuntimeState::Unknown,
    }
}

fn is_framework_net_vm_name(name: &str) -> bool {
    name.strip_prefix("sys-")
        .and_then(|rest| rest.strip_suffix("-net"))
        .is_some_and(|env| !env.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::{Auth, Inventory, UsbProbe};

    fn inventory_vm(name: &str, coarse_status: Option<&str>) -> InventoryVm {
        InventoryVm {
            name: name.into(),
            env: Some("work".into()),
            is_net_vm: false,
            features: Default::default(),
            static_ip: None,
            coarse_status: coarse_status.map(String::from),
        }
    }

    #[test]
    fn daemon_down_yields_empty_state() {
        let input = ReduceInput {
            connectivity: Connectivity::DaemonDown,
            ..Default::default()
        };
        let state = reduce(input);
        assert_eq!(state.connectivity, Connectivity::DaemonDown);
        assert!(state.vms.is_empty());
        assert_eq!(state.role, AuthRole::None);
    }

    #[test]
    fn per_vm_status_overrides_coarse() {
        let input = ReduceInput {
            connectivity: Connectivity::Connected,
            auth: Some(Auth {
                role: AuthRole::Admin,
            }),
            inventory: Some(Inventory {
                vms: vec![inventory_vm("corp-vm", Some("running"))],
            }),
            statuses: vec![VmStatus {
                name: "corp-vm".into(),
                state: RuntimeState::Running,
                pending_restart: true,
                readiness: vec!["api-ready".into()],
            }],
            usb: Some(UsbProbe::default()),
        };
        let state = reduce(input);
        assert_eq!(state.vms.len(), 1);
        assert_eq!(state.vms[0].state, RuntimeState::Running);
        assert!(state.vms[0].pending_restart);
        assert_eq!(state.role, AuthRole::Admin);
    }

    #[test]
    fn no_role_maps_to_auth_denied() {
        let input = ReduceInput {
            connectivity: Connectivity::Connected,
            auth: Some(Auth {
                role: AuthRole::None,
            }),
            inventory: Some(Inventory::default()),
            ..Default::default()
        };
        let state = reduce(input);
        assert_eq!(state.connectivity, Connectivity::AuthDenied);
    }

    #[test]
    fn sys_dash_env_dash_net_name_is_net_vm_fallback() {
        assert!(is_framework_net_vm_name("sys-work-net"));
        assert!(!is_framework_net_vm_name("sys-net"));
        assert!(!is_framework_net_vm_name("sys--net"));
        assert!(!is_framework_net_vm_name("corp-vm"));

        let input = ReduceInput {
            connectivity: Connectivity::Connected,
            auth: Some(Auth {
                role: AuthRole::Admin,
            }),
            inventory: Some(Inventory {
                vms: vec![inventory_vm("sys-work-net", Some("stopped"))],
            }),
            ..Default::default()
        };
        let state = reduce(input);
        assert!(state.vms[0].is_net_vm);
    }

    #[test]
    fn favorites_reorder_and_hidden_vms_are_marked() {
        let input = ReduceInput {
            connectivity: Connectivity::Connected,
            auth: Some(Auth {
                role: AuthRole::Admin,
            }),
            inventory: Some(Inventory {
                vms: vec![
                    inventory_vm("alpha", Some("stopped")),
                    inventory_vm("bravo", Some("stopped")),
                    inventory_vm("charlie", Some("stopped")),
                ],
            }),
            ..Default::default()
        };
        let config = Config {
            favorites: vec!["charlie".into(), "missing".into(), "alpha".into()],
            hidden_vms: vec!["bravo".into()],
            ..Default::default()
        };

        let state = reduce_with_config(input, &config);

        assert_eq!(
            state
                .vms
                .iter()
                .map(|vm| vm.name.as_str())
                .collect::<Vec<_>>(),
            ["charlie", "alpha", "bravo"]
        );
        assert!(!state.vms[0].hidden);
        assert!(!state.vms[1].hidden);
        assert!(state.vms[2].hidden);
    }

    #[test]
    fn missing_status_for_running_inventory_becomes_unknown_attention() {
        let input = ReduceInput {
            connectivity: Connectivity::Connected,
            auth: Some(Auth {
                role: AuthRole::Admin,
            }),
            inventory: Some(Inventory {
                vms: vec![inventory_vm("corp-vm", Some("running"))],
            }),
            ..Default::default()
        };
        let state = reduce(input);
        assert_eq!(state.vms[0].state, RuntimeState::Unknown);
        assert!(state.needs_attention());
    }

    #[test]
    fn conflicting_inventory_and_status_becomes_unknown_attention() {
        let input = ReduceInput {
            connectivity: Connectivity::Connected,
            auth: Some(Auth {
                role: AuthRole::Admin,
            }),
            inventory: Some(Inventory {
                vms: vec![inventory_vm("corp-vm", Some("stopped"))],
            }),
            statuses: vec![VmStatus {
                name: "corp-vm".into(),
                state: RuntimeState::Running,
                pending_restart: false,
                readiness: vec![],
            }],
            ..Default::default()
        };
        let state = reduce(input);
        assert_eq!(state.vms[0].state, RuntimeState::Unknown);
        assert!(state.needs_attention());
    }
}
