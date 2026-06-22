use std::{
    fs, io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, OwnedFd},
        unix::ffi::OsStrExt as _,
    },
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Duration,
};

use nix::sys::socket::{
    accept4, bind, connect, listen, recv, send, socket, AddressFamily, Backlog, MsgFlags, SockFlag,
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
    assert!(!inventory.vms[0].features.graphics);
    assert!(!inventory.vms[0].features.tpm);
    assert!(!inventory.vms[0].features.usbip);
    assert!(!inventory.vms[0].features.audio);
    assert_eq!(inventory.vms[0].static_ip.as_deref(), Some("10.1.0.10"));
    assert!(inventory.vms[0].capabilities.usb_hotplug);
    assert!(!inventory.vms[0].capabilities.terminal);
    assert_eq!(input.statuses.len(), 1);
    assert_eq!(input.statuses[0].state, RuntimeState::Running);
    assert!(input.statuses[0].pending_restart);
    assert!(!input.statuses[0].capabilities.store_verify);
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
    cleanup_socket_path(&path);
}

#[test]
fn broker_socket_guard_rejects_priv_sock_before_connect() {
    let client = NixlingClient::new(&Config {
        public_socket: "/run/nixling/priv.sock".to_owned(),
        command_timeout_ms: 100,
        ..Default::default()
    });

    let error = client.dispatch(&SocketIntent::List).expect_err("guarded");

    assert!(
        matches!(error, WlError::Config(ref message) if message.contains("privileged nixling broker socket")),
        "expected config guard error, got {error:?}"
    );
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
fn boot_dispatch_returns_applied_summary() {
    let server = FakeNixlingd::start(FakeMode::BootOk);
    let client = client_for(server.path());

    let outcome = client
        .dispatch(&SocketIntent::Boot {
            vm: "corp-vm".to_owned(),
        })
        .expect("dispatch");

    assert_eq!(outcome.summary, "staged corp-vm for next boot");
    server.join();
}

#[test]
fn mutating_non_applied_outcomes_map_to_typed_errors() {
    let cases = [
        (
            FakeMode::MutatingDryRunPlanned,
            ExpectedError::Nixling,
            ["dry-run-planned", "planned only", "use --apply"],
        ),
        (
            FakeMode::MutatingApiReadyTimeout,
            ExpectedError::Timeout,
            ["api-ready-timeout", "api not ready", "inspect guest"],
        ),
        (
            FakeMode::MutatingNotYetImplemented,
            ExpectedError::Nixling,
            ["not-yet-implemented", "not implemented", "upgrade nixling"],
        ),
        (
            FakeMode::MutatingBrokerError,
            ExpectedError::Nixling,
            ["broker-error", "broker refused", "inspect daemon logs"],
        ),
        (
            FakeMode::MutatingInvalidRequest,
            ExpectedError::Protocol,
            ["invalid-request", "bad vm", "choose a declared VM"],
        ),
    ];

    for (mode, expected, needles) in cases {
        let server = FakeNixlingd::start(mode);
        let client = client_for(server.path());

        let error = client
            .dispatch(&SocketIntent::VmStart {
                vm: "corp-vm".to_owned(),
            })
            .expect_err("non-applied outcome must fail");

        expected.assert_matches(&error);
        let rendered = error.to_string();
        for needle in needles {
            assert!(
                rendered.contains(needle),
                "{rendered:?} did not contain {needle:?}"
            );
        }
        server.join();
    }
}

#[test]
fn usb_probe_mutating_response_maps_to_wl_error() {
    let server = FakeNixlingd::start(FakeMode::UsbProbeBrokerError);
    let client = client_for(server.path());

    let error = client
        .dispatch(&SocketIntent::UsbProbe)
        .expect_err("usb probe broker error");

    assert!(matches!(error, WlError::Nixling(ref message) if message.contains("broker refused")));
    server.join();
}

#[test]
fn accept_then_stall_degrades_refresh_and_times_out_dispatch() {
    let server = FakeNixlingd::start(FakeMode::AcceptThenStall);
    let client = client_for_timeout(server.path(), 100);

    let input = client.refresh();

    assert_degraded_refresh(&input);
    server.join();

    let server = FakeNixlingd::start(FakeMode::AcceptThenStall);
    let client = client_for_timeout(server.path(), 100);

    let error = client.dispatch(&SocketIntent::List).expect_err("timeout");

    assert!(
        matches!(error, WlError::Timeout(_)),
        "expected timeout, got {error:?}"
    );
    server.join();
}

#[test]
fn close_after_hello_degrades_refresh_and_reports_daemon_down() {
    let server = FakeNixlingd::start(FakeMode::CloseAfterHello);
    let client = client_for_timeout(server.path(), 100);

    let input = client.refresh();

    assert_degraded_refresh(&input);
    server.join();

    let server = FakeNixlingd::start(FakeMode::CloseAfterHello);
    let client = client_for_timeout(server.path(), 100);

    let error = client.dispatch(&SocketIntent::List).expect_err("closed");

    assert!(
        matches!(error, WlError::DaemonDown(_)),
        "expected daemon down, got {error:?}"
    );
    server.join();
}

#[test]
fn post_auth_request_failure_degrades_refresh_not_false_healthy() {
    // Regression (panel W1fu2): auth succeeds but the inventory request then
    // fails on a closed connection. refresh() MUST degrade to daemon-down
    // rather than returning a false-healthy "Connected with zero VMs"
    // snapshot built from the successful auth alone.
    let server = FakeNixlingd::start(FakeMode::RefreshAuthThenClose);
    let client = client_for_timeout(server.path(), 100);

    let input = client.refresh();

    assert_degraded_refresh(&input);
    server.join();
}

#[test]
fn invalid_json_degrades_refresh_and_is_protocol_error_on_dispatch() {
    let server = FakeNixlingd::start(FakeMode::InvalidJson);
    let client = client_for_timeout(server.path(), 100);

    let input = client.refresh();

    assert_degraded_refresh(&input);
    server.join();

    let server = FakeNixlingd::start(FakeMode::InvalidJson);
    let client = client_for_timeout(server.path(), 100);

    let error = client
        .dispatch(&SocketIntent::List)
        .expect_err("invalid json");

    assert!(
        matches!(error, WlError::Protocol(_)),
        "expected protocol error, got {error:?}"
    );
    server.join();
}

#[test]
fn malformed_frames_degrade_refresh_and_are_protocol_errors_on_dispatch() {
    for mode in [FakeMode::LengthMismatchFrame, FakeMode::OversizedFrame] {
        let server = FakeNixlingd::start(mode);
        let client = client_for_timeout(server.path(), 100);

        let input = client.refresh();

        assert_degraded_refresh(&input);
        server.join();

        let server = FakeNixlingd::start(mode);
        let client = client_for_timeout(server.path(), 100);

        let error = client.dispatch(&SocketIntent::List).expect_err("bad frame");

        assert!(
            matches!(error, WlError::Protocol(_)),
            "expected protocol error for {mode:?}, got {error:?}"
        );
        server.join();
    }
}

#[test]
fn malformed_frame_server_tolerates_client_closing_after_hello() {
    for mode in [
        FakeMode::AcceptThenStall,
        FakeMode::LengthMismatchFrame,
        FakeMode::OversizedFrame,
    ] {
        let server = FakeNixlingd::start(mode);
        send_hello_then_close(server.path());
        server.join();
    }
}

#[derive(Clone, Copy, Debug)]
enum ExpectedError {
    Nixling,
    Protocol,
    Timeout,
}

impl ExpectedError {
    fn assert_matches(self, error: &WlError) {
        match self {
            Self::Nixling => assert!(
                matches!(error, WlError::Nixling(_)),
                "expected nixling error, got {error:?}"
            ),
            Self::Protocol => assert!(
                matches!(error, WlError::Protocol(_)),
                "expected protocol error, got {error:?}"
            ),
            Self::Timeout => assert!(
                matches!(error, WlError::Timeout(_)),
                "expected timeout error, got {error:?}"
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FakeMode {
    Refresh,
    RejectHello,
    VmStartOk,
    BootOk,
    MutatingDryRunPlanned,
    MutatingApiReadyTimeout,
    MutatingNotYetImplemented,
    MutatingBrokerError,
    MutatingInvalidRequest,
    UsbProbeBrokerError,
    AcceptThenStall,
    CloseAfterHello,
    /// Answer `authStatus` successfully, then close the next connection
    /// (the inventory request) before responding — exercising a transport
    /// failure that happens AFTER auth succeeds.
    RefreshAuthThenClose,
    InvalidJson,
    LengthMismatchFrame,
    OversizedFrame,
}

impl FakeMode {
    fn allows_early_client_close(self) -> bool {
        matches!(
            self,
            Self::AcceptThenStall | Self::LengthMismatchFrame | Self::OversizedFrame
        )
    }
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
        listen(&fd, Backlog::new(8).expect("backlog")).expect("listen");

        let handle = thread::spawn(move || serve(fd, mode));
        Self { path, handle }
    }

    fn path(&self) -> &PathBuf {
        &self.path
    }

    fn join(self) {
        let Self { path, handle } = self;
        handle.join().expect("fake nixlingd thread");
        cleanup_socket_path(&path);
    }
}

fn serve(listener: OwnedFd, mode: FakeMode) {
    if mode == FakeMode::Refresh {
        for expected in ["authStatus", "list", "status", "usbipProbe"] {
            serve_connection(&listener, mode, Some(expected));
        }
    } else if mode == FakeMode::RefreshAuthThenClose {
        // Connection 1: answer authStatus like a healthy Refresh.
        serve_connection(&listener, FakeMode::Refresh, Some("authStatus"));
        // Connection 2 (inventory): handshake, then close before responding.
        serve_connection(&listener, FakeMode::CloseAfterHello, None);
    } else {
        serve_connection(&listener, mode, None);
    }
}

fn serve_connection(listener: &OwnedFd, mode: FakeMode, expected_request: Option<&str>) {
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

    if let Err(err) = send_json(
        &conn,
        json!({
            "type": "helloOk",
            "serverVersion": "0.4.0",
            "selectedVersion": "0.4.0",
            "capabilities": ["typed-errors", "export-broker-audit"]
        }),
    ) {
        if mode.allows_early_client_close() && err.kind() == io::ErrorKind::BrokenPipe {
            return;
        }
        panic!("send helloOk: {err}");
    }

    match mode {
        FakeMode::AcceptThenStall => {
            thread::sleep(Duration::from_millis(250));
            return;
        }
        FakeMode::CloseAfterHello => return,
        _ => {}
    }

    let request = recv_json(&conn).expect("request");
    let request_type = request.get("type").and_then(Value::as_str).unwrap_or("");
    if let Some(expected) = expected_request {
        assert_eq!(request_type, expected);
    }
    assert_request_shape(&request, request_type);

    match mode {
        FakeMode::InvalidJson => send_payload(&conn, b"{not json").expect("send invalid json"),
        FakeMode::LengthMismatchFrame => {
            send_raw(&conn, &[10, 0, 0, 0, b'{', b'}']).expect("send short frame");
        }
        FakeMode::OversizedFrame => {
            let len = (wire::MAX_FRAME_BYTES as u32 + 1).to_le_bytes();
            send_raw(&conn, &len).expect("send oversized frame");
        }
        _ => send_json(&conn, response_for(mode, request_type)).expect("send response"),
    }
}

fn assert_request_shape(request: &Value, request_type: &str) {
    match request_type {
        "authStatus" => assert_eq!(request, &json!({ "type": "authStatus" })),
        "list" => assert_eq!(request, &json!({ "type": "list", "env": null, "vm": null })),
        "status" => assert_eq!(
            request,
            &json!({ "type": "status", "vm": "corp-vm", "checkBridges": false })
        ),
        "usbipProbe" => assert_eq!(request, &json!({ "type": "usbipProbe" })),
        "vmStart" => assert_eq!(
            request,
            &json!({
                "type": "vmStart",
                "vm": "corp-vm",
                "dryRun": false,
                "apply": true,
                "json": true
            })
        ),
        "boot" => assert_eq!(
            request,
            &json!({
                "type": "boot",
                "vm": "corp-vm",
                "dryRun": false,
                "apply": true,
                "json": true
            })
        ),
        other => panic!("unexpected request type {other}"),
    }
}

fn response_for(mode: FakeMode, request_type: &str) -> Value {
    match (mode, request_type) {
        (FakeMode::Refresh, "authStatus") => json!({
            "type": "authStatusResponse",
            "allowedSubcommands": ["list", "status", "usb probe"],
            "deniedSubcommands": [],
            "role": "admin",
            "sockets": []
        }),
        (FakeMode::Refresh, "list") => json!({
            "type": "listResponse",
            "vms": [{
                "env": "work",
                "lifecycle": {
                    "pendingRestart": false,
                    "state": "Stopped"
                },
                "runtime": {
                    "detail": "stopped",
                    "operationCapabilities": {
                        "media": { "usbHotplug": true },
                        "guest": { "exec": false },
                        "storage": { "storeSync": false }
                    }
                },
                "sshUser": "alice",
                "vm": "corp-vm"
            }]
        }),
        (FakeMode::Refresh, "status") => json!({
            "type": "statusResponse",
            "status": {
                "bridgeChecks": [],
                "env": "work",
                "lifecycle": {
                    "pendingRestart": true,
                    "state": "Running"
                },
                "runtime": {
                    "detail": "running",
                    "operationCapabilities": {
                        "lifecycle": { "switch": false },
                        "media": { "usbHotplug": true },
                        "guest": { "exec": false },
                        "storage": { "storeSync": false }
                    }
                },
                "sshUser": "alice",
                "staticIp": "10.1.0.10",
                "vm": "corp-vm"
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
        (FakeMode::VmStartOk, "vmStart") => mutating_response("applied", "started corp-vm", ""),
        (FakeMode::BootOk, "boot") => {
            mutating_response("applied", "staged corp-vm for next boot", "")
        }
        (FakeMode::MutatingDryRunPlanned, "vmStart") => {
            mutating_response("dry-run-planned", "planned only", "use --apply")
        }
        (FakeMode::MutatingApiReadyTimeout, "vmStart") => {
            mutating_response("api-ready-timeout", "api not ready", "inspect guest")
        }
        (FakeMode::MutatingNotYetImplemented, "vmStart") => {
            mutating_response("not-yet-implemented", "not implemented", "upgrade nixling")
        }
        (FakeMode::MutatingBrokerError, "vmStart") => {
            mutating_response("broker-error", "broker refused", "inspect daemon logs")
        }
        (FakeMode::MutatingInvalidRequest, "vmStart") => {
            mutating_response("invalid-request", "bad vm", "choose a declared VM")
        }
        (FakeMode::UsbProbeBrokerError, "usbipProbe") => {
            mutating_response("broker-error", "broker refused", "inspect daemon logs")
        }
        (_, other) => json!({
            "type": "error",
            "error": {
                "kind": "wire-unsupported-request",
                "exitCode": 24,
                "message": format!("unexpected request {other}"),
                "remediation": "fix the test"
            }
        }),
    }
}

fn mutating_response(outcome: &str, summary: &str, remediation: &str) -> Value {
    let mut value = json!({
        "type": "mutatingVerbResponse",
        "verb": "vm start",
        "outcome": outcome,
        "summary": summary
    });
    if !remediation.is_empty() {
        value["remediation"] = Value::String(remediation.to_owned());
    }
    value
}

fn recv_json(fd: &OwnedFd) -> io::Result<Value> {
    let mut buffer = vec![0_u8; wire::MAX_FRAME_BYTES + 4];
    let received = recv(fd.as_raw_fd(), &mut buffer, MsgFlags::empty())
        .map_err(|errno| io::Error::from_raw_os_error(errno as i32))?;
    if received == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "socket closed",
        ));
    }
    let payload = wire::decode_frame(&buffer[..received])
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    serde_json::from_slice(payload).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn send_json(fd: &OwnedFd, value: Value) -> io::Result<()> {
    let payload = serde_json::to_vec(&value)?;
    send_payload(fd, &payload)
}

fn send_hello_then_close(path: &Path) {
    let fd = socket(
        AddressFamily::Unix,
        SockType::SeqPacket,
        SockFlag::SOCK_CLOEXEC,
        None,
    )
    .expect("socket");
    let addr = UnixAddr::new(path).expect("unix addr");
    connect(fd.as_raw_fd(), &addr).expect("connect");
    send_json(
        &fd,
        json!({
            "type": "hello",
            "clientVersion": ">=0.4.0, <0.5.0"
        }),
    )
    .expect("send hello");
}

fn send_payload(fd: &OwnedFd, payload: &[u8]) -> io::Result<()> {
    let frame = wire::encode_frame(payload)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    send_raw(fd, &frame)
}

fn send_raw(fd: &OwnedFd, frame: &[u8]) -> io::Result<()> {
    let sent = send(fd.as_raw_fd(), frame, MsgFlags::empty())
        .map_err(|errno| io::Error::from_raw_os_error(errno as i32))?;
    if sent == frame.len() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short seqpacket write",
        ))
    }
}

fn assert_degraded_refresh(input: &wlcontrol_core::sources::ReduceInput) {
    assert_eq!(input.connectivity, Connectivity::DaemonDown);
    assert!(input.auth.is_none());
    assert!(input.inventory.is_none());
    assert!(input.statuses.is_empty());
    assert!(input.usb.is_none());
}

fn client_for(path: &Path) -> NixlingClient {
    client_for_timeout(path, 1000)
}

fn client_for_timeout(path: &Path, command_timeout_ms: u64) -> NixlingClient {
    NixlingClient::new(&Config {
        public_socket: path.display().to_string(),
        command_timeout_ms,
        ..Default::default()
    })
}

fn unique_socket_path() -> PathBuf {
    let base = std::env::temp_dir().join("wlc-ps");
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

fn cleanup_socket_path(path: &Path) {
    let _ = fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
        if let Some(base) = parent.parent() {
            let _ = fs::remove_dir(base);
        }
    }
}
