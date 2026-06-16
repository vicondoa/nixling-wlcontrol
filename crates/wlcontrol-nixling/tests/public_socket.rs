use std::{
    fs,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::ffi::OsStrExt as _,
    },
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

use nix::sys::socket::{
    accept4, bind, listen, recv, send, socket, AddressFamily, Backlog, MsgFlags, SockFlag,
    SockType, UnixAddr,
};
use serde_json::{json, Value};
use wlcontrol_core::{
    model::{AuthRole, Connectivity, RuntimeState, SocketIntent},
    Config, WlError,
};
use wlcontrol_nixling::{wire, NixlingClient};

static NEXT_SOCKET: AtomicUsize = AtomicUsize::new(0);

#[test]
fn refresh_populates_reduce_input_from_public_socket() {
    let server = FakeNixlingd::start(FakeMode::Refresh);
    let client = client_for(server.path());

    let input = client.refresh();

    assert_eq!(input.connectivity, Connectivity::Connected);
    assert_eq!(input.auth.expect("auth").role, AuthRole::Admin);
    let inventory = input.inventory.expect("inventory");
    assert_eq!(inventory.vms.len(), 1);
    assert_eq!(inventory.vms[0].name, "corp-vm");
    assert_eq!(inventory.vms[0].env.as_deref(), Some("work"));
    assert!(inventory.vms[0].features.graphics);
    assert!(inventory.vms[0].features.tpm);
    assert!(inventory.vms[0].features.usbip);
    assert_eq!(inventory.vms[0].static_ip.as_deref(), Some("10.1.0.10"));
    assert_eq!(input.statuses.len(), 1);
    assert_eq!(input.statuses[0].state, RuntimeState::Running);
    assert!(input.statuses[0].pending_restart);
    let usb = input.usb.expect("usb");
    assert_eq!(usb.claims.len(), 1);
    assert!(usb.claims[0].bound);
    assert_eq!(usb.claims[0].owner_vm.as_deref(), Some("corp-vm"));

    server.join();
}

#[test]
fn version_mismatch_rejection_is_protocol_error() {
    let server = FakeNixlingd::start(FakeMode::RejectHello);
    let client = client_for(server.path());

    let error = client.dispatch(&SocketIntent::List).expect_err("rejects");

    assert!(
        matches!(error, WlError::Protocol(_)),
        "expected protocol error, got {error:?}"
    );
    server.join();
}

#[test]
fn absent_socket_reports_daemon_down() {
    let path = unique_socket_path();
    let client = client_for(&path);

    let input = client.refresh();

    assert_eq!(input.connectivity, Connectivity::DaemonDown);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
        if let Some(base) = parent.parent() {
            let _ = fs::remove_dir(base);
        }
    }
}

#[test]
fn vm_start_dispatch_returns_applied_summary() {
    let server = FakeNixlingd::start(FakeMode::VmStartOk);
    let client = client_for(server.path());

    let outcome = client
        .dispatch(&SocketIntent::VmStart {
            vm: "corp-vm".to_owned(),
        })
        .expect("dispatch");

    assert_eq!(outcome.summary, "started corp-vm");
    server.join();
}

#[test]
fn mutating_verb_error_maps_to_wl_error() {
    let server = FakeNixlingd::start(FakeMode::MutatingBrokerError);
    let client = client_for(server.path());

    let error = client
        .dispatch(&SocketIntent::VmStart {
            vm: "corp-vm".to_owned(),
        })
        .expect_err("broker error");

    assert!(matches!(error, WlError::Nixling(message) if message.contains("broker refused")));
    server.join();
}

#[derive(Clone, Copy, Debug)]
enum FakeMode {
    Refresh,
    RejectHello,
    VmStartOk,
    MutatingBrokerError,
}

struct FakeNixlingd {
    path: PathBuf,
    handle: thread::JoinHandle<()>,
}

impl FakeNixlingd {
    fn start(mode: FakeMode) -> Self {
        let path = unique_socket_path();
        let _ = fs::remove_file(&path);
        let fd = socket(
            AddressFamily::Unix,
            SockType::SeqPacket,
            SockFlag::SOCK_CLOEXEC,
            None,
        )
        .expect("socket");
        let addr = UnixAddr::new(path.as_path()).expect("unix addr");
        bind(fd.as_raw_fd(), &addr).expect("bind");
        listen(&fd, Backlog::new(1).expect("backlog")).expect("listen");

        let handle = thread::spawn(move || serve(fd, mode));
        Self { path, handle }
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }

    fn join(self) {
        let Self { path, handle } = self;
        handle.join().expect("fake nixlingd thread");
        let _ = fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir(parent);
            if let Some(base) = parent.parent() {
                let _ = fs::remove_dir(base);
            }
        }
    }
}

fn serve(listener: OwnedFd, mode: FakeMode) {
    let raw = accept4(listener.as_raw_fd(), SockFlag::SOCK_CLOEXEC).expect("accept");
    // SAFETY: accept4 returned a fresh owned file descriptor for this thread.
    let conn = unsafe { OwnedFd::from_raw_fd(raw) };
    let hello = recv_json(&conn).expect("hello");
    assert_eq!(hello.get("type").and_then(Value::as_str), Some("hello"));
    assert_eq!(
        hello.get("clientVersion").and_then(Value::as_str),
        Some(">=0.4.0, <0.5.0")
    );

    if matches!(mode, FakeMode::RejectHello) {
        send_json(
            &conn,
            json!({
                "type": "helloRejected",
                "reason": "versionMismatch",
                "error": {
                    "kind": "wire-version-mismatch",
                    "exitCode": 20,
                    "message": "client/server versions do not overlap",
                    "remediation": "upgrade"
                }
            }),
        )
        .expect("send rejection");
        return;
    }

    send_json(
        &conn,
        json!({
            "type": "helloOk",
            "serverVersion": "0.4.0",
            "selectedVersion": "0.4.0",
            "capabilities": ["typed-errors", "export-broker-audit"]
        }),
    )
    .expect("send helloOk");

    while let Ok(request) = recv_json(&conn) {
        let request_type = request.get("type").and_then(Value::as_str).unwrap_or("");
        let response = match (mode, request_type) {
            (FakeMode::Refresh, "authStatus") => json!({
                "type": "authStatusResponse",
                "auth": {
                    "allowedSubcommands": ["list", "status"],
                    "deniedSubcommands": [],
                    "role": "admin",
                    "sockets": []
                }
            }),
            (FakeMode::Refresh, "list") => json!({
                "type": "listResponse",
                "vms": [{
                    "name": "corp-vm",
                    "env": "work",
                    "graphics": true,
                    "tpm": true,
                    "usbip": true,
                    "staticIp": "10.1.0.10",
                    "status": "running",
                    "isNetVm": false
                }]
            }),
            (FakeMode::Refresh, "status") => json!({
                "type": "statusResponse",
                "status": {
                    "vm": "corp-vm",
                    "lifecycle": {
                        "pendingRestart": true,
                        "state": "Running"
                    },
                    "runtime": { "detail": "running" },
                    "readiness": ["guest-control-health"]
                }
            }),
            (FakeMode::Refresh, "usbipProbe") => json!({
                "type": "usbipProbeResponse",
                "entries": [{
                    "vm": "corp-vm",
                    "env": "work",
                    "busId": "1-2",
                    "lockPath": "/run/nixling/usbip/corp-vm-1-2.lock",
                    "status": "bound",
                    "ownerVm": "corp-vm"
                }]
            }),
            (FakeMode::VmStartOk, "vmStart") => {
                assert_eq!(request.get("apply").and_then(Value::as_bool), Some(true));
                assert_eq!(request.get("vm").and_then(Value::as_str), Some("corp-vm"));
                json!({
                    "type": "mutatingVerbResponse",
                    "verb": "vm start",
                    "outcome": "applied",
                    "summary": "started corp-vm"
                })
            }
            (FakeMode::MutatingBrokerError, "vmStart") => json!({
                "type": "mutatingVerbResponse",
                "verb": "vm start",
                "outcome": "broker-error",
                "summary": "broker refused",
                "remediation": "inspect daemon logs"
            }),
            (_, other) => json!({
                "type": "error",
                "error": {
                    "kind": "wire-unsupported-request",
                    "exitCode": 24,
                    "message": format!("unexpected request {other}"),
                    "remediation": "fix the test"
                }
            }),
        };
        send_json(&conn, response).expect("send response");
    }
}

fn recv_json(fd: &OwnedFd) -> std::io::Result<Value> {
    let mut buffer = vec![0_u8; wire::MAX_FRAME_BYTES + 4];
    let received = recv(fd.as_raw_fd(), &mut buffer, MsgFlags::empty())
        .map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))?;
    if received == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "socket closed",
        ));
    }
    let payload = wire::decode_frame(&buffer[..received])
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))?;
    serde_json::from_slice(payload)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
}

fn send_json(fd: &OwnedFd, value: Value) -> std::io::Result<()> {
    let payload = serde_json::to_vec(&value)?;
    let frame = wire::encode_frame(&payload)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))?;
    let sent = send(fd.as_raw_fd(), &frame, MsgFlags::empty())
        .map_err(|errno| std::io::Error::from_raw_os_error(errno as i32))?;
    if sent == frame.len() {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "short seqpacket write",
        ))
    }
}

fn client_for(path: &Path) -> NixlingClient {
    NixlingClient::new(&Config {
        public_socket: path.display().to_string(),
        command_timeout_ms: 1000,
        ..Default::default()
    })
}

fn unique_socket_path() -> PathBuf {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".s");
    let id = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    let dir = base.join(format!("s{}-{id}", process::id()));
    fs::create_dir_all(&dir).expect("socket dir");
    let path = dir.join("x.sock");
    assert!(
        path.as_os_str().as_bytes().len() < 100,
        "socket path must fit sockaddr_un"
    );
    path
}
