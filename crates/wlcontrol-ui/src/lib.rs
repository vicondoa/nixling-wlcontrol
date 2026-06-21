//! Quickshell/QML layer-shell frontend launcher.
//!
//! `nixling-wlcontrol` is a Waybar-adjacent desktop shell widget, not a
//! document-style application. The visible frontend is therefore a Quickshell
//! layer-shell popup with explicit Waybar/Catppuccin colours, while Rust remains
//! the backend (`status-json` + `action …`) and the safe process launcher.

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

    let mut child = Command::new("quickshell")
        .arg("--path")
        .arg(&qml_path)
        .arg("--no-duplicate")
        .env("NIXLING_WLCONTROL_BIN", backend)
        .env(
            "NIXLING_WLCONTROL_OBSERVABILITY_ENABLED",
            if config.observability.enabled && config.observability.url.is_some() {
                "1"
            } else {
                "0"
            },
        )
        .env(
            "NIXLING_WLCONTROL_OBSERVABILITY_SUCCESS",
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
    Ok(base.join("nixling-wlcontrol").join("quickshell"))
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
/// - Colours are explicit Catppuccin/Waybar-style tokens.
/// - The panel is a draggable layer-shell overlay anchored near the top-right.
const QML_SOURCE: &str = r##"
//@ pragma StateDir $XDG_STATE_HOME/nixling-wlcontrol/quickshell
//@ pragma IconTheme Adwaita

import QtQuick
import Quickshell
import Quickshell.Io

ShellRoot {
  id: root

  property string backend: Quickshell.env("NIXLING_WLCONTROL_BIN") || "nixling-wlcontrol"
  property var state: ({ connectivity: "daemon-down", role: "none", vms: [] })
  property var usbDevices: []
  property bool busy: false
  property string hoverHint: ""
  property string actionMessage: ""
  property bool actionFailed: false
  property bool actionBusy: false
  property real panelTopMargin: 8
  property real panelRightMargin: 8
  property string confirmKey: ""
  property bool observabilityEnabled: Quickshell.env("NIXLING_WLCONTROL_OBSERVABILITY_ENABLED") === "1"
  property string observabilitySuccess: Quickshell.env("NIXLING_WLCONTROL_OBSERVABILITY_SUCCESS") || "Opened observability portal"

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

  function vmDotColor(vm) {
    if (vm.state === "running") return "#a6e3a1"
    if (vm.state === "starting" || vm.state === "stopping") return "#f9e2af"
    if (vm.pendingRestart) return "#fab387"
    return "#6c7086"
  }

  function envAccentColor(env) {
    // Match waybar group accent colors: work=orange, personal=green, default=blue
    if (!env) return "#89b4fa"
    const e = env.toLowerCase()
    if (e === "work") return "#ffa500"
    if (e === "personal") return "#a6e3a1"
    return "#89b4fa"
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

  function canAdvanced(vm, capability) {
    return canAdminMutate() && vm.state === "running" && hasCapability(vm, capability)
  }

  function canUsb(vm, u) {
    return canAdminMutate() && hasCapability(vm, "usbHotplug") && (!u.ownerVm || u.ownerVm === vm.name)
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
    if (verb === "terminal") return "Opening terminal in " + vm + "..."
    if (verb === "quick-launch") return "Launching " + (args[2] || "command") + " in " + vm + "..."
    if (verb === "build") return "Building " + vm + "..."
    if (verb === "boot") return "Staging " + vm + " for next boot..."
    if (verb === "switch") return "Switching " + vm + "..."
    if (verb === "store-verify") return "Verifying store for " + vm + "..."
    if (verb === "observability") return "Opening observability..."
    if (verb === "restart") return "Restarting " + vm + "..."
    if (verb === "start") return "Starting " + vm + "..."
    if (verb === "stop") return "Stopping " + vm + "..."
    return "Working..."
  }

  function successMessage(args) {
    const verb = args[0] || "action"
    const vm = args[1] || ""
    if (verb === "usb-attach") return "USB attached to " + vm
    if (verb === "usb-detach") return "USB detached from " + vm
    if (verb === "terminal") return "Terminal launch requested for " + vm
    if (verb === "quick-launch") return "Quick launch requested for " + vm
    if (verb === "build") return "Build completed for " + vm
    if (verb === "boot") return "Boot generation staged for " + vm
    if (verb === "switch") return "Switched " + vm
    if (verb === "store-verify") return "Store verified for " + vm
    if (verb === "observability") return observabilitySuccess
    if (verb === "restart") return "Restarted " + vm
    if (verb === "start") return "Started " + vm
    if (verb === "stop") return "Stopped " + vm
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
    if (state.connectivity !== "connected") return state.connectivity === "auth-denied" ? "Authorization denied" : "nixlingd is unreachable"
    if (role === "admin" && state.role !== "admin") return "Requires admin role"
    if (state.role === "none") return "Requires launcher role"
    if (vm && vm.state !== "running") return "VM must be running"
    return "Unavailable"
  }

  function confirmAction(key, message, args) {
    if (confirmKey === key) {
      confirmKey = ""
      confirmTimer.stop()
      action(args)
    } else {
      confirmKey = key
      hoverHint = message
      confirmTimer.restart()
    }
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
    interval: 2200
    repeat: false
    onTriggered: {
      root.confirmKey = ""
      if (root.hoverHint.indexOf("Click again") === 0) root.hoverHint = ""
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
      color: "#0f1117"
      border.color: "#2a2d35"
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
              color: root.state.connectivity === "connected" ? "#1a2e1a" : "#2e1a1a"
              anchors.left: parent.left
              anchors.verticalCenter: parent.verticalCenter
              Text {
                anchors.centerIn: parent
                color: root.state.connectivity === "connected" ? "#a6e3a1" : "#f38ba8"
                font.pixelSize: 11
                font.bold: true
                text: root.state.role || "none"
              }
            }

            Text {
              anchors.centerIn: parent
              width: parent.width - 160
              anchors.verticalCenter: parent.verticalCenter
              color: "#ffffff"
              font.pixelSize: 16
              font.bold: true
              horizontalAlignment: Text.AlignHCenter
              text: "nixling"
            }

            Row {
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              spacing: 4

              IconButton {
                text: "monitoring"
                tooltip: root.observabilityEnabled ? "Open Signoz observability portal" : "Observability URL is not configured"
                accent: "#ffffff"
                enabled: root.observabilityEnabled && !root.busy
                onClicked: root.action(["observability"])
              }
              IconButton {
                text: "refresh"
                tooltip: "Refresh VM status"
                accent: "#ffffff"
                enabled: !root.busy
                onClicked: root.reload()
              }
            }
          }

          Rectangle {
            width: parent.width
            height: 1
            color: "#2a2d35"
          }

          Row {
            width: parent.width
            height: 26
            spacing: 10
            Text {
              color: "#ffffff"
              font.pixelSize: 13
              font.bold: true
              text: root.runningCount() + "/" + root.visibleVms().length + " running"
            }
            Text {
              color: "#9399b2"
              font.pixelSize: 12
              text: root.hoverHint.length > 0 ? root.hoverHint : root.statusText()
            }
          }

          Rectangle {
            visible: root.actionMessage.length > 0 && !root.busy
            width: parent.width
            height: visible ? Math.max(26, actionResult.implicitHeight + 10) : 0
            radius: 10
            color: root.actionFailed ? "#2e1a1a" : "#1a2e1a"
            border.color: root.actionFailed ? "#f38ba8" : "#a6e3a1"
            border.width: 1

            Text {
              id: actionResult
              anchors.fill: parent
              anchors.margins: 6
              color: root.actionFailed ? "#f38ba8" : "#a6e3a1"
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
                  color: "#16181d"
                  border.color: "#2a2d35"
                  border.width: 1
                  clip: true

                  property var vm: modelData
                  property bool expanded: false
                  property bool usbEntryVisible: false
                  property string usbEntryText: ""

                  // Left accent border matching waybar env groups
                  Rectangle {
                    id: leftAccent
                    width: 4
                    height: parent.height
                    x: 0
                    y: 0
                    color: root.envAccentColor(vm.env)
                  }

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
                        tooltip: enabled ? ((vm.state === "running" ? "Stop " : "Start ") + vm.name) : root.disabledReason(vm, "admin", vm.state === "running" ? "stop" : "start")
                        accent: vm.state === "running" ? "#f38ba8" : "#a6e3a1"
                        enabled: vm.state === "running" ? root.canStop(vm) : root.canStart(vm)
                        prominent: true
                        onClicked: {
                          if (vm.state === "running") root.confirmAction("stop:" + vm.name, "Click again to confirm stopping " + vm.name, ["stop", vm.name])
                          else root.action(["start", vm.name])
                        }
                      }
                      IconButton {
                        text: expanded ? "expand_less" : "more_horiz"
                        tooltip: expanded ? "Hide controls" : "More controls"
                        accent: "#ffffff"
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
                        color: "#ffffff"
                        font.pixelSize: 14
                        font.bold: true
                        elide: Text.ElideRight
                        text: vm.name
                      }
                      Text {
                        width: parent.width
                        color: "#9399b2"
                        font.pixelSize: 11
                        elide: Text.ElideRight
                        text: root.vmMeta(vm)
                      }
                    }
                  }

                  Row {
                    id: quickActions
                    width: parent.width
                    spacing: 8
                    IconButton { text: "terminal"; tooltip: enabled ? ("Open a terminal in " + vm.name) : root.disabledReason(vm, "admin", "terminal"); accent: "#ffffff"; enabled: root.canAdvanced(vm, "terminal") && root.state.role === "admin"; onClicked: root.action(["terminal", vm.name]) }
                    Repeater {
                      model: vm.quickLaunch || []
                      IconButton { text: modelData.icon; tooltip: enabled ? modelData.tooltip : root.disabledReason(vm, "admin", "terminal"); accent: "#ffffff"; enabled: root.canAdvanced(vm, "terminal") && root.state.role === "admin"; onClicked: root.action(["quick-launch", vm.name, modelData.id]) }
                    }
                    IconButton { text: "restart_alt"; tooltip: enabled ? ("Restart " + vm.name) : root.disabledReason(vm, "admin", "restart"); accent: "#6c7086"; enabled: root.canAdvanced(vm, "restart"); onClicked: root.confirmAction("restart:" + vm.name, "Click again to confirm restarting " + vm.name, ["restart", vm.name]) }
                    IconButton { text: "verified"; tooltip: enabled ? ("Verify " + vm.name + " store integrity") : root.disabledReason(vm, "admin", "storeVerify"); accent: "#6c7086"; enabled: root.canAdminMutate() && root.hasCapability(vm, "storeVerify"); onClicked: root.action(["store-verify", vm.name]) }
                    IconButton { text: "build"; tooltip: enabled ? ("Build/evaluate " + vm.name + " without activating") : root.disabledReason(vm, "launcher", "build"); accent: "#6c7086"; enabled: root.canMutate() && root.hasCapability(vm, "build"); onClicked: root.action(["build", vm.name]) }
                    IconButton { text: "move_up"; tooltip: enabled ? ("Stage " + vm.name + " for next boot") : root.disabledReason(vm, "admin", "boot"); accent: "#6c7086"; enabled: root.canAdminMutate() && root.hasCapability(vm, "boot"); onClicked: root.action(["boot", vm.name]) }
                    IconButton { text: "sync_alt"; tooltip: enabled ? ("Switch " + vm.name + " generation now") : root.disabledReason(vm, "admin", "switch"); accent: "#6c7086"; enabled: root.canAdvanced(vm, "switch"); onClicked: root.confirmAction("switch:" + vm.name, "Click again to confirm switching " + vm.name, ["switch", vm.name]) }
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
                        accent: "#6c7086"
                        enabled: root.canUsb(vm, modelData)
                        onClicked: root.attachOrPrompt(vmCard, vm, modelData)
                      }
                    }
                    ControlChip {
                      icon: "add"
                      label: "USB"
                      tooltip: enabled ? ("Attach another USB device to " + vm.name) : root.disabledReason(vm, "admin", "usbHotplug")
                      accent: "#6c7086"
                      enabled: root.canAdminMutate() && root.hasCapability(vm, "usbHotplug")
                      onClicked: root.attachOrPrompt(vmCard, vm, ({ busId: "pending", bound: false, ownerVm: null }))
                    }
                    Rectangle {
                      visible: vm.pendingRestart
                      height: 24
                      width: restartText.width + 18
                      radius: 999
                      color: "#2e2a1a"
                      Text { id: restartText; anchors.centerIn: parent; color: "#fab387"; font.pixelSize: 10; font.bold: true; text: "restart" }
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
                          accent: "#6c7086"
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
                      color: "#0d0d0d"
                      border.color: "#2a2d35"
                      border.width: 1

                      TextInput {
                        id: usbEntry
                        anchors.fill: parent
                        anchors.leftMargin: 9
                        anchors.rightMargin: 9
                        color: "#ffffff"
                        selectionColor: "#89b4fa"
                        selectedTextColor: "#000000"
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
                        color: "#6c7086"
                        font.pixelSize: 12
                        text: "USB bus id (e.g. 1-2)"
                      }
                    }

                    ControlChip {
                      icon: "usb"
                      label: "attach"
                      tooltip: "Attach entered USB bus id"
                      accent: "#6c7086"
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
                color: "#0d0d0d"

                Rectangle {
                  width: parent.width
                  radius: 999
                  color: "#6c7086"
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
    property color accent: "#89b4fa"
    property bool prominent: false
    signal clicked()
    width: prominent ? 30 : 26
    height: prominent ? 30 : 26
    radius: width / 2
    opacity: enabled ? 1.0 : 0.28
    border.width: 0
    color: prominent
      ? Qt.rgba(accent.r, accent.g, accent.b, mouse.containsMouse ? 0.22 : 0.15)
      : (mouse.containsMouse ? Qt.rgba(accent.r, accent.g, accent.b, 0.12) : "transparent")

    Text {
      id: label
      anchors.fill: parent
      color: parent.accent
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

  component ControlChip: Rectangle {
    property string icon: ""
    property string label: ""
    property string tooltip: ""
    property color accent: "#89b4fa"
    signal clicked()

    height: 24
    width: chipRow.implicitWidth + 16
    radius: 999
    opacity: enabled ? 1.0 : 0.34
    color: mouse.containsMouse && enabled ? Qt.rgba(accent.r, accent.g, accent.b, 0.15) : Qt.rgba(accent.r, accent.g, accent.b, enabled ? 0.085 : 0.045)
    border.color: Qt.rgba(accent.r, accent.g, accent.b, enabled ? 0.18 : 0.10)
    border.width: 1

    Row {
      id: chipRow
      anchors.centerIn: parent
      spacing: 5
      Text {
        color: parent.parent.accent
        font.family: "Material Symbols Rounded"
        font.pixelSize: 16
        height: parent.parent.height
        horizontalAlignment: Text.AlignHCenter
        verticalAlignment: Text.AlignVCenter
        text: parent.parent.icon
      }
      Text {
        color: parent.parent.accent
        font.pixelSize: 10
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
        assert!(QML_SOURCE.contains("model: vm.quickLaunch || []"));
        assert!(QML_SOURCE.contains("[\"quick-launch\", vm.name, modelData.id]"));
        assert!(QML_SOURCE.contains("function hasCapability(vm, capability)"));
        assert!(QML_SOURCE.contains("root.hasCapability(vm, \"storeVerify\")"));
        assert!(QML_SOURCE.contains("root.canAdvanced(vm, \"switch\")"));
        assert!(QML_SOURCE.contains("root.hasCapability(vm, \"usbHotplug\")"));
        assert!(QML_SOURCE.contains("onExited: (exitCode, exitStatus)"));
        assert!(QML_SOURCE.contains("NIXLING_WLCONTROL_OBSERVABILITY_ENABLED"));
        assert!(!QML_SOURCE.contains("import QtQuick.Controls"));
    }
}
