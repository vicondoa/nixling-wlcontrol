//! Frozen cross-crate domain contract for d2b-wlcontrol.
//!
//! These types are the **stable internal contract** that every other crate
//! builds against:
//!
//! - `wlcontrol-d2b` produces [`WlState`] / [`Vm`] / [`UsbClaim`] from the
//!   d2bd public socket.
//! - `wlcontrol-waybar` and `wlcontrol-ui` render [`WlState`].
//! - `wlcontrol-cli` dispatches [`PlannedAction`].
//!
//! Owning wave: Wave 0 (integrator). Downstream fleet agents may **extend**
//! these types (add fields with `#[serde(default)]`, add enum variants at the
//! end) but must not break the published field/variant names that other crates
//! already consume. Breaking changes go through an integrator prep commit.

use serde::{Deserialize, Serialize};

/// Effective operator authorization, mirrored from `d2b auth status`.
///
/// This gates which controls the UI may enable. `Admin` is required for
/// guest-control exec (terminal launch); lifecycle/USB verbs require at least
/// the launcher role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AuthRole {
    /// No recognized role; the public socket is unreachable or denied.
    #[default]
    None,
    /// Recognized launcher. Current d2bd keeps destructive lifecycle/USB
    /// verbs admin-only; wlcontrol uses this role for non-destructive build.
    Launcher,
    /// Full admin: launcher plus guest-control exec.
    Admin,
}

/// Normalized runtime state for a single VM.
///
/// This is a *reduced* state derived from `d2b list` + `d2b status`,
/// never a raw passthrough of either. Inconsistent or unreadable inputs reduce
/// to [`RuntimeState::Unknown`] (never to a false-healthy `Running`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeState {
    /// Process alive and (where applicable) api-ready.
    Running,
    /// Process alive but readiness not yet confirmed.
    Starting,
    /// Stop in progress.
    Stopping,
    /// Declared but not running.
    Stopped,
    /// State could not be determined.
    #[default]
    Unknown,
}

/// A USBIP busid claim, mirrored from `d2b usb probe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsbClaim {
    /// VM the claim is declared for.
    pub vm: String,
    /// Environment the claim belongs to.
    pub env: String,
    /// Host USB busid in canonical `B-P[.P...]` form.
    pub bus_id: String,
    /// Whether the device is currently bound.
    pub bound: bool,
    /// The VM currently holding the device, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_vm: Option<String>,
}

/// Per-VM feature toggles surfaced for display and control gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VmFeatures {
    pub graphics: bool,
    pub tpm: bool,
    pub usbip: bool,
    /// True when the VM declares `audio.enable`. Controls are enabled only
    /// after d2b also reports live audio status for the VM.
    pub audio: bool,
}

/// Runtime operations the connected d2b daemon says this VM supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmCapabilities {
    pub start: bool,
    pub stop: bool,
    pub restart: bool,
    pub switch: bool,
    pub build: bool,
    pub boot: bool,
    pub usb_hotplug: bool,
    pub store_verify: bool,
    pub terminal: bool,
}

impl Default for VmCapabilities {
    fn default() -> Self {
        Self {
            start: true,
            stop: true,
            restart: true,
            switch: true,
            build: true,
            boot: true,
            usb_hotplug: true,
            store_verify: true,
            terminal: true,
        }
    }
}

/// The audio channel represented by a d2b audio operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioChannel {
    Speaker,
    Microphone,
}

/// Provider backing a VM's audio controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioProviderKind {
    LocalHypervisor,
    QemuMedia,
    AcaSandbox,
    Unknown,
}

/// Enforcement posture reported by d2b for a VM's audio controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AudioEnforcementPosture {
    HostAndGuest,
    HostOnly,
    GuestOnly,
    Unsupported,
    Unknown,
}

/// Per-channel audio state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioChannelState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u16>,
    pub muted: bool,
}

/// Per-VM audio state and provider posture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmAudioState {
    pub speaker: AudioChannelState,
    pub microphone: AudioChannelState,
    pub provider_kind: AudioProviderKind,
    pub enforcement: AudioEnforcementPosture,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

/// A custom per-VM quick-launch icon surfaced by the popup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickLaunchIcon {
    pub id: String,
    pub icon: String,
    pub tooltip: String,
}

/// A normalized VM as presented to the UI surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Vm {
    /// VM name as declared in `d2b.vms.<name>`.
    pub name: String,
    /// Environment name, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// Reduced runtime state.
    pub state: RuntimeState,
    /// True for framework-declared net VMs (`sys-*-net`); hidden by default.
    #[serde(default)]
    pub is_net_vm: bool,
    /// True when user config hides this VM from compact surfaces.
    #[serde(default)]
    pub hidden: bool,
    /// True when the running closure differs from the declared closure.
    #[serde(default)]
    pub pending_restart: bool,
    /// Declared feature toggles.
    #[serde(default)]
    pub features: VmFeatures,
    /// Runtime operation support reported by d2b.
    #[serde(default)]
    pub capabilities: VmCapabilities,
    /// Static IP, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub static_ip: Option<String>,
    /// Free-form readiness/role hints for the detail view.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readiness: Vec<String>,
    /// USB claims associated with this VM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usb: Vec<UsbClaim>,
    /// Audio status and controls, when d2b reports them for this VM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<VmAudioState>,
    /// Configured custom quick-launch icons for this VM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quick_launch: Vec<QuickLaunchIcon>,
}

/// Connectivity / authorization posture for the whole control surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Connectivity {
    /// Public socket reachable and a role was resolved.
    Connected,
    /// Public socket reachable but no role (controls are read-only/denied).
    AuthDenied,
    /// `d2bd` is unreachable.
    #[default]
    DaemonDown,
}

/// The aggregate, reduced control-surface state. This is what every UI surface
/// renders and what `d2b-wlcontrol status-json` emits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct WlState {
    /// Connectivity / auth posture.
    pub connectivity: Connectivity,
    /// Effective operator role.
    pub role: AuthRole,
    /// All known VMs (including net VMs and hidden ones); renderers use
    /// `is_net_vm` / `hidden` to choose compact vs. detail surfaces.
    pub vms: Vec<Vm>,
    /// True when this state was served from cache after a failed refresh.
    #[serde(default)]
    pub stale: bool,
    /// Optional human-facing note (e.g. last error remediation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl WlState {
    /// Count of running VMs, excluding net VMs.
    pub fn running_count(&self) -> usize {
        self.vms
            .iter()
            .filter(|v| !v.is_net_vm && !v.hidden && v.state == RuntimeState::Running)
            .count()
    }

    /// Count of visible (non-net, non-hidden) VMs.
    pub fn visible_count(&self) -> usize {
        self.vms
            .iter()
            .filter(|v| !v.is_net_vm && !v.hidden)
            .count()
    }

    /// True when any visible VM needs operator attention (pending restart or
    /// an unknown/inconsistent state while the daemon is reachable).
    pub fn needs_attention(&self) -> bool {
        if self.connectivity != Connectivity::Connected {
            return false;
        }
        self.vms
            .iter()
            .filter(|v| !v.is_net_vm && !v.hidden)
            .any(|v| {
                v.pending_restart
                    || v.state == RuntimeState::Unknown
                    || v.audio
                        .as_ref()
                        .is_some_and(|audio| audio.error_kind.is_some() || !audio.microphone.muted)
            })
    }
}

/// The set of operations the control surface can request. Each maps to a
/// d2b public-socket request or an argv-only host process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum ActionKind {
    /// Refresh the reduced state.
    Refresh,
    /// Start a VM (`--apply`).
    Start { vm: String },
    /// Stop a VM (`--apply`).
    Stop { vm: String },
    /// Force stop a VM, bypassing graceful guest shutdown.
    ForceStop { vm: String },
    /// Restart a VM (`--apply`).
    Restart { vm: String },
    /// Activate the VM's current closure (`switch --apply`).
    Switch { vm: String },
    /// Build/evaluate the per-VM toplevel without activation.
    Build { vm: String },
    /// Stage the current per-VM closure for next boot (`boot --apply`).
    Boot { vm: String },
    /// Bind a USB busid to a VM (`usb attach --apply`).
    UsbAttach { vm: String, bus_id: String },
    /// Unbind a USB busid from a VM (`usb detach --apply`).
    UsbDetach { vm: String, bus_id: String },
    /// Verify the per-VM store live pool.
    StoreVerify { vm: String },
    /// Launch a guest terminal with detached guest-control exec.
    LaunchTerminal { vm: String },
    /// Run a configured custom guest quick-launch command.
    QuickLaunch { vm: String, id: String },
    /// Toggle microphone forwarding for a VM.
    AudioMic { vm: String, on: bool },
    /// Toggle speaker forwarding for a VM.
    AudioSpeaker { vm: String, on: bool },
    /// Set speaker playback volume for a VM.
    AudioSpeakerVolume { vm: String, level_percent: u8 },
    /// Set microphone input gain for a VM.
    AudioMicGain { vm: String, level_percent: u8 },
    /// Disable all audio forwarding for a VM.
    AudioOff { vm: String },
    /// Open / focus the Quickshell control center.
    OpenControlCenter,
    /// Open the configured observability portal in a browser.
    OpenObservability,
    /// Cycle the Waybar compact/detail display mode.
    CycleDisplay,
}

/// Why an action is or is not currently available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "reason")]
pub enum Unavailable {
    /// `d2bd` is unreachable.
    DaemonDown,
    /// Caller role is insufficient for this action.
    InsufficientRole { required: AuthRole },
    /// The target VM is not in a state that allows the action.
    VmState { detail: String },
    /// USB device is owned by another VM.
    UsbOwnedElsewhere { owner: String },
    /// Backed by a d2b surface that is not yet implemented.
    NotYetImplemented,
    /// Generic block with a human-facing detail.
    Blocked { detail: String },
}

/// An action paired with whether it can currently be invoked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionAvailability {
    pub action: ActionKind,
    /// `None` means available; `Some(_)` carries the block reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<Unavailable>,
}

impl ActionAvailability {
    pub fn available(action: ActionKind) -> Self {
        Self {
            action,
            unavailable: None,
        }
    }

    pub fn blocked(action: ActionKind, reason: Unavailable) -> Self {
        Self {
            action,
            unavailable: Some(reason),
        }
    }

    pub fn is_available(&self) -> bool {
        self.unavailable.is_none()
    }
}

/// A fully-resolved, ready-to-dispatch action.
///
/// The planner emits exactly one of these. A [`PlannedAction::Process`] is an
/// **argv vector**, never a shell string — there is no shell interpolation
/// anywhere in the control surface. `wait` tells the CLI whether to wait for
/// short-lived commands (build, detached exec creation) or just launch and
/// return (browser open).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "dispatch")]
pub enum PlannedAction {
    /// A d2b public-socket intent the protocol client should execute.
    Socket { intent: SocketIntent },
    /// A host process, expressed as an argv vector.
    Process {
        argv: Vec<String>,
        #[serde(default)]
        wait: bool,
    },
}

/// A typed d2b public-socket intent. The protocol client maps each variant
/// onto the corresponding `PublicRequest`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "intent")]
pub enum SocketIntent {
    List,
    Status {
        vm: String,
    },
    AuthStatus,
    UsbProbe,
    VmStart {
        vm: String,
    },
    VmStop {
        vm: String,
        #[serde(default, skip_serializing_if = "is_false")]
        force: bool,
    },
    VmRestart {
        vm: String,
    },
    Switch {
        vm: String,
    },
    Boot {
        vm: String,
    },
    UsbAttach {
        vm: String,
        bus_id: String,
    },
    UsbDetach {
        vm: String,
        bus_id: String,
    },
    StoreVerify {
        vm: String,
    },
    AudioMute {
        vm: String,
        channel: AudioChannel,
        mute: bool,
    },
    AudioSetVolume {
        vm: String,
        channel: AudioChannel,
        level_percent: u8,
    },
    AudioOff {
        vm: String,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_and_visible_counts_exclude_net_vms() {
        let state = WlState {
            connectivity: Connectivity::Connected,
            role: AuthRole::Admin,
            vms: vec![
                Vm {
                    name: "corp-vm".into(),
                    env: Some("work".into()),
                    state: RuntimeState::Running,
                    is_net_vm: false,
                    hidden: false,
                    pending_restart: false,
                    features: VmFeatures::default(),
                    capabilities: VmCapabilities::default(),
                    static_ip: None,
                    readiness: vec![],
                    usb: vec![],
                    audio: None,
                    quick_launch: vec![],
                },
                Vm {
                    name: "sys-work-net".into(),
                    env: Some("work".into()),
                    state: RuntimeState::Running,
                    is_net_vm: true,
                    hidden: false,
                    pending_restart: false,
                    features: VmFeatures::default(),
                    capabilities: VmCapabilities::default(),
                    static_ip: None,
                    readiness: vec![],
                    usb: vec![],
                    audio: None,
                    quick_launch: vec![],
                },
            ],
            stale: false,
            note: None,
        };
        assert_eq!(state.running_count(), 1);
        assert_eq!(state.visible_count(), 1);
        assert!(!state.needs_attention());
    }

    #[test]
    fn attention_triggers_on_pending_restart() {
        let mut state = WlState {
            connectivity: Connectivity::Connected,
            ..Default::default()
        };
        state.vms.push(Vm {
            name: "corp-vm".into(),
            env: None,
            state: RuntimeState::Running,
            is_net_vm: false,
            hidden: false,
            pending_restart: true,
            features: VmFeatures::default(),
            capabilities: VmCapabilities::default(),
            static_ip: None,
            readiness: vec![],
            usb: vec![],
            audio: None,
            quick_launch: vec![],
        });
        assert!(state.needs_attention());
    }

    #[test]
    fn attention_triggers_on_active_microphone() {
        let mut state = WlState {
            connectivity: Connectivity::Connected,
            ..Default::default()
        };
        state.vms.push(Vm {
            name: "corp-vm".into(),
            audio: Some(VmAudioState {
                speaker: AudioChannelState {
                    level: Some(80),
                    muted: false,
                },
                microphone: AudioChannelState {
                    level: Some(50),
                    muted: false,
                },
                provider_kind: AudioProviderKind::LocalHypervisor,
                enforcement: AudioEnforcementPosture::HostAndGuest,
                error_kind: None,
                remediation: None,
            }),
            ..Default::default()
        });
        assert!(state.needs_attention());
    }

    #[test]
    fn counts_and_attention_exclude_hidden_vms() {
        let state = WlState {
            connectivity: Connectivity::Connected,
            role: AuthRole::Admin,
            vms: vec![Vm {
                name: "noisy-vm".into(),
                env: None,
                state: RuntimeState::Unknown,
                is_net_vm: false,
                hidden: true,
                pending_restart: true,
                features: VmFeatures::default(),
                capabilities: VmCapabilities::default(),
                static_ip: None,
                readiness: vec![],
                usb: vec![],
                audio: None,
                quick_launch: vec![],
            }],
            stale: false,
            note: None,
        };
        assert_eq!(state.running_count(), 0);
        assert_eq!(state.visible_count(), 0);
        assert!(!state.needs_attention());
    }

    #[test]
    fn wlstate_round_trips_through_json() {
        let mut state = WlState::default();
        state.vms.push(Vm {
            name: "corp-vm".into(),
            hidden: true,
            ..Default::default()
        });
        let json = serde_json::to_string(&state).expect("serialize");
        let back: WlState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(state, back);
    }

    #[test]
    fn vm_hidden_defaults_when_absent_from_json() {
        let vm: Vm = serde_json::from_str(
            r#"{
                "name": "corp-vm",
                "state": "running"
            }"#,
        )
        .expect("deserialize vm");
        assert!(!vm.hidden);
    }

    #[test]
    fn audio_action_variants_round_trip_through_json() {
        let actions = [
            ActionKind::Build {
                vm: "corp-vm".into(),
            },
            ActionKind::Boot {
                vm: "corp-vm".into(),
            },
            ActionKind::ForceStop {
                vm: "corp-vm".into(),
            },
            ActionKind::AudioMic {
                vm: "corp-vm".into(),
                on: true,
            },
            ActionKind::AudioSpeaker {
                vm: "corp-vm".into(),
                on: false,
            },
            ActionKind::AudioSpeakerVolume {
                vm: "corp-vm".into(),
                level_percent: 70,
            },
            ActionKind::AudioMicGain {
                vm: "corp-vm".into(),
                level_percent: 40,
            },
            ActionKind::AudioOff {
                vm: "corp-vm".into(),
            },
        ];

        for action in actions {
            let json = serde_json::to_string(&action).expect("serialize action");
            let back: ActionKind = serde_json::from_str(&json).expect("deserialize action");
            assert_eq!(action, back);
        }

        let json = serde_json::to_string(&ActionKind::OpenObservability).expect("serialize action");
        let back: ActionKind = serde_json::from_str(&json).expect("deserialize action");
        assert_eq!(back, ActionKind::OpenObservability);
    }

    #[test]
    fn vm_stop_socket_intent_defaults_force_false_for_compatibility() {
        let intent: SocketIntent =
            serde_json::from_str(r#"{"intent":"vm-stop","vm":"corp-vm"}"#).expect("deserialize");
        assert_eq!(
            intent,
            SocketIntent::VmStop {
                vm: "corp-vm".into(),
                force: false
            }
        );

        let normal_json = serde_json::to_string(&intent).expect("serialize");
        assert!(!normal_json.contains("force"));

        let force_json = serde_json::to_string(&SocketIntent::VmStop {
            vm: "corp-vm".into(),
            force: true,
        })
        .expect("serialize");
        assert!(force_json.contains(r#""force":true"#));
    }
}
