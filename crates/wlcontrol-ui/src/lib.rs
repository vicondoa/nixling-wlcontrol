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
pub fn open(_config: &Config) -> WlResult<()> {
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
/// - The panel is a fixed-size layer-shell overlay in the top-right.
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

  function visibleVms() {
    const vms = state.vms || []
    return vms.filter(v => !v.isNetVm && !v.hidden)
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
    if (!busy && actionMessage === "done") actionMessage = ""
    statusProc.exec([backend, "status-json"])
  }

  function reloadUsbDevices() {
    usbDevicesProc.exec([backend, "usb-devices-json"])
  }

  function action(args) {
    busy = true
    actionMessage = "running " + args.join(" ")
    actionFailed = false
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

  function canStart(vm) {
    return canMutate() && vm.state !== "running"
  }

  function canStop(vm) {
    return canMutate() && vm.state === "running"
  }

  function canAdvanced(vm) {
    return canMutate() && vm.state === "running"
  }

  function canUsb(vm, u) {
    return canMutate() && (!u.ownerVm || u.ownerVm === vm.name)
  }

  function usbLabel(u) {
    if (u.ownerVm && u.ownerVm !== u.vm) return "owned " + u.ownerVm
    if (u.busId === "pending") return u.bound ? "detach USB" : "attach USB"
    return (u.bound ? "detach " : "attach ") + u.busId
  }

  function usbTooltip(vm, u) {
    if (u.ownerVm && u.ownerVm !== vm.name) return "USB " + u.busId + " is owned by " + u.ownerVm
    return (u.bound ? "Detach USB " : "Attach USB ") + u.busId
  }

  function shortDeviceLabel(device) {
    const product = device.product || device.label || "USB device"
    const name = product.length > 18 ? product.substring(0, 17) + "…" : product
    return name + " " + device.busId
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
    onExited: root.busy = false
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
    stdout: StdioCollector {
      onStreamFinished: actionProc.out = this.text.trim()
    }
    stderr: StdioCollector {
      onStreamFinished: actionProc.err = this.text.trim()
    }
    onExited: {
      if (actionProc.err.length > 0) {
        root.actionFailed = true
        root.actionMessage = actionProc.err
      } else if (actionProc.out.length > 0) {
        root.actionFailed = false
        root.actionMessage = actionProc.out
      } else {
        root.actionFailed = false
        root.actionMessage = "done"
      }
      actionProc.out = ""
      actionProc.err = ""
      root.reload()
    }
  }

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
    width: 420
    height: 620
    color: "transparent"
    surfaceFormat { opaque: false }

    anchors { top: true; right: true }
    margins { top: 8; right: 8 }

    Rectangle {
      anchors.fill: parent
      radius: 18
      color: "#1e1e2e"
      border.color: "#45475a"
      border.width: 1
      clip: true

      Column {
        anchors.fill: parent
        anchors.margins: 16
        spacing: 12

          Row {
            width: parent.width
            height: 32
            spacing: 10

            Rectangle {
              width: 66
              height: 22
              radius: 999
              color: root.state.connectivity === "connected" ? "#1f3f2c" : "#4a1f2a"
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
              width: parent.width - 116
              anchors.verticalCenter: parent.verticalCenter
              color: "#cdd6f4"
              font.pixelSize: 16
              font.bold: true
              horizontalAlignment: Text.AlignHCenter
              text: "nixling VMs"
            }

            Text {
              width: 34
              anchors.verticalCenter: parent.verticalCenter
              color: "#89b4fa"
              font.pixelSize: 18
              horizontalAlignment: Text.AlignRight
              text: "↻"
              MouseArea { anchors.fill: parent; onClicked: root.reload() }
            }
          }

          Rectangle {
            width: parent.width
            height: 1
            color: "#313244"
          }

          Row {
            width: parent.width
            height: 26
            spacing: 10
            Text {
              color: "#cdd6f4"
              font.pixelSize: 13
              font.bold: true
              text: root.runningCount() + "/" + root.visibleVms().length + " running"
            }
            Text {
              color: "#bac2de"
              font.pixelSize: 12
              text: root.hoverHint.length > 0 ? root.hoverHint : root.statusText()
            }
          }

          Rectangle {
            visible: root.actionMessage.length > 0 && !root.busy
            width: parent.width
            height: visible ? Math.max(26, actionResult.implicitHeight + 10) : 0
            radius: 10
            color: root.actionFailed ? "#4a1f2a" : "#1f3f2c"
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

          Flickable {
            width: parent.width
            height: parent.height - 84 - ((root.actionMessage.length > 0 && !root.busy) ? 36 : 0)
            contentWidth: width
            contentHeight: list.height
            clip: true

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
                  color: "#313244"
                  border.color: "#45475a"
                  border.width: 1

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
                        tooltip: (vm.state === "running" ? "Stop " : "Start ") + vm.name
                        accent: vm.state === "running" ? "#f38ba8" : "#a6e3a1"
                        enabled: vm.state === "running" ? root.canStop(vm) : root.canStart(vm)
                        prominent: true
                        onClicked: root.action([vm.state === "running" ? "stop" : "start", vm.name])
                      }
                      IconButton {
                        text: expanded ? "expand_less" : "more_horiz"
                        tooltip: expanded ? "Hide controls" : "More controls"
                        accent: "#89b4fa"
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
                        color: "#cdd6f4"
                        font.pixelSize: 14
                        font.bold: true
                        elide: Text.ElideRight
                        text: vm.name
                      }
                      Text {
                        width: parent.width
                        color: "#a6adc8"
                        font.pixelSize: 11
                        elide: Text.ElideRight
                        text: root.vmMeta(vm)
                      }
                    }
                  }

                  Flow {
                    id: usbControls
                    visible: (vm.usb || []).length > 0
                    width: parent.width
                    spacing: 6
                    Repeater {
                      model: vm.usb || []
                      ControlChip {
                        icon: modelData.bound ? "usb_off" : "usb"
                        label: root.usbLabel(modelData)
                        tooltip: root.usbTooltip(vm, modelData)
                        accent: "#94e2d5"
                        enabled: root.canUsb(vm, modelData)
                        onClicked: root.attachOrPrompt(vmCard, vm, modelData)
                      }
                    }
                    Rectangle {
                      visible: vm.pendingRestart
                      height: 24
                      width: restartText.width + 18
                      radius: 999
                      color: "#4a3223"
                      Text { id: restartText; anchors.centerIn: parent; color: "#fab387"; font.pixelSize: 10; font.bold: true; text: "restart" }
                    }
                  }

                  Row {
                    visible: usbEntryVisible
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
                          accent: "#94e2d5"
                          enabled: root.canMutate()
                          onClicked: root.action(["usb-attach", vm.name, modelData.busId])
                        }
                      }
                    }
                  }

                  Row {
                    visible: usbEntryVisible
                    width: parent.width
                    height: visible ? 30 : 0
                    spacing: 6

                    Rectangle {
                      width: parent.width - 86
                      height: 28
                      radius: 8
                      color: "#181825"
                      border.color: "#45475a"
                      border.width: 1

                      TextInput {
                        id: usbEntry
                        anchors.fill: parent
                        anchors.leftMargin: 9
                        anchors.rightMargin: 9
                        color: "#cdd6f4"
                        selectionColor: "#89b4fa"
                        selectedTextColor: "#11111b"
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
                      accent: "#94e2d5"
                      enabled: usbEntryText.length > 0 && root.canMutate()
                      onClicked: root.action(["usb-attach", vm.name, usbEntryText])
                    }
                  }

                  Row {
                    id: details
                    visible: expanded
                    width: parent.width
                    spacing: 8
                    ControlChip { icon: "terminal"; label: "terminal"; tooltip: root.state.role === "admin" ? "Open terminal" : "Requires admin role"; accent: "#cba6f7"; enabled: root.canAdvanced(vm) && root.state.role === "admin"; onClicked: root.action(["terminal", vm.name]) }
                    ControlChip { icon: "restart_alt"; label: "restart"; tooltip: "Restart VM"; accent: "#fab387"; enabled: root.canAdvanced(vm); onClicked: root.action(["restart", vm.name]) }
                    ControlChip { icon: "verified"; label: "verify"; tooltip: "Verify store"; accent: "#f9e2af"; enabled: root.canMutate(); onClicked: root.action(["store-verify", vm.name]) }
                    ControlChip { icon: "sync_alt"; label: "switch"; tooltip: "Switch VM generation"; accent: "#89b4fa"; enabled: root.canAdvanced(vm); onClicked: root.action(["switch", vm.name]) }
                  }
                }

                MouseArea {
                  anchors.fill: parent
                  acceptedButtons: Qt.RightButton
                  onClicked: expanded = !expanded
                }
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
    radius: prominent ? 10 : 8
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
      onContainsMouseChanged: root.hoverHint = containsMouse ? parent.tooltip : ""
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
"##;
