//! Waybar custom-module rendering.
//!
//! Owning wave: **Wave 2 — Waybar module agent**. Wave 0 ships a working
//! baseline that produces a valid single-object Waybar JSON line with stable
//! CSS classes so the bar shows real state immediately. The Wave 2 agent
//! refines the compact/detail formats, tooltip richness, and display-mode
//! cycling.
//!
//! Waybar custom-module contract (`return-type = "json"`): one newline-
//! terminated JSON object per update with `text`, `class`, and `tooltip`.

use serde::Serialize;
use wlcontrol_core::model::{Connectivity, RuntimeState, WlState};

/// A single Waybar JSON line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaybarLine {
    pub text: String,
    /// CSS classes (Waybar accepts an array of classes).
    pub class: Vec<String>,
    pub tooltip: String,
}

impl WaybarLine {
    /// Serialize to a single newline-terminated JSON line, as Waybar expects.
    pub fn to_json_line(&self) -> String {
        // Serialization of a flat struct of strings cannot fail.
        let mut s = serde_json::to_string(self).unwrap_or_else(|_| {
            "{\"text\":\"\",\"class\":[\"error\"],\"tooltip\":\"render error\"}".to_owned()
        });
        s.push('\n');
        s
    }
}

/// Render the compact Waybar line for a reduced state.
pub fn render(state: &WlState) -> WaybarLine {
    match state.connectivity {
        Connectivity::DaemonDown => WaybarLine {
            text: "◆ down".to_owned(),
            class: vec!["daemon-down".to_owned()],
            tooltip: "nixlingd is unreachable".to_owned(),
        },
        Connectivity::AuthDenied => WaybarLine {
            text: "◆ auth".to_owned(),
            class: vec!["auth-denied".to_owned()],
            tooltip: "Not authorized on the nixling public socket".to_owned(),
        },
        Connectivity::Connected => render_connected(state),
    }
}

fn render_connected(state: &WlState) -> WaybarLine {
    let running = state.running_count();
    let total = state.visible_count();
    let attention = state.needs_attention();

    let mut text = format!("◆ {running}/{total}");
    if attention {
        text.push_str(" !");
    }

    let mut class = vec![match (running, total) {
        (0, _) => "all-stopped".to_owned(),
        (r, t) if r == t => "all-running".to_owned(),
        _ => "partial-running".to_owned(),
    }];
    if attention {
        class.push("attention".to_owned());
    }
    if state.stale {
        class.push("stale".to_owned());
    }

    WaybarLine {
        text,
        class,
        tooltip: render_tooltip(state),
    }
}

fn render_tooltip(state: &WlState) -> String {
    let mut lines = Vec::new();
    for vm in state.vms.iter().filter(|v| !v.is_net_vm) {
        let glyph = match vm.state {
            RuntimeState::Running => "●",
            RuntimeState::Starting | RuntimeState::Stopping => "◐",
            RuntimeState::Stopped => "○",
            RuntimeState::Unknown => "?",
        };
        let env = vm.env.as_deref().unwrap_or("-");
        let mut line = format!("{glyph} {} [{env}]", vm.name);
        if vm.pending_restart {
            line.push_str(" (pending restart)");
        }
        lines.push(line);
    }
    if lines.is_empty() {
        "No VMs declared".to_owned()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wlcontrol_core::model::{Vm, VmFeatures};

    fn vm(name: &str, state: RuntimeState, net: bool) -> Vm {
        Vm {
            name: name.into(),
            env: Some("work".into()),
            state,
            is_net_vm: net,
            hidden: false,
            pending_restart: false,
            features: VmFeatures::default(),
            static_ip: None,
            readiness: vec![],
            usb: vec![],
        }
    }

    #[test]
    fn daemon_down_line() {
        let state = WlState {
            connectivity: Connectivity::DaemonDown,
            ..Default::default()
        };
        let line = render(&state);
        assert!(line.class.contains(&"daemon-down".to_owned()));
        assert!(line.to_json_line().ends_with("\n"));
    }

    #[test]
    fn partial_running_excludes_net_vm() {
        let state = WlState {
            connectivity: Connectivity::Connected,
            role: wlcontrol_core::model::AuthRole::Admin,
            vms: vec![
                vm("a", RuntimeState::Running, false),
                vm("b", RuntimeState::Stopped, false),
                vm("sys-work-net", RuntimeState::Running, true),
            ],
            stale: false,
            note: None,
        };
        let line = render(&state);
        assert_eq!(line.text, "◆ 1/2");
        assert!(line.class.contains(&"partial-running".to_owned()));
    }

    #[test]
    fn json_line_is_single_line() {
        let line = render(&WlState::default());
        let json = line.to_json_line();
        assert_eq!(json.matches('\n').count(), 1);
        assert!(json.ends_with('\n'));
    }
}
