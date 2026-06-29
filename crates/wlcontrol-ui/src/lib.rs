//! Quickshell/QML layer-shell frontend launcher.
//!
//! `d2b-wlcontrol` is a Waybar-adjacent desktop shell widget, not a
//! document-style application. The visible frontend is therefore a Quickshell
//! layer-shell popup with neutral shell styling and d2b-owned accent colors,
//! while Rust remains the backend (`status-json` + `action …`) and the safe
//! process launcher.

use std::{
    env, fs,
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use wlcontrol_core::{Config, WlError, WlResult};

#[cfg(test)]
mod view_model;

const QML_FILE: &str = "shell.qml";
const PID_FILE: &str = "quickshell.pid";
const SIGTERM: i32 = 15;

unsafe extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    start_time_ticks: u64,
}

/// Open or hide the Quickshell control popup.
///
/// A running frontend is hidden by terminating its Quickshell process. The next
/// invocation starts a fresh instance. This intentionally matches Waybar popup
/// ergonomics: click once to show, click again to hide.
pub fn open(config: &Config) -> WlResult<()> {
    let dir = runtime_dir()?;
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;

    let pid_path = dir.join(PID_FILE);
    if let Some(identity) = read_live_frontend(&pid_path, &dir) {
        // Toggle hide. Quickshell is the direct child we launched, and the pid
        // file lives in a private 0700 runtime dir.
        // SAFETY: pid is validated against /proc start_time and cmdline before
        // signaling. If the process exits after validation, kill returns ESRCH.
        let _ = unsafe { kill(identity.pid as i32, SIGTERM) };
        let _ = fs::remove_file(&pid_path);
        return Ok(());
    }

    let qml_path = materialize_qml(&dir)?;
    let backend = env::current_exe()
        .map_err(|err| WlError::Config(format!("failed to locate backend binary: {err}")))?;
    let mut theme_value = match config.load_ui_colors()? {
        Some(colors) => serde_json::to_value(colors)?,
        None => serde_json::Value::Object(serde_json::Map::new()),
    };
    if let serde_json::Value::Object(ref mut object) = theme_value {
        object.insert("shell".to_owned(), serde_json::to_value(&config.theme)?);
    }
    let theme_json = serde_json::to_string(&theme_value)?;

    let mut child = Command::new("quickshell")
        .arg("--path")
        .arg(&qml_path)
        .arg("--no-duplicate")
        .env("D2B_WLCONTROL_BIN", backend)
        .env("D2B_WLCONTROL_THEME_JSON", theme_json)
        .env(
            "D2B_WLCONTROL_OBSERVABILITY_ENABLED",
            if config.observability.enabled && config.observability.url.is_some() {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "D2B_WLCONTROL_OBSERVABILITY_SUCCESS",
            &config.observability.success_message,
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            WlError::Config(format!(
                "failed to launch quickshell frontend; is quickshell installed/on PATH? {err}"
            ))
        })?;

    let identity = process_identity(child.id())
        .ok_or_else(|| WlError::Config("failed to read quickshell process identity".to_owned()))?;
    write_pid_record(&pid_path, identity)?;

    // Reap asynchronously when the shell exits naturally so the launcher does
    // not leave a zombie. We deliberately do not wait here: `open` is a quick
    // toggle command for Waybar.
    std::thread::spawn(move || {
        let _ = child.wait();
        if read_pid_record(&pid_path).is_some_and(|current| current == identity) {
            let _ = fs::remove_file(&pid_path);
        }
    });

    Ok(())
}

fn runtime_dir() -> WlResult<PathBuf> {
    let base = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    Ok(base.join("d2b-wlcontrol").join("quickshell"))
}

fn materialize_qml(dir: &Path) -> WlResult<PathBuf> {
    let path = dir.join(QML_FILE);
    write_private_file(&path, QML_SOURCE.as_bytes())?;
    Ok(path)
}

fn write_private_file(path: &Path, content: &[u8]) -> WlResult<()> {
    let tmp = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(content)?;
    file.sync_all()?;
    fs::rename(tmp, path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn write_pid_record(path: &Path, identity: ProcessIdentity) -> WlResult<()> {
    write_private_file(
        path,
        format!("{} {}\n", identity.pid, identity.start_time_ticks).as_bytes(),
    )
}

fn read_pid_record(path: &Path) -> Option<ProcessIdentity> {
    let text = fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let pid = parts.next()?.parse::<u32>().ok()?;
    let start_time_ticks = parts.next()?.parse::<u64>().ok()?;
    Some(ProcessIdentity {
        pid,
        start_time_ticks,
    })
}

fn read_live_frontend(path: &Path, runtime_dir: &Path) -> Option<ProcessIdentity> {
    let identity = read_pid_record(path)?;
    if identity.pid == 0 {
        return None;
    }
    let live = process_identity(identity.pid)?;
    if live == identity && cmdline_matches_quickshell(identity.pid, runtime_dir) {
        Some(identity)
    } else {
        let _ = fs::remove_file(path);
        None
    }
}

fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    let stat =
        fs::read_to_string(PathBuf::from("/proc").join(pid.to_string()).join("stat")).ok()?;
    let after_comm = stat.rsplit_once(") ")?.1;
    let start_time_ticks = after_comm.split_whitespace().nth(19)?.parse::<u64>().ok()?;
    Some(ProcessIdentity {
        pid,
        start_time_ticks,
    })
}

fn cmdline_matches_quickshell(pid: u32, runtime_dir: &Path) -> bool {
    let bytes =
        fs::read(PathBuf::from("/proc").join(pid.to_string()).join("cmdline")).unwrap_or_default();
    let args: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter(|part| !part.is_empty())
        .filter_map(|part| std::str::from_utf8(part).ok().map(ToOwned::to_owned))
        .collect();
    if args.is_empty() {
        return false;
    }
    let exe_is_quickshell = args
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .is_some_and(|name| name == "quickshell");
    let qml_path = runtime_dir.join(QML_FILE).display().to_string();
    exe_is_quickshell
        && args
            .windows(2)
            .any(|pair| pair == ["--path", qml_path.as_str()])
        && args.iter().any(|arg| arg == "--no-duplicate")
}

/// Quickshell frontend.
///
/// Notes:
/// - Uses argv-vector `Process` commands; no shell strings.
/// - Neutral shell colors stay local; colored accents come from d2b's UI artifact.
/// - The panel is a draggable layer-shell overlay anchored near the top-right.
const QML_SOURCE: &str = r##"
//@ pragma StateDir $XDG_STATE_HOME/d2b-wlcontrol/quickshell
//@ pragma IconTheme Adwaita

import QtQuick
import Quickshell
import Quickshell.Io

ShellRoot {
  id: root

  property string backend: Quickshell.env("D2B_WLCONTROL_BIN") || "d2b-wlcontrol"
  property var state: ({ connectivity: "daemon-down", role: "none", vms: [] })
  property var usbDevices: []
  property bool busy: false
  property string hoverHint: ""
  property string actionMessage: ""
  property bool actionFailed: false
  property bool actionBusy: false
  property real panelTopMargin: 24
  property real panelRightMargin: 24
  property string confirmKey: ""
  property int normalConfirmMs: 2200
  property int forceConfirmMs: 5200
  property bool observabilityEnabled: Quickshell.env("D2B_WLCONTROL_OBSERVABILITY_ENABLED") === "1"
  property string observabilitySuccess: Quickshell.env("D2B_WLCONTROL_OBSERVABILITY_SUCCESS") || "Opened observability portal"
  property var artifactThemeEnv: root.parseJsonObject(Quickshell.env("D2B_WLCONTROL_THEME_JSON"))

  function visibleVms() {
    const vms = state.vms || []
    return vms
      .map((vm, index) => ({ vm: vm, index: index }))
      .filter(entry => !entry.vm.isNetVm && !entry.vm.hidden)
      .sort((a, b) => {
        const sysDiff = Number((a.vm.name || "").startsWith("sys-")) - Number((b.vm.name || "").startsWith("sys-"))
        return sysDiff !== 0 ? sysDiff : a.index - b.index
      })
      .map(entry => entry.vm)
  }

  function runningCount() {
    return visibleVms().filter(v => v.state === "running").length
  }

  function parseJsonObject(text) {
    if (!text || text.length === 0) return ({})
    try {
      const parsed = JSON.parse(text)
      return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : ({})
    } catch (e) {
      return ({})
    }
  }

  function themeSection(name) {
    const fromEnv = artifactThemeEnv[name]
    if (fromEnv && typeof fromEnv === "object" && !Array.isArray(fromEnv)) return fromEnv
    return ({})
  }

  function shellColor(name, fallback) {
    const shell = themeSection("shell")
    const color = shell[name]
    return isHexColor(color) ? color : fallback
  }

  function isHexColor(value) {
    return typeof value === "string" && /^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/.test(value)
  }

  function stateColor(name) {
    const states = themeSection("states")
    const color = states[name]
    return isHexColor(color) ? color : "transparent"
  }

  function hostAccentColor() {
    const host = themeSection("host")
    return isHexColor(host.accent) ? host.accent : "transparent"
  }

  function vmBorderColor(vm) {
    if (!vm || !vm.name) return hostAccentColor()
    const vms = themeSection("vms")
    const themed = vms[vm.name]
    const border = themed && typeof themed === "object" ? themed.border : null
    const active = border && typeof border === "object" ? border.active : null
    return isHexColor(active) ? active : "transparent"
  }

  function vmDotColor(vm) {
    if (vm.pendingRestart) return stateColor("pendingRestart")
    if (vm.state === "running") return stateColor("running")
    if (vm.state === "starting" || vm.state === "stopping") return stateColor("transitioning")
    return stateColor("unknown")
  }

  function vmGlyph(vm) {
    if (vm.state === "running") return "●"
    if (vm.state === "starting" || vm.state === "stopping") return "◐"
    return "●"
  }

  function vmMeta(vm) {
    const parts = [vm.env || "default"]
    if (vm.state && vm.state !== "unknown") parts.push(vm.state)
    if (vm.staticIp) parts.push(vm.staticIp)
    if (vm.pendingRestart) parts.push("pending restart")
    const audio = root.audioBadge(vm)
    if (audio.length > 0) parts.push(audio)
    return parts.join(" · ")
  }

  function reload() {
    statusProc.exec([backend, "status-json"])
  }

  function reloadUsbDevices() {
    usbDevicesProc.exec([backend, "usb-devices-json"])
  }

  function action(args) {
    busy = true
    actionBusy = true
    actionMessage = runningMessage(args)
    actionFailed = false
    actionClearTimer.stop()
    actionProc.args = args
    actionProc.exec([backend, "action"].concat(args))
  }

  function attachOrPrompt(card, vm, u) {
    if (!canUsb(vm, u)) return
    if (u.bound) {
      action(["usb-detach", vm.name, u.busId])
    } else if (u.busId && u.busId !== "pending") {
      action(["usb-attach", vm.name, u.busId])
    } else {
      card.usbEntryVisible = !card.usbEntryVisible
      card.usbEntryText = ""
      if (card.usbEntryVisible) {
        reloadUsbDevices()
        hoverHint = "Select a USB device or enter a bus id for " + vm.name
      } else {
        hoverHint = ""
      }
    }
  }

  function statusText() {
    if (actionMessage.length > 0) return actionMessage
    if (busy) return "working…"
    if (state.connectivity === "connected") return "live"
    if (state.connectivity === "auth-denied") return "auth denied"
    if (state.stale) return "stale"
    return "daemon down"
  }

  function canMutate() {
    return state.connectivity === "connected" && state.role !== "none" && !busy
  }

  function canAdminMutate() {
    return state.connectivity === "connected" && state.role === "admin" && !busy
  }

  function hasCapability(vm, capability) {
    const caps = (vm && vm.capabilities) ? vm.capabilities : ({})
    return caps[capability] !== false
  }

  function canStart(vm) {
    return canAdminMutate() && vm.state !== "running" && hasCapability(vm, "start")
  }

  function canStop(vm) {
    return canAdminMutate() && vm.state === "running" && hasCapability(vm, "stop")
  }

  function supportsCapability(vm, capability) {
    const caps = (vm && vm.capabilities) ? vm.capabilities : ({})
    return caps[capability] === true
  }

  function canForceStop(vm) {
    return canAdminMutate() && vm.state === "running" && supportsCapability(vm, "forceStop")
  }

  function forceStopLabel(vm) {
    return confirmKey === "force-stop:" + vm.name ? "confirm force" : "force shutdown"
  }

  function forceStopDisabledReason(vm) {
    if (state.connectivity !== "connected" || state.role !== "admin" || vm.state !== "running" || busy) return root.disabledReason(vm, "admin", "stop")
    if (!supportsCapability(vm, "forceStop")) return "Force shutdown is waiting for d2b force-stop support"
    return root.disabledReason(vm, "admin", "stop")
  }

  function canAdvanced(vm, capability) {
    return canAdminMutate() && vm.state === "running" && hasCapability(vm, capability)
  }

  function canUsb(vm, u) {
    return canAdminMutate() && hasCapability(vm, "usbHotplug") && (!u.ownerVm || u.ownerVm === vm.name)
  }

  function hasAudio(vm) {
    return !!(vm && vm.audio)
  }

  function canAudio(vm) {
    return canAdminMutate() && hasAudio(vm) && vm.audio.enforcement !== "unsupported" && vm.audio.enforcement !== "unknown" && !vm.audio.errorKind
  }

  function audioDisabledReason(vm) {
    if (!hasAudio(vm)) return "Audio status is not available from this d2b generation"
    if (vm.audio.errorKind) return vm.audio.remediation || ("Audio unavailable: " + vm.audio.errorKind)
    if (vm.audio.enforcement === "unsupported") return "Audio controls are unsupported for this VM runtime"
    if (state.connectivity !== "connected") return "d2bd is unreachable"
    if (state.role !== "admin") return "Requires admin role"
    if (busy) return "Action in progress"
    return "Unavailable"
  }

  function audioBadge(vm) {
    if (!hasAudio(vm)) return ""
    if (vm.audio.errorKind) return "audio issue"
    if (vm.audio.microphone && !vm.audio.microphone.muted) return "hot mic"
    if (vm.audio.enforcement === "host-only") return "host-only audio"
    if (vm.audio.enforcement === "guest-only") return "guest-only audio"
    if (vm.audio.enforcement === "unsupported" || vm.audio.enforcement === "unknown") return "audio unsupported"
    return ""
  }

  function audioBadgeAccent(vm) {
    const badge = root.audioBadge(vm)
    if (badge === "hot mic" || (vm.audio && vm.audio.errorKind)) return root.stateColor("error")
    if (badge.length > 0) return root.stateColor("pendingRestart")
    return root.shellColor("muted", "#9399b2")
  }

  function audioBadgeFill(vm) {
    const badge = root.audioBadge(vm)
    if (badge === "hot mic" || (vm.audio && vm.audio.errorKind)) return root.shellColor("error_surface", "#2e1a1a")
    if (badge.length > 0) return root.shellColor("warning_surface", "#2e2a1a")
    return "transparent"
  }

  function audioLevel(channel, fallback) {
    if (!channel || channel.level === undefined || channel.level === null) return fallback
    return root.clamp(channel.level, 0, 100)
  }

  function audioLevelAction(vm, channelName, level) {
    const safe = String(Math.round(root.clamp(level, 0, 100)))
    if (channelName === "speaker") root.action(["audio-speaker-volume", vm.name, safe])
    else root.action(["audio-mic-gain", vm.name, safe])
  }

  function audioToggleAction(vm, channelName, muted) {
    const next = muted ? "on" : "off"
    if (channelName === "speaker") root.action(["audio-speaker", vm.name, next])
    else root.action(["audio-mic", vm.name, next])
  }

  function audioTooltip(vm) {
    if (!hasAudio(vm)) return root.audioDisabledReason(vm)
    const a = vm.audio
    const parts = ["Audio " + a.enforcement]
    parts.push("speaker " + (a.speaker.muted ? "off" : "on") + " " + root.audioLevel(a.speaker, 80) + "%")
    parts.push("mic " + (a.microphone.muted ? "off" : "on") + " gain " + root.audioLevel(a.microphone, 50) + "%")
    if (a.remediation) parts.push(a.remediation)
    return parts.join(" · ")
  }

  function audioSliderTooltip(vm, channelName) {
    if (!hasAudio(vm)) return root.audioDisabledReason(vm)
    const channel = channelName === "speaker" ? vm.audio.speaker : vm.audio.microphone
    if (channel && channel.muted) return channelName === "speaker" ? "Speaker is off; turn speaker on to adjust volume" : "Microphone is off; turn mic on to adjust gain"
    if (!root.canAudio(vm)) return root.audioDisabledReason(vm)
    return channelName === "speaker" ? "Speaker playback volume; sends on release" : "Microphone input gain; sends on release"
  }

  function usbLabel(u) {
    if (u.ownerVm && u.ownerVm !== u.vm) return "owned " + u.ownerVm
    if (u.busId === "pending") return u.bound ? "detach USB" : "attach USB"
    return (u.bound ? "detach " : "attach ") + u.busId
  }

  function visibleUsbClaims(vm) {
    return (vm.usb || []).filter(u => u.busId !== "pending")
  }

  function usbTooltip(vm, u) {
    const device = usbDevice(u.busId)
    const name = device ? device.label : "USB " + u.busId
    if (u.ownerVm && u.ownerVm !== vm.name) return name + " is owned by " + u.ownerVm
    return (u.bound ? "Detach " : "Attach ") + name
  }

  function usbDevice(busId) {
    const devices = usbDevices || []
    for (let i = 0; i < devices.length; i++) {
      if (devices[i].busId === busId) return devices[i]
    }
    return null
  }

  function shortDeviceLabel(device) {
    const product = device.product || device.label || "USB device"
    const name = product.length > 18 ? product.substring(0, 17) + "…" : product
    return name + " " + device.busId
  }

  function runningMessage(args) {
    const verb = args[0] || "action"
    const vm = args[1] || ""
    if (verb === "usb-attach") return "Attaching USB to " + vm + "..."
    if (verb === "usb-detach") return "Detaching USB from " + vm + "..."
    if (verb === "audio-mic") return "Updating microphone for " + vm + "..."
    if (verb === "audio-speaker") return "Updating speaker for " + vm + "..."
    if (verb === "audio-speaker-volume") return "Updating speaker volume for " + vm + "..."
    if (verb === "audio-mic-gain") return "Updating microphone gain for " + vm + "..."
    if (verb === "audio-off") return "Disabling audio for " + vm + "..."
    if (verb === "terminal") return "Opening terminal in " + vm + "..."
    if (verb === "quick-launch") return "Launching " + (args[2] || "command") + " in " + vm + "..."
    if (verb === "build") return "Building " + vm + "..."
    if (verb === "boot") return "Staging " + vm + " for next boot..."
    if (verb === "switch") return "Switching " + vm + "..."
    if (verb === "store-verify") return "Verifying store for " + vm + "..."
    if (verb === "observability") return "Opening observability..."
    if (verb === "restart") return "Restarting " + vm + "..."
    if (verb === "start") return "Starting " + vm + "..."
    if (verb === "stop") return "Requesting graceful stop for " + vm + "..."
    if (verb === "force-stop") return "Force shutting down " + vm + " (skipping graceful guest shutdown)..."
    return "Working..."
  }

  function successMessage(args) {
    const verb = args[0] || "action"
    const vm = args[1] || ""
    if (verb === "usb-attach") return "USB attached to " + vm
    if (verb === "usb-detach") return "USB detached from " + vm
    if (verb === "audio-mic") return "Microphone updated for " + vm
    if (verb === "audio-speaker") return "Speaker updated for " + vm
    if (verb === "audio-speaker-volume") return "Speaker volume updated for " + vm
    if (verb === "audio-mic-gain") return "Microphone gain updated for " + vm
    if (verb === "audio-off") return "Audio disabled for " + vm
    if (verb === "terminal") return "Terminal launch requested for " + vm
    if (verb === "quick-launch") return "Quick launch requested for " + vm
    if (verb === "build") return "Build completed for " + vm
    if (verb === "boot") return "Boot generation staged for " + vm
    if (verb === "switch") return "Switched " + vm
    if (verb === "store-verify") return "Store verified for " + vm
    if (verb === "observability") return observabilitySuccess
    if (verb === "restart") return "Restarted " + vm
    if (verb === "start") return "Started " + vm
    if (verb === "stop") return "Graceful stop requested for " + vm
    if (verb === "force-stop") return "Force shutdown requested for " + vm
    return "Done"
  }

  function screenWidth() {
    return panel.screen ? panel.screen.width : 1280
  }

  function screenHeight() {
    return panel.screen ? panel.screen.height : 1080
  }

  function clamp(value, min, max) {
    return Math.max(min, Math.min(max, value))
  }

  function movePanel(dx, dy) {
    panelRightMargin = clamp(panelRightMargin - dx, 4, Math.max(4, screenWidth() - panel.width - 4))
    panelTopMargin = clamp(panelTopMargin + dy, 4, Math.max(4, screenHeight() - panel.height - 4))
  }

  function panelContentHeight() {
    const resultHeight = actionMessage.length > 0 && !busy ? Math.max(26, actionResult.implicitHeight + 10) + 12 : 0
    return 32 + 12 + 1 + 12 + 26 + 12 + resultHeight + list.height + 32
  }

  function reclampPanelMargins() {
    panelRightMargin = clamp(panelRightMargin, 4, Math.max(4, screenWidth() - panel.width - 4))
    panelTopMargin = clamp(panelTopMargin, 4, Math.max(4, screenHeight() - panel.height - 4))
  }

  function disabledReason(vm, role, capability) {
    if (vm && capability && !hasCapability(vm, capability)) return "Unsupported by this VM runtime"
    if (state.connectivity !== "connected") return state.connectivity === "auth-denied" ? "Authorization denied" : "d2bd is unreachable"
    if (role === "admin" && state.role !== "admin") return "Requires admin role"
    if (state.role === "none") return "Requires launcher role"
    if (vm && vm.state !== "running") return "VM must be running"
    return "Unavailable"
  }

  function confirmAction(key, message, args, timeoutMs) {
    if (confirmKey === key) {
      confirmKey = ""
      confirmTimer.stop()
      action(args)
    } else {
      confirmKey = key
      hoverHint = message
      confirmTimer.interval = timeoutMs || normalConfirmMs
      confirmTimer.restart()
    }
  }

  function confirmForceStop(vm) {
    const key = "force-stop:" + vm.name
    const warning = "Danger: click Force shutdown again to kill " + vm.name + " without graceful guest shutdown"
    root.confirmAction(key, warning, ["force-stop", vm.name], forceConfirmMs)
  }

  Process {
    id: statusProc
    stdout: StdioCollector {
      onStreamFinished: {
        try {
          root.state = JSON.parse(this.text)
        } catch (e) {
          root.state = ({ connectivity: "daemon-down", role: "none", vms: [], note: String(e) })
        }
      }
    }
    stderr: StdioCollector {}
    onExited: if (!root.actionBusy) root.busy = false
  }

  Process {
    id: usbDevicesProc
    stdout: StdioCollector {
      onStreamFinished: {
        try {
          root.usbDevices = JSON.parse(this.text)
        } catch (e) {
          root.usbDevices = []
          root.hoverHint = "Could not list USB devices: " + String(e)
        }
      }
    }
    stderr: StdioCollector {}
  }

  Process {
    id: actionProc
    property string out: ""
    property string err: ""
    property var args: []
    stdout: StdioCollector {
      onStreamFinished: actionProc.out = this.text.trim()
    }
    stderr: StdioCollector {
      onStreamFinished: actionProc.err = this.text.trim()
    }
    onExited: (exitCode, exitStatus) => {
      const ok = exitCode === 0 && exitStatus === 0
      if (!ok) {
        root.actionFailed = true
        root.actionMessage = actionProc.err.length > 0 ? actionProc.err : (actionProc.out.length > 0 ? actionProc.out : "Action failed")
      } else if (actionProc.out.length > 0) {
        root.actionFailed = false
        root.actionMessage = actionProc.out
      } else {
        root.actionFailed = false
        root.actionMessage = root.successMessage(actionProc.args)
      }
      actionProc.out = ""
      actionProc.err = ""
      actionProc.args = []
      root.actionBusy = false
      root.busy = false
      actionClearTimer.restart()
      root.reload()
    }
  }

  Timer {
    id: actionClearTimer
    interval: 2200
    repeat: false
    onTriggered: {
      if (!root.busy) root.actionMessage = ""
    }
  }

  Timer {
    id: confirmTimer
    interval: root.normalConfirmMs
    repeat: false
    onTriggered: {
      root.confirmKey = ""
      if (root.hoverHint.indexOf("Click again") === 0 || root.hoverHint.indexOf("Danger:") === 0) root.hoverHint = ""
      confirmTimer.interval = root.normalConfirmMs
    }
  }

  Component.onCompleted: reloadUsbDevices()

  Timer {
    interval: 2500
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: if (!statusProc.running && !actionProc.running) root.reload()
  }

  PanelWindow {
    id: panel
    visible: true
    focusable: true
    aboveWindows: true
    exclusiveZone: 0
    implicitWidth: 420
    implicitHeight: Math.min(Math.max(240, root.panelContentHeight()), Math.floor(root.screenHeight() * 0.5))
    color: "transparent"
    surfaceFormat { opaque: false }

    anchors { top: true; right: true }
    margins { top: root.panelTopMargin; right: root.panelRightMargin }
    onWidthChanged: root.reclampPanelMargins()
    onHeightChanged: root.reclampPanelMargins()
    onScreenChanged: root.reclampPanelMargins()

    Rectangle {
      anchors.fill: parent
      radius: 18
      color: root.shellColor("background", "#0f1117")
      border.color: root.shellColor("border", "#2a2d35")
      border.width: 1
      clip: true

      Column {
        id: shellContent
        x: 16
        y: 16
        width: parent.width - 32
        height: parent.height - 32
        spacing: 12
        onImplicitHeightChanged: root.reclampPanelMargins()

          Item {
            width: parent.width
            height: 32

            MouseArea {
              id: dragHandle
              anchors.fill: parent
              acceptedButtons: Qt.LeftButton
              property real lastX: 0
              property real lastY: 0
              onPressed: (mouse) => {
                lastX = mouse.x
                lastY = mouse.y
              }
              onPositionChanged: (mouse) => {
                if (pressed) root.movePanel(mouse.x - lastX, mouse.y - lastY)
              }
            }

            Rectangle {
              width: 66
              height: 22
              radius: 999
              color: root.state.connectivity === "connected" ? root.shellColor("success_surface", "#1a2e1a") : root.shellColor("error_surface", "#2e1a1a")
              anchors.left: parent.left
              anchors.verticalCenter: parent.verticalCenter
              Text {
                anchors.centerIn: parent
                color: root.state.connectivity === "connected" ? root.stateColor("running") : root.stateColor(root.state.connectivity === "auth-denied" ? "denied" : "error")
                font.pixelSize: 11
                font.bold: true
                text: root.state.role || "none"
              }
            }

            Text {
              anchors.centerIn: parent
              width: parent.width - 160
              anchors.verticalCenter: parent.verticalCenter
              color: root.shellColor("foreground_strong", "#ffffff")
              font.pixelSize: 16
              font.bold: true
              horizontalAlignment: Text.AlignHCenter
              text: "d2b"
            }

            Row {
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              spacing: 8

              IconButton {
                text: "monitoring"
                tooltip: root.observabilityEnabled ? "Open Signoz observability portal" : "Observability URL is not configured"
                accent: root.shellColor("foreground_strong", "#ffffff")
                enabled: root.observabilityEnabled && !root.busy
                onClicked: root.action(["observability"])
              }
              IconButton {
                text: "refresh"
                tooltip: "Refresh VM status"
                accent: root.shellColor("foreground_strong", "#ffffff")
                enabled: !root.busy
                onClicked: root.reload()
              }
            }
          }

          Rectangle {
            width: parent.width
            height: 1
            color: root.shellColor("border", "#2a2d35")
          }

          Row {
            width: parent.width
            height: 26
            spacing: 10
            Text {
              color: root.shellColor("foreground_strong", "#ffffff")
              font.pixelSize: 13
              font.bold: true
              text: root.runningCount() + "/" + root.visibleVms().length + " running"
            }
            Text {
              color: root.shellColor("muted", "#9399b2")
              font.pixelSize: 12
              text: root.hoverHint.length > 0 ? root.hoverHint : root.statusText()
            }
          }

          Rectangle {
            visible: root.actionMessage.length > 0 && !root.busy
            width: parent.width
            height: visible ? Math.max(26, actionResult.implicitHeight + 10) : 0
            radius: 10
            color: root.actionFailed ? root.shellColor("error_surface", "#2e1a1a") : root.shellColor("success_surface", "#1a2e1a")
            border.color: root.actionFailed ? root.stateColor("error") : root.stateColor("running")
            border.width: 1

            Text {
              id: actionResult
              anchors.fill: parent
              anchors.margins: 6
              color: root.actionFailed ? root.stateColor("error") : root.stateColor("running")
              font.pixelSize: 11
              elide: Text.ElideRight
              verticalAlignment: Text.AlignVCenter
              text: root.actionMessage
            }
          }

          Item {
            width: parent.width
            height: Math.max(96, panel.height - y - 16)

          Flickable {
            id: vmListFlickable
            anchors.fill: parent
            contentWidth: width
            contentHeight: list.implicitHeight
            clip: true
            boundsBehavior: Flickable.StopAtBounds

            Column {
              id: list
              width: parent.width
              spacing: 8

              Repeater {
                model: root.visibleVms()

                Rectangle {
                  id: vmCard
                  width: list.width
                  height: cardContent.implicitHeight + 16
                  radius: 13
                  color: root.shellColor("surface", "#16181d")
                  border.color: root.vmBorderColor(vm)
                  border.width: 1
                  clip: true

                  property var vm: modelData
                  property bool expanded: false
                  property bool usbEntryVisible: false
                  property string usbEntryText: ""

                Column {
                  id: cardContent
                  anchors.left: parent.left
                  anchors.right: parent.right
                  anchors.top: parent.top
                  anchors.margins: 8
                  spacing: 6

                  Item {
                    width: parent.width
                    height: 30

                    Text {
                      id: stateDot
                      width: 20
                      anchors.left: parent.left
                      anchors.verticalCenter: parent.verticalCenter
                      color: root.vmDotColor(vm)
                      font.pixelSize: 15
                      horizontalAlignment: Text.AlignHCenter
                      text: root.vmGlyph(vm)
                    }

                    Row {
                      id: actionButtons
                      spacing: 6
                      anchors.right: parent.right
                      anchors.verticalCenter: parent.verticalCenter
                      IconButton {
                        text: vm.state === "running" ? "stop" : "play_arrow"
                        tooltip: enabled ? ((vm.state === "running" ? "Gracefully stop " : "Start ") + vm.name) : root.disabledReason(vm, "admin", vm.state === "running" ? "stop" : "start")
                        accent: vm.state === "running" ? root.stateColor("transitioning") : root.stateColor("running")
                        enabled: vm.state === "running" ? root.canStop(vm) : root.canStart(vm)
                        prominent: true
                        onClicked: {
                          if (vm.state === "running") root.confirmAction("stop:" + vm.name, "Click again to gracefully stop " + vm.name, ["stop", vm.name])
                          else root.action(["start", vm.name])
                        }
                      }
                      IconButton {
                        text: expanded ? "expand_less" : "more_horiz"
                        tooltip: expanded ? "Hide controls" : "More controls"
                        accent: root.shellColor("foreground_strong", "#ffffff")
                        enabled: root.state.connectivity === "connected"
                        onClicked: expanded = !expanded
                      }
                    }

                    Column {
                      anchors.left: stateDot.right
                      anchors.leftMargin: 8
                      anchors.right: actionButtons.left
                      anchors.rightMargin: 8
                      anchors.verticalCenter: parent.verticalCenter
                      spacing: 1
                      Text {
                        width: parent.width
                        color: root.shellColor("foreground_strong", "#ffffff")
                        font.pixelSize: 14
                        font.bold: true
                        elide: Text.ElideRight
                        text: vm.name
                      }
                      Text {
                        width: parent.width
                        color: root.shellColor("muted", "#9399b2")
                        font.pixelSize: 11
                        elide: Text.ElideRight
                        text: root.vmMeta(vm).replace(" · hot mic", "")
                      }
                    }
                  }

                  Rectangle {
                    visible: root.audioBadge(vm).length > 0
                    width: parent.width
                    height: 22
                    radius: 999
                    color: root.audioBadgeFill(vm)
                    border.color: root.audioBadgeAccent(vm)
                    border.width: 1
                    Text {
                      anchors.centerIn: parent
                      color: root.audioBadgeAccent(vm)
                      font.pixelSize: 11
                      font.bold: true
                      text: root.audioBadge(vm)
                    }
                  }

                  Row {
                    id: quickActions
                    width: parent.width
                    spacing: 8
                    IconButton { text: "terminal"; tooltip: enabled ? ("Open a terminal in " + vm.name) : root.disabledReason(vm, "admin", "terminal"); accent: root.shellColor("foreground_strong", "#ffffff"); enabled: root.canAdvanced(vm, "terminal") && root.state.role === "admin"; onClicked: root.action(["terminal", vm.name]) }
                    Repeater {
                      model: vm.quickLaunch || []
                      IconButton { text: modelData.icon; tooltip: enabled ? modelData.tooltip : root.disabledReason(vm, "admin", "terminal"); accent: root.shellColor("foreground_strong", "#ffffff"); enabled: root.canAdvanced(vm, "terminal") && root.state.role === "admin"; onClicked: root.action(["quick-launch", vm.name, modelData.id]) }
                    }
                    IconButton { text: "restart_alt"; tooltip: enabled ? ("Restart " + vm.name) : root.disabledReason(vm, "admin", "restart"); accent: root.shellColor("muted", "#9399b2"); enabled: root.canAdvanced(vm, "restart"); onClicked: root.confirmAction("restart:" + vm.name, "Click again to confirm restarting " + vm.name, ["restart", vm.name]) }
                    IconButton { text: "verified"; tooltip: enabled ? ("Verify " + vm.name + " store integrity") : root.disabledReason(vm, "admin", "storeVerify"); accent: root.shellColor("muted", "#9399b2"); enabled: root.canAdminMutate() && root.hasCapability(vm, "storeVerify"); onClicked: root.action(["store-verify", vm.name]) }
                    IconButton { text: "build"; tooltip: enabled ? ("Build/evaluate " + vm.name + " without activating") : root.disabledReason(vm, "launcher", "build"); accent: root.shellColor("muted", "#9399b2"); enabled: root.canMutate() && root.hasCapability(vm, "build"); onClicked: root.action(["build", vm.name]) }
                    IconButton { text: "move_up"; tooltip: enabled ? ("Stage " + vm.name + " for next boot") : root.disabledReason(vm, "admin", "boot"); accent: root.shellColor("muted", "#9399b2"); enabled: root.canAdminMutate() && root.hasCapability(vm, "boot"); onClicked: root.action(["boot", vm.name]) }
                    IconButton { text: "sync_alt"; tooltip: enabled ? ("Switch " + vm.name + " generation now") : root.disabledReason(vm, "admin", "switch"); accent: root.shellColor("muted", "#9399b2"); enabled: root.canAdvanced(vm, "switch"); onClicked: root.confirmAction("switch:" + vm.name, "Click again to confirm switching " + vm.name, ["switch", vm.name]) }
                  }

                  Column {
                    id: audioControls
                    visible: expanded && root.hasAudio(vm)
                    width: parent.width
                    height: visible ? implicitHeight : 0
                    spacing: 6

                    Row {
                      width: parent.width
                      spacing: 6
                      ControlChip {
                        icon: vm.audio && vm.audio.microphone && !vm.audio.microphone.muted ? "mic" : "mic_off"
                        label: vm.audio && vm.audio.microphone && !vm.audio.microphone.muted ? "mic on" : "mic off"
                        tooltip: enabled ? root.audioTooltip(vm) : root.audioDisabledReason(vm)
                        accent: vm.audio && vm.audio.microphone && !vm.audio.microphone.muted ? root.stateColor("running") : root.shellColor("muted", "#9399b2")
                        enabled: root.canAudio(vm)
                        onClicked: root.audioToggleAction(vm, "microphone", vm.audio.microphone.muted)
                      }
                      ControlChip {
                        icon: vm.audio && vm.audio.speaker && !vm.audio.speaker.muted ? "volume_up" : "volume_off"
                        label: vm.audio && vm.audio.speaker && !vm.audio.speaker.muted ? "speaker on" : "speaker off"
                        tooltip: enabled ? root.audioTooltip(vm) : root.audioDisabledReason(vm)
                        accent: vm.audio && vm.audio.speaker && !vm.audio.speaker.muted ? root.stateColor("running") : root.shellColor("muted", "#9399b2")
                        enabled: root.canAudio(vm)
                        onClicked: root.audioToggleAction(vm, "speaker", vm.audio.speaker.muted)
                      }
                      ControlChip {
                        icon: "no_sound"
                        label: "audio off"
                        tooltip: enabled ? ("Disable microphone and speaker for " + vm.name) : root.audioDisabledReason(vm)
                        accent: root.stateColor("error")
                        enabled: root.canAudio(vm)
                        onClicked: root.action(["audio-off", vm.name])
                      }
                    }

                    AudioSlider {
                      width: parent.width
                      icon: "volume_up"
                      label: "speaker"
                      value: root.audioLevel(vm.audio ? vm.audio.speaker : null, 80)
                      enabled: root.canAudio(vm) && vm.audio && vm.audio.speaker && !vm.audio.speaker.muted
                      tooltip: root.audioSliderTooltip(vm, "speaker")
                      onCommitted: (level) => root.audioLevelAction(vm, "speaker", level)
                    }

                    AudioSlider {
                      width: parent.width
                      icon: "mic"
                      label: "mic gain"
                      value: root.audioLevel(vm.audio ? vm.audio.microphone : null, 50)
                      enabled: root.canAudio(vm) && vm.audio && vm.audio.microphone && !vm.audio.microphone.muted
                      tooltip: root.audioSliderTooltip(vm, "microphone")
                      onCommitted: (level) => root.audioLevelAction(vm, "microphone", level)
                    }
                  }

                  Flow {
                    id: destructiveControls
                    visible: expanded
                    width: parent.width
                    spacing: 6

                    ControlChip {
                      icon: "dangerous"
                      label: root.forceStopLabel(vm)
                      tooltip: enabled ? ("Force shutdown " + vm.name + "; skips graceful guest shutdown") : root.forceStopDisabledReason(vm)
                      accent: root.stateColor("error")
                      enabled: root.canForceStop(vm)
                      onClicked: root.confirmForceStop(vm)
                    }
                  }

                  Flow {
                    id: usbControls
                    visible: expanded && (root.visibleUsbClaims(vm).length > 0 || (root.state.connectivity === "connected" && root.state.role !== "none"))
                    width: parent.width
                    spacing: 6
                    Repeater {
                      model: root.visibleUsbClaims(vm)
                      ControlChip {
                        icon: modelData.bound ? "usb_off" : "usb"
                        label: root.usbLabel(modelData)
                        tooltip: enabled ? root.usbTooltip(vm, modelData) : (modelData.ownerVm && modelData.ownerVm !== vm.name ? root.usbTooltip(vm, modelData) : root.disabledReason(vm, "admin", "usbHotplug"))
                        accent: root.shellColor("muted", "#9399b2")
                        enabled: root.canUsb(vm, modelData)
                        onClicked: root.attachOrPrompt(vmCard, vm, modelData)
                      }
                    }
                    ControlChip {
                      icon: "add"
                      label: "USB"
                      tooltip: enabled ? ("Attach another USB device to " + vm.name) : root.disabledReason(vm, "admin", "usbHotplug")
                      accent: root.shellColor("muted", "#9399b2")
                      enabled: root.canAdminMutate() && root.hasCapability(vm, "usbHotplug")
                      onClicked: root.attachOrPrompt(vmCard, vm, ({ busId: "pending", bound: false, ownerVm: null }))
                    }
                    Rectangle {
                      visible: vm.pendingRestart
                      height: 24
                      width: restartText.width + 18
                      radius: 999
                      color: root.shellColor("warning_surface", "#2e2a1a")
                      Text { id: restartText; anchors.centerIn: parent; color: root.stateColor("pendingRestart"); font.pixelSize: 10; font.bold: true; text: "restart" }
                    }
                  }

                  Row {
                    visible: expanded && usbEntryVisible
                    width: parent.width
                    height: visible ? chooserFlow.implicitHeight : 0
                    spacing: 6

                    Flow {
                      id: chooserFlow
                      width: parent.width
                      spacing: 6

                      Repeater {
                        model: root.usbDevices
                        ControlChip {
                          icon: "usb"
                          label: root.shortDeviceLabel(modelData)
                          tooltip: "Attach " + modelData.label + " to " + vm.name
                          accent: root.shellColor("muted", "#9399b2")
                          enabled: root.canAdminMutate()
                          onClicked: root.action(["usb-attach", vm.name, modelData.busId])
                        }
                      }
                    }

                  }

                  Row {
                    visible: expanded && usbEntryVisible
                    width: parent.width
                    height: visible ? 30 : 0
                    spacing: 6

                    Rectangle {
                      width: parent.width - 86
                      height: 28
                      radius: 8
                      color: root.shellColor("input_background", "#0d0d0d")
                      border.color: root.shellColor("border", "#2a2d35")
                      border.width: 1

                      TextInput {
                        id: usbEntry
                        anchors.fill: parent
                        anchors.leftMargin: 9
                        anchors.rightMargin: 9
                        color: root.shellColor("foreground_strong", "#ffffff")
                        selectionColor: root.hostAccentColor()
                        selectedTextColor: root.shellColor("inverse_foreground", "#000000")
                        font.pixelSize: 12
                        verticalAlignment: TextInput.AlignVCenter
                        text: usbEntryText
                        onTextChanged: usbEntryText = text
                        Keys.onReturnPressed: {
                          if (usbEntryText.length > 0) root.action(["usb-attach", vm.name, usbEntryText])
                        }
                      }
                      Text {
                        visible: usbEntry.text.length === 0
                        anchors.verticalCenter: parent.verticalCenter
                        anchors.left: parent.left
                        anchors.leftMargin: 9
                        color: root.shellColor("muted", "#9399b2")
                        font.pixelSize: 12
                        text: "USB bus id (e.g. 1-2)"
                      }
                    }

                    ControlChip {
                      icon: "usb"
                      label: "attach"
                      tooltip: "Attach entered USB bus id"
                      accent: root.shellColor("muted", "#9399b2")
                      enabled: usbEntryText.length > 0 && root.canAdminMutate() && root.hasCapability(vm, "usbHotplug")
                      onClicked: root.action(["usb-attach", vm.name, usbEntryText])
                    }
                  }

                }

                MouseArea {
                  anchors.fill: parent
                  acceptedButtons: Qt.RightButton
                  onClicked: expanded = !expanded
                }
              }
            }

              Rectangle {
                parent: vmListFlickable
                visible: vmListFlickable.contentHeight > vmListFlickable.height
                width: 4
                height: vmListFlickable.height
                x: vmListFlickable.width - width
                y: 0
                radius: 999
                color: root.shellColor("input_background", "#0d0d0d")

                Rectangle {
                  width: parent.width
                  radius: 999
                  color: root.shellColor("muted", "#9399b2")
                  height: Math.max(24, parent.height * (vmListFlickable.height / vmListFlickable.contentHeight))
                  y: (parent.height - height) * (vmListFlickable.contentY / Math.max(1, vmListFlickable.contentHeight - vmListFlickable.height))
                }
              }
          }
      }
    }
  }
  }

  component IconButton: Rectangle {
    property alias text: label.text
    property string tooltip: ""
    property color accent: root.shellColor("muted", "#9399b2")
    property bool prominent: false
    signal clicked()
    width: prominent ? 30 : 26
    height: prominent ? 30 : 26
    radius: width / 2
    opacity: enabled ? 1.0 : 0.28
    border.width: prominent ? 1 : 0
    border.color: prominent ? accent : "transparent"
    color: prominent
      ? Qt.rgba(accent.r, accent.g, accent.b, mouse.containsMouse ? 0.34 : 0.24)
      : (mouse.containsMouse ? Qt.rgba(accent.r, accent.g, accent.b, 0.12) : "transparent")

    Text {
      id: label
      anchors.fill: parent
      color: parent.prominent ? root.shellColor("foreground_strong", "#f5f7ff") : parent.accent
      font.family: "Material Symbols Rounded"
      font.pixelSize: prominent ? 21 : 20
      font.bold: false
      horizontalAlignment: Text.AlignHCenter
      verticalAlignment: Text.AlignVCenter
    }
    MouseArea {
      id: mouse
      anchors.fill: parent
      hoverEnabled: true
      onContainsMouseChanged: root.hoverHint = containsMouse ? (parent.tooltip.length > 0 ? parent.tooltip : parent.text) : ""
      onClicked: if (parent.enabled) parent.clicked()
      onEntered: parent.scale = 1.05
      onExited: parent.scale = 1.0
    }
  }

  component AudioSlider: Rectangle {
    id: audioSlider
    property string icon: ""
    property string label: ""
    property int value: 0
    property string tooltip: ""
    property int draftValue: value
    property bool dragging: false
    signal committed(int level)

    height: 30
    radius: 10
    activeFocusOnTab: true
    opacity: enabled ? 1.0 : 0.34
    color: root.shellColor("input_background", "#0d0d0d")
    border.color: activeFocus ? root.hostAccentColor() : root.shellColor("border", "#2a2d35")
    border.width: 1
    Keys.onLeftPressed: if (enabled && !dragging) { draftValue = root.clamp(draftValue - 5, 0, 100); commitTimer.restart() }
    Keys.onRightPressed: if (enabled && !dragging) { draftValue = root.clamp(draftValue + 5, 0, 100); commitTimer.restart() }
    Keys.onPressed: (event) => {
      if (!enabled || dragging) return
      if (event.key === Qt.Key_PageDown) {
        draftValue = root.clamp(draftValue - 10, 0, 100)
        commitTimer.restart()
        event.accepted = true
      } else if (event.key === Qt.Key_PageUp) {
        draftValue = root.clamp(draftValue + 10, 0, 100)
        commitTimer.restart()
        event.accepted = true
      }
    }

    onValueChanged: if (!dragging && !commitTimer.running) draftValue = value
    onEnabledChanged: if (!enabled) { commitTimer.stop(); dragging = false; draftValue = value }

    Timer {
      id: commitTimer
      interval: 350
      repeat: false
      onTriggered: if (audioSlider.enabled) committed(draftValue)
    }

    Text {
      id: sliderIcon
      anchors.left: parent.left
      anchors.leftMargin: 8
      anchors.verticalCenter: parent.verticalCenter
      width: 18
      color: root.shellColor("foreground", "#cdd6f4")
      font.family: "Material Symbols Rounded"
      font.pixelSize: 16
      horizontalAlignment: Text.AlignHCenter
      text: parent.icon
    }
    Text {
      id: sliderLabel
      anchors.left: sliderIcon.right
      anchors.leftMargin: 6
      anchors.verticalCenter: parent.verticalCenter
      width: 70
      color: root.shellColor("foreground", "#cdd6f4")
      font.pixelSize: 11
      font.bold: true
      elide: Text.ElideRight
      text: parent.label
    }
    Rectangle {
      id: sliderTrack
      anchors.left: sliderLabel.right
      anchors.leftMargin: 6
      anchors.right: sliderValue.left
      anchors.rightMargin: 8
      anchors.verticalCenter: parent.verticalCenter
      height: 6
      radius: 999
      color: root.shellColor("slider_track", "#252832")
      Rectangle {
        width: parent.width * (parent.parent.draftValue / 100)
        height: parent.height
        radius: 999
        color: root.hostAccentColor()
      }
      Rectangle {
        width: 12
        height: 12
        radius: 999
        x: root.clamp(parent.width * (parent.parent.draftValue / 100) - width / 2, 0, parent.width - width)
        anchors.verticalCenter: parent.verticalCenter
        color: root.shellColor("foreground_strong", "#ffffff")
      }
    }
    Text {
      id: sliderValue
      anchors.right: parent.right
      anchors.rightMargin: 8
      anchors.verticalCenter: parent.verticalCenter
      width: 36
      color: root.shellColor("muted", "#9399b2")
      font.pixelSize: 11
      horizontalAlignment: Text.AlignRight
      text: parent.draftValue + "%"
    }

    MouseArea {
      id: sliderMouse
      anchors.fill: parent
      hoverEnabled: true
      preventStealing: true
      onPressed: (mouse) => {
        if (!parent.enabled) return
        parent.forceActiveFocus()
        if (mouse.x < sliderTrack.x || mouse.x > sliderTrack.x + sliderTrack.width) return
        commitTimer.stop()
        parent.dragging = true
        updateDraft(mouse.x)
      }
      onPositionChanged: (mouse) => {
        if (parent.dragging) updateDraft(mouse.x)
      }
      onReleased: {
        if (parent.dragging) {
          commitTimer.stop()
          if (parent.enabled) parent.committed(parent.draftValue)
        }
        parent.dragging = false
      }
      onCanceled: {
        if (parent.dragging) {
          commitTimer.stop()
          parent.draftValue = parent.value
        }
        parent.dragging = false
      }
      onContainsMouseChanged: root.hoverHint = containsMouse ? parent.tooltip : ""
      function updateDraft(x) {
        if (!parent.enabled) return
        const left = sliderTrack.x
        const pct = (x - left) / Math.max(1, sliderTrack.width)
        parent.draftValue = Math.round(root.clamp(pct, 0, 1) * 100)
      }
    }
  }

  component ControlChip: Rectangle {
    property string icon: ""
    property string label: ""
    property string tooltip: ""
    property color accent: root.shellColor("muted", "#9399b2")
    property color foreground: enabled ? root.shellColor("foreground_strong", "#f5f7ff") : root.shellColor("foreground_disabled", "#bac2de")
    signal clicked()

    height: 28
    width: chipRow.implicitWidth + 20
    radius: 999
    activeFocusOnTab: true
    opacity: enabled ? 1.0 : 0.54
    color: mouse.containsMouse && enabled ? Qt.rgba(accent.r, accent.g, accent.b, 0.28) : Qt.rgba(accent.r, accent.g, accent.b, enabled ? 0.18 : 0.08)
    border.color: activeFocus ? root.hostAccentColor() : Qt.rgba(accent.r, accent.g, accent.b, enabled ? 0.42 : 0.16)
    border.width: 1
    Keys.onSpacePressed: if (enabled) clicked()
    Keys.onReturnPressed: if (enabled) clicked()

    Row {
      id: chipRow
      anchors.centerIn: parent
      spacing: 5
      Text {
        color: parent.parent.foreground
        font.family: "Material Symbols Rounded"
        font.pixelSize: 17
        height: parent.parent.height
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
        text: parent.parent.icon
      }
      Text {
        color: parent.parent.foreground
        font.pixelSize: 11
        font.bold: true
        height: parent.parent.height
        verticalAlignment: Text.AlignVCenter
        text: parent.parent.label
      }
    }

    MouseArea {
      id: mouse
      anchors.fill: parent
      hoverEnabled: true
      onContainsMouseChanged: root.hoverHint = containsMouse ? parent.tooltip : ""
      onClicked: if (parent.enabled) parent.clicked()
    }
  }
}
}
"##;

#[cfg(test)]
mod qml_tests {
    use super::QML_SOURCE;

    #[test]
    fn qml_source_smoke_covers_runtime_contract() {
        assert!(QML_SOURCE.contains("PanelWindow"));
        assert!(QML_SOURCE.contains("id: vmListFlickable"));
        assert!(QML_SOURCE.contains("x: vmListFlickable.width - width"));
        assert!(QML_SOURCE.contains("property real panelTopMargin: 24"));
        assert!(QML_SOURCE.contains("property real panelRightMargin: 24"));
        assert!(QML_SOURCE.contains("spacing: 8"));
        assert!(QML_SOURCE.contains("model: vm.quickLaunch || []"));
        assert!(QML_SOURCE.contains("[\"quick-launch\", vm.name, modelData.id]"));
        assert!(QML_SOURCE.contains("function hasCapability(vm, capability)"));
        assert!(QML_SOURCE.contains("root.hasCapability(vm, \"storeVerify\")"));
        assert!(QML_SOURCE.contains("root.canAdvanced(vm, \"switch\")"));
        assert!(QML_SOURCE.contains("root.hasCapability(vm, \"usbHotplug\")"));
        assert!(QML_SOURCE.contains("onExited: (exitCode, exitStatus)"));
        assert!(QML_SOURCE.contains("D2B_WLCONTROL_OBSERVABILITY_ENABLED"));
        assert!(QML_SOURCE.contains("D2B_WLCONTROL_THEME_JSON"));
        assert!(QML_SOURCE.contains("function shellColor(name, fallback)"));
        assert!(QML_SOURCE.contains("root.shellColor(\"surface\", \"#16181d\")"));
        assert!(QML_SOURCE.contains("root.shellColor(\"foreground_strong\", \"#ffffff\")"));
        assert!(QML_SOURCE.contains("root.shellColor(\"muted\", \"#9399b2\")"));
        assert!(!QML_SOURCE.contains("D2B_WLCONTROL_STATE_COLORS"));
        assert!(!QML_SOURCE.contains("D2B_WLCONTROL_ENV_COLORS"));
        assert!(!QML_SOURCE.contains("D2B_WLCONTROL_HOST_ACCENT"));
        assert!(!QML_SOURCE.contains("fallbackStateColors"));
        assert!(!QML_SOURCE.contains("fallbackHostAccent"));
        assert!(!QML_SOURCE.contains("property color accent: \"#89b4fa\""));
        assert!(QML_SOURCE.contains("function stateColor(name)"));
        assert!(QML_SOURCE.contains("return isHexColor(color) ? color : \"transparent\""));
        assert!(QML_SOURCE.contains("function vmBorderColor(vm)"));
        assert!(QML_SOURCE.contains(
            "const active = border && typeof border === \"object\" ? border.active : null"
        ));
        assert!(QML_SOURCE.contains("return isHexColor(active) ? active : \"transparent\""));
        assert!(QML_SOURCE.contains("border.color: root.vmBorderColor(vm)"));
        assert!(!QML_SOURCE.contains("function envAccentColor"));
        assert!(!QML_SOURCE.contains("leftAccent"));
        assert!(!QML_SOURCE.contains("root.envAccentColor"));
        assert!(!QML_SOURCE.contains("vmBorderTheme"));
        assert!(QML_SOURCE.contains("root.stateColor(\"pendingRestart\")"));
        assert!(!QML_SOURCE.contains("if (e === \"work\")"));
        assert!(!QML_SOURCE.contains("if (e === \"personal\")"));
        assert!(QML_SOURCE.contains("id: destructiveControls"));
        assert!(QML_SOURCE.contains("function confirmForceStop(vm)"));
        assert!(QML_SOURCE.contains("[\"force-stop\", vm.name]"));
        assert!(QML_SOURCE.contains("Danger: click Force shutdown again"));
        assert!(QML_SOURCE.contains("Force shutdown is waiting for d2b force-stop support"));
        assert!(QML_SOURCE.contains("Requesting graceful stop"));
        assert!(QML_SOURCE.contains("skipping graceful guest shutdown"));
        assert!(QML_SOURCE.contains("return \"hot mic\""));
        assert!(QML_SOURCE.contains("Speaker is off; turn speaker on to adjust volume"));
        assert!(QML_SOURCE.contains("if (parent.dragging) {"));
        let primary_start = QML_SOURCE
            .find("id: actionButtons")
            .expect("primary controls");
        let destructive_start = QML_SOURCE
            .find("id: destructiveControls")
            .expect("expanded destructive controls");
        assert!(destructive_start > primary_start);
        assert!(!QML_SOURCE[primary_start..destructive_start].contains("force-stop"));
        assert!(!QML_SOURCE.contains("import QtQuick.Controls"));
    }
}
