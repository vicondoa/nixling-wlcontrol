//! Normalized per-call source fragments.
//!
//! These are the **output contract of the protocol client** (`wlcontrol-d2b`)
//! and the **input contract of the reducer** ([`crate::reduce`]). Keeping the
//! dependency direction one-way (`wlcontrol-d2b` → `wlcontrol-core`) means
//! the reducer never needs to know about d2b wire types: the protocol
//! client translates raw d2b JSON into these neutral fragments.
//!
//! Owning wave: Wave 0 (integrator). Wave 1 agents extend as needed.

use serde::{Deserialize, Serialize};

use crate::model::{
    AuthRole, Connectivity, RuntimeState, UsbClaim, VmAudioState, VmCapabilities, VmFeatures,
};

/// One declared VM as reported by `d2b list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryVm {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default)]
    pub is_net_vm: bool,
    #[serde(default)]
    pub features: VmFeatures,
    #[serde(default)]
    pub capabilities: VmCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_ip: Option<String>,
    /// Coarse status string from `list` (`stopped`, `running`, ...). The
    /// reducer treats per-VM `status` as more authoritative when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coarse_status: Option<String>,
}

/// The declared inventory from `d2b list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub vms: Vec<InventoryVm>,
}

/// Per-VM runtime truth from `d2b status <vm>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmStatus {
    pub name: String,
    pub state: RuntimeState,
    #[serde(default)]
    pub pending_restart: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness: Vec<String>,
    #[serde(default)]
    pub capabilities: VmCapabilities,
}

/// USB claims from `d2b usb probe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsbProbe {
    pub claims: Vec<UsbClaim>,
}

/// Audio status from `d2b audio status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatus {
    pub entries: Vec<AudioStatusEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<AudioStatusError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatusEntry {
    pub vm: String,
    pub audio: VmAudioState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStatusError {
    pub vm: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// Authorization posture from `d2b auth status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Auth {
    pub role: AuthRole,
}

/// The full bundle of fragments the reducer consumes for one refresh cycle.
///
/// Any field may be `None` when the corresponding call failed; the reducer is
/// responsible for degrading gracefully (e.g. marking state stale/unknown).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReduceInput {
    /// Overall connectivity as observed by the protocol client.
    pub connectivity: Connectivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<Auth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Inventory>,
    /// Per-VM statuses keyed by VM name order; the reducer matches by `name`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<VmStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usb: Option<UsbProbe>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioStatus>,
}
