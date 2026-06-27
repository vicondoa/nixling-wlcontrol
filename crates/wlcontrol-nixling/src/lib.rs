//! Direct nixlingd public-socket client.
//!
//! Owning wave: **Wave 1 — Protocol client agent**. Wave 0 fixes the public
//! API surface (this module's signatures) so the Waybar / GTK / CLI crates can
//! build against a stable seam. The Wave 1 agent implements the real protocol:
//!
//! - connect to the non-abstract `SOCK_SEQPACKET` socket at the configured path;
//! - send the `Hello` negotiation frame and enforce the selected version range;
//! - length-prefix (4-byte little-endian) every JSON request frame;
//! - read bounded responses and map `PublicResponse::Error` /
//!   `MutatingVerbResponse` into typed [`WlError`] values;
//! - translate raw nixling wire JSON into the neutral [`ReduceInput`] fragments.
//!
//! The protocol/transport details live in [`wire`]; high-level intents live on
//! [`NixlingClient`].

use std::{path::Path, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use wlcontrol_core::error::{WlError, WlResult};
use wlcontrol_core::model::{
    AuthRole, Connectivity, RuntimeState, SocketIntent, UsbClaim, VmCapabilities,
};
use wlcontrol_core::sources::{Auth, Inventory, InventoryVm, ReduceInput, UsbProbe, VmStatus};
use wlcontrol_core::Config;

mod transport;
pub mod wire;

use transport::SeqpacketTransport;

const CLIENT_VERSION_RANGE: &str = ">=0.4.0, <0.5.0";
const CLIENT_FEATURES: &[&str] = &["typed-errors", "export-broker-audit"];

/// Outcome of a single dispatched mutating intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchOutcome {
    /// Human-facing one-line summary suitable for the UI.
    pub summary: String,
}

/// A connected (or connectable) nixlingd public-socket client.
///
/// The client is cheap to construct and does not hold a persistent connection;
/// each call connects, negotiates, performs the request, and closes. This keeps
/// the daemon-down/auth-denied posture observable on every refresh.
#[derive(Debug, Clone)]
pub struct NixlingClient {
    socket_path: String,
    timeout: Duration,
}

impl NixlingClient {
    /// Build a client from user configuration.
    pub fn new(config: &Config) -> Self {
        Self {
            socket_path: config.public_socket.clone(),
            timeout: Duration::from_millis(config.command_timeout_ms),
        }
    }

    /// The configured public-socket path.
    pub fn socket_path(&self) -> &str {
        &self.socket_path
    }

    /// The per-operation timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Collect one full refresh bundle from the daemon's fast status model.
    ///
    pub fn refresh(&self) -> ReduceInput {
        match self.try_refresh() {
            Ok(input) => input,
            Err(WlError::Denied(_)) => ReduceInput {
                connectivity: Connectivity::Connected,
                auth: Some(Auth {
                    role: AuthRole::None,
                }),
                ..Default::default()
            },
            Err(_) => ReduceInput {
                connectivity: Connectivity::DaemonDown,
                ..Default::default()
            },
        }
    }

    /// Fallible refresh variant useful for tests and diagnostics.
    pub fn try_refresh(&self) -> WlResult<ReduceInput> {
        let auth = self.request(request_auth_status)?;

        // After auth succeeds, one unfiltered status read gives wlcontrol the
        // daemon-maintained fast model. Do not run per-VM status or deep USB
        // probe calls on the UI refresh path.
        let status = self.request(request_status_model)?;

        Ok(ReduceInput {
            connectivity: Connectivity::Connected,
            auth: Some(auth),
            inventory: Some(status.inventory),
            statuses: status.statuses,
            usb: Some(status.usb),
        })
    }

    /// Dispatch a single typed socket intent (`vm start`, `usb attach`, ...).
    pub fn dispatch(&self, intent: &SocketIntent) -> WlResult<DispatchOutcome> {
        let transport = self.connect_and_handshake()?;
        match intent {
            SocketIntent::List => {
                let inventory = request_inventory(&transport)?;
                Ok(DispatchOutcome {
                    summary: format!("listed {} VM(s)", inventory.vms.len()),
                })
            }
            SocketIntent::Status { vm } => {
                let status = request_status(&transport, vm)?;
                Ok(DispatchOutcome {
                    summary: format!("{} status: {:?}", status.name, status.state),
                })
            }
            SocketIntent::AuthStatus => {
                let auth = request_auth_status(&transport)?;
                Ok(DispatchOutcome {
                    summary: format!("auth role: {:?}", auth.role),
                })
            }
            SocketIntent::UsbProbe => {
                let probe = request_usb_probe(&transport)?;
                Ok(DispatchOutcome {
                    summary: format!("probed {} USB claim(s)", probe.claims.len()),
                })
            }
            SocketIntent::VmStart { vm } => dispatch_mutating(
                &transport,
                "vmStart",
                json_values([
                    ("vm", Value::String(vm.clone())),
                    ("noWaitApi", Value::Bool(true)),
                ]),
            ),
            SocketIntent::VmStop { vm, force } => {
                dispatch_mutating(&transport, "vmStop", vm_stop_fields(vm, *force))
            }
            SocketIntent::VmRestart { vm } => dispatch_mutating(
                &transport,
                "vmRestart",
                json_values([
                    ("vm", Value::String(vm.clone())),
                    ("noWaitApi", Value::Bool(true)),
                ]),
            ),
            SocketIntent::Switch { vm } => {
                dispatch_mutating(&transport, "switch", json_object([("vm", vm.clone())]))
            }
            SocketIntent::Boot { vm } => {
                dispatch_mutating(&transport, "boot", json_object([("vm", vm.clone())]))
            }
            SocketIntent::UsbAttach { vm, bus_id } => dispatch_mutating(
                &transport,
                "usbipBind",
                json_object([("vm", vm.clone()), ("busId", bus_id.clone())]),
            ),
            SocketIntent::UsbDetach { vm, bus_id } => dispatch_mutating(
                &transport,
                "usbipUnbind",
                json_object([("vm", vm.clone()), ("busId", bus_id.clone())]),
            ),
            SocketIntent::StoreVerify { vm } => {
                let value = request_value(
                    &transport,
                    frame(
                        "storeVerify",
                        json_values([
                            ("vm", Value::String(vm.clone())),
                            ("repair", Value::Bool(false)),
                        ]),
                    )?,
                )?;
                let response = parse_store_verify(value)?;
                Ok(DispatchOutcome {
                    summary: format!(
                        "store verify {}: status={} checked={} drifted={} repaired={}",
                        response.vm,
                        response.status,
                        response.checked,
                        response.drifted,
                        response.repaired
                    ),
                })
            }
        }
    }

    fn request<T>(&self, request: impl FnOnce(&SeqpacketTransport) -> WlResult<T>) -> WlResult<T> {
        let transport = self.connect_and_handshake()?;
        request(&transport)
    }

    fn connect_and_handshake(&self) -> WlResult<SeqpacketTransport> {
        reject_privileged_broker_socket(&self.socket_path)?;
        let transport = SeqpacketTransport::connect(Path::new(&self.socket_path), self.timeout)?;
        transport.send_payload(&hello_frame()?)?;
        parse_hello_reply(&transport.recv_payload()?)?;
        Ok(transport)
    }
}

fn request_auth_status(transport: &SeqpacketTransport) -> WlResult<Auth> {
    let value = request_value(transport, frame_empty("authStatus")?)?;
    let payload = response_payload(value, "authStatusResponse", "auth status", None)?;
    let payload = payload.get("auth").unwrap_or(&payload);
    Ok(Auth {
        role: map_auth_role(payload.get("role").and_then(Value::as_str)),
    })
}

fn request_inventory(transport: &SeqpacketTransport) -> WlResult<Inventory> {
    let value = request_value(
        transport,
        frame(
            "list",
            json_values([("env", Value::Null), ("vm", Value::Null)]),
        )?,
    )?;
    let payload = response_payload(value, "listResponse", "list", None)?;
    let vms = payload
        .get("vms")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(inventory_vm_from_value).collect())
        .unwrap_or_default();
    Ok(Inventory { vms })
}

fn request_status(transport: &SeqpacketTransport, vm: &str) -> WlResult<VmStatus> {
    Ok(request_status_snapshot(transport, vm)?.status)
}

fn request_status_snapshot(transport: &SeqpacketTransport, vm: &str) -> WlResult<StatusSnapshot> {
    let value = request_value(
        transport,
        frame(
            "status",
            json_values([
                ("checkBridges", Value::Bool(false)),
                ("vm", Value::String(vm.to_owned())),
            ]),
        )?,
    )?;
    let payload = response_payload(value, "statusResponse", "status", Some("status"))?;
    status_snapshot_from_payload(&payload, vm).ok_or_else(|| {
        WlError::Protocol(format!(
            "status response did not contain an entry for VM '{vm}'"
        ))
    })
}

fn request_status_model(transport: &SeqpacketTransport) -> WlResult<StatusModelSnapshot> {
    let value = request_value(
        transport,
        frame(
            "status",
            json_values([("checkBridges", Value::Bool(false)), ("vm", Value::Null)]),
        )?,
    )?;
    let payload = response_payload(value, "statusResponse", "status", None)?;
    status_model_from_payload(&payload)
        .ok_or_else(|| WlError::Protocol("status response did not contain VM entries".to_owned()))
}

fn request_usb_probe(transport: &SeqpacketTransport) -> WlResult<UsbProbe> {
    let value = request_value(transport, frame_empty("usbipProbe")?)?;
    if value.get("type").and_then(Value::as_str) == Some("mutatingVerbResponse") {
        let response: MutatingVerbResponse = serde_json::from_value(response_payload(
            value,
            "mutatingVerbResponse",
            "mutating verb",
            None,
        )?)?;
        return Err(mutating_response_error(response));
    }
    let payload = response_payload(value, "usbipProbeResponse", "usb probe", None)?;
    let claims = payload
        .get("entries")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(usb_claim_from_value).collect())
        .unwrap_or_default();
    Ok(UsbProbe { claims })
}

fn dispatch_mutating(
    transport: &SeqpacketTransport,
    request_type: &str,
    mut fields: Map<String, Value>,
) -> WlResult<DispatchOutcome> {
    fields.insert("dryRun".to_owned(), Value::Bool(false));
    fields.insert("apply".to_owned(), Value::Bool(true));
    fields.insert("json".to_owned(), Value::Bool(true));
    let value = request_value(transport, frame(request_type, fields)?)?;
    let payload = response_payload(value, "mutatingVerbResponse", "mutating verb", None)?;
    let response: MutatingVerbResponse = serde_json::from_value(payload)?;
    mutating_response_result(response)
}

fn mutating_response_result(response: MutatingVerbResponse) -> WlResult<DispatchOutcome> {
    match response.outcome.as_str() {
        "applied" => Ok(DispatchOutcome {
            summary: response
                .summary
                .unwrap_or_else(|| format!("{} {}", response.verb, response.outcome)),
        }),
        "api-ready-timeout" => Err(WlError::Timeout(response.failure_message())),
        "invalid-request" => Err(WlError::Protocol(response.failure_message())),
        "dry-run-planned" | "not-yet-implemented" | "broker-error" => {
            Err(WlError::Nixling(response.failure_message()))
        }
        other => Err(WlError::Protocol(format!(
            "unknown mutating verb outcome '{other}'"
        ))),
    }
}

fn mutating_response_error(response: MutatingVerbResponse) -> WlError {
    match mutating_response_result(response) {
        Ok(outcome) => WlError::Protocol(format!(
            "unexpected applied mutating response to usbipProbe: {}",
            outcome.summary
        )),
        Err(error) => error,
    }
}

fn request_value(transport: &SeqpacketTransport, payload: Vec<u8>) -> WlResult<Value> {
    transport.send_payload(&payload)?;
    let response = transport.recv_payload()?;
    let value: Value = serde_json::from_slice(&response)
        .map_err(|err| WlError::Protocol(format!("invalid JSON from nixlingd: {err}")))?;
    reject_error_response(&value)?;
    Ok(value)
}

fn hello_frame() -> WlResult<Vec<u8>> {
    let payload = HelloFrame {
        type_name: "hello",
        client_version: CLIENT_VERSION_RANGE,
        supported_features: CLIENT_FEATURES,
    };
    serde_json::to_vec(&payload).map_err(WlError::from)
}

fn frame_empty(type_name: &str) -> WlResult<Vec<u8>> {
    frame(type_name, Map::new())
}

fn frame(type_name: &str, mut payload: Map<String, Value>) -> WlResult<Vec<u8>> {
    payload.insert("type".to_owned(), Value::String(type_name.to_owned()));
    serde_json::to_vec(&Value::Object(payload)).map_err(WlError::from)
}

fn json_object<const N: usize>(fields: [(&str, String); N]) -> Map<String, Value> {
    fields
        .into_iter()
        .map(|(key, value)| (key.to_owned(), Value::String(value)))
        .collect()
}

fn vm_stop_fields(vm: &str, force: bool) -> Map<String, Value> {
    let mut fields = json_values([("vm", Value::String(vm.to_owned()))]);
    if force {
        fields.insert("force".to_owned(), Value::Bool(true));
    }
    fields
}

fn json_values<const N: usize>(fields: [(&str, Value); N]) -> Map<String, Value> {
    fields
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn parse_hello_reply(bytes: &[u8]) -> WlResult<()> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|err| WlError::Protocol(format!("invalid hello JSON from nixlingd: {err}")))?;
    reject_error_response(&value)?;
    let type_name = value.get("type").and_then(Value::as_str);
    if type_name == Some("helloRejected") {
        let reason = value
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if let Some(error) = value.get("error").and_then(parse_daemon_error) {
            return Err(error.into_wl_error());
        }
        return Err(match reason {
            "versionMismatch" => WlError::Protocol("nixlingd rejected client version".to_owned()),
            "capabilityNegotiationFailed" => {
                WlError::Protocol("nixlingd rejected client capabilities".to_owned())
            }
            _ => WlError::Nixling(format!("nixlingd rejected hello: {reason}")),
        });
    }

    let hello_ok = type_name == Some("helloOk")
        || (type_name.is_none()
            && value.get("serverVersion").is_some()
            && value.get("selectedVersion").is_some());
    if !hello_ok {
        return Err(WlError::Protocol(
            "unexpected nixlingd hello response".to_owned(),
        ));
    }
    let selected = value
        .get("selectedVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| WlError::Protocol("helloOk missing selectedVersion".to_owned()))?;
    selected_version_supported(selected).map_err(|reason| {
        WlError::Protocol(format!(
            "nixlingd selected unsupported protocol version {selected}: {reason}"
        ))
    })
}

fn selected_version_supported(version: &str) -> Result<(), String> {
    let version = parse_stable_semver(version)?;
    if ((0, 4, 0)..(0, 5, 0)).contains(&version) {
        Ok(())
    } else {
        Err("outside client range >=0.4.0, <0.5.0".to_owned())
    }
}

fn parse_stable_semver(version: &str) -> Result<(u64, u64, u64), String> {
    let (core, build) = version.split_once('+').unwrap_or((version, ""));
    if let Some(build) = version.split_once('+').map(|(_, build)| build) {
        validate_semver_identifiers(build, "build metadata")?;
    }
    if core.contains('-') {
        return Err("pre-release versions are not accepted".to_owned());
    }
    let mut parts = core.split('.');
    let major = parse_numeric_identifier(parts.next(), "major")?;
    let minor = parse_numeric_identifier(parts.next(), "minor")?;
    let patch = parse_numeric_identifier(parts.next(), "patch")?;
    if parts.next().is_some() {
        return Err("expected exactly major.minor.patch".to_owned());
    }
    if build.is_empty() && version.contains('+') {
        return Err("build metadata must not be empty".to_owned());
    }
    Ok((major, minor, patch))
}

fn parse_numeric_identifier(part: Option<&str>, name: &str) -> Result<u64, String> {
    let part = part.ok_or_else(|| format!("missing {name} version component"))?;
    if part.is_empty() {
        return Err(format!("{name} version component is empty"));
    }
    if part.len() > 1 && part.starts_with('0') {
        return Err(format!("{name} version component has a leading zero"));
    }
    if !part.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("{name} version component is not numeric"));
    }
    part.parse::<u64>()
        .map_err(|err| format!("{name} version component is invalid: {err}"))
}

fn validate_semver_identifiers(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    for identifier in value.split('.') {
        if identifier.is_empty() {
            return Err(format!("{label} contains an empty identifier"));
        }
        if !identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(format!("{label} contains an invalid identifier"));
        }
    }
    Ok(())
}

fn response_payload(
    value: Value,
    expected_type: &str,
    expected_kind: &str,
    wrapper_field: Option<&str>,
) -> WlResult<Value> {
    reject_error_response(&value)?;
    if let Some(type_name) = value.get("type").and_then(Value::as_str) {
        if type_name != expected_type {
            return Err(WlError::Protocol(format!(
                "expected {expected_type}, got {type_name}"
            )));
        }
        if let Some(field) = wrapper_field {
            return value
                .get(field)
                .cloned()
                .ok_or_else(|| WlError::Protocol(format!("{expected_type} missing {field}")));
        }
        let mut object = value
            .as_object()
            .cloned()
            .ok_or_else(|| WlError::Protocol(format!("{expected_type} must be an object")))?;
        object.remove("type");
        return Ok(Value::Object(object));
    }

    if let Some(kind) = value.get("kind").and_then(Value::as_str) {
        if kind != expected_kind {
            return Err(WlError::Protocol(format!(
                "expected response kind '{expected_kind}', got '{kind}'"
            )));
        }
        return Ok(value.get("payload").cloned().unwrap_or(Value::Null));
    }

    Ok(value)
}

fn reject_error_response(value: &Value) -> WlResult<()> {
    if value.get("type").and_then(Value::as_str) == Some("error") {
        let error = value
            .get("error")
            .and_then(parse_daemon_error)
            .unwrap_or_else(|| DaemonError::new("error", "nixlingd returned an error"));
        return Err(error.into_wl_error());
    }
    if value.get("kind").and_then(Value::as_str) == Some("error") {
        let error_value = value.get("payload").unwrap_or(value);
        let error = parse_daemon_error(error_value)
            .unwrap_or_else(|| DaemonError::new("error", "nixlingd returned an error"));
        return Err(error.into_wl_error());
    }
    Ok(())
}

fn parse_daemon_error(value: &Value) -> Option<DaemonError> {
    serde_json::from_value(value.clone()).ok()
}

fn reject_privileged_broker_socket(socket_path: &str) -> WlResult<()> {
    let trimmed = socket_path.trim_end_matches('/');
    let is_canonical_broker = trimmed == "/run/nixling/priv.sock";
    let has_broker_filename = Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("priv.sock");
    if is_canonical_broker || has_broker_filename {
        return Err(WlError::Config(format!(
            "refusing to connect wlcontrol to privileged nixling broker socket {socket_path}; configure the public socket instead"
        )));
    }
    Ok(())
}

fn inventory_vm_from_value(value: &Value) -> Option<InventoryVm> {
    let name = string_field(value, &["name", "vm"])?;
    Some(InventoryVm {
        name,
        env: string_field(value, &["env"]),
        is_net_vm: bool_field(value, &["isNetVm", "is_net_vm"]).unwrap_or(false),
        features: wlcontrol_core::model::VmFeatures::default(),
        capabilities: capabilities_from_value(value),
        static_ip: None,
        coarse_status: nested_string_field(value, &["lifecycle"], &["state"])
            .or_else(|| runtime_text(value.get("runtime"))),
    })
}

#[derive(Debug, Clone)]
struct StatusSnapshot {
    status: VmStatus,
    static_ip: Option<String>,
}

#[derive(Debug, Clone)]
struct StatusModelSnapshot {
    inventory: Inventory,
    statuses: Vec<VmStatus>,
    usb: UsbProbe,
}

fn status_model_from_payload(payload: &Value) -> Option<StatusModelSnapshot> {
    let entries = status_entries(payload)?;
    let mut inventory = Vec::with_capacity(entries.len());
    let mut statuses = Vec::with_capacity(entries.len());
    let mut claims = Vec::new();

    for entry in entries {
        let snapshot = status_snapshot_from_entry(entry, None)?;
        let mut vm = inventory_vm_from_value(entry)?;
        vm.static_ip = snapshot.static_ip.clone();
        for claim in usb_claims_from_status_entry(entry) {
            claims.push(claim);
        }
        inventory.push(vm);
        statuses.push(snapshot.status);
    }

    Some(StatusModelSnapshot {
        inventory: Inventory { vms: inventory },
        statuses,
        usb: UsbProbe { claims },
    })
}

fn status_entries(payload: &Value) -> Option<Vec<&Value>> {
    if let Some(entries) = payload.get("vms").and_then(Value::as_array) {
        return Some(entries.iter().collect());
    }
    if let Some(entries) = payload.get("entries").and_then(Value::as_array) {
        return Some(entries.iter().collect());
    }
    if let Some(status) = payload.get("status") {
        if let Some(entries) = status.get("vms").and_then(Value::as_array) {
            return Some(entries.iter().collect());
        }
        if let Some(entries) = status.get("entries").and_then(Value::as_array) {
            return Some(entries.iter().collect());
        }
    }
    None
}

fn status_snapshot_from_payload(payload: &Value, requested_vm: &str) -> Option<StatusSnapshot> {
    let candidate = status_candidate(payload, requested_vm)?;
    status_snapshot_from_entry(candidate, Some(requested_vm))
}

fn status_snapshot_from_entry(
    candidate: &Value,
    fallback_name: Option<&str>,
) -> Option<StatusSnapshot> {
    let name =
        string_field(candidate, &["name", "vm"]).or_else(|| fallback_name.map(str::to_owned))?;
    let pending_restart = candidate
        .get("lifecycle")
        .and_then(|lifecycle| bool_field(lifecycle, &["pendingRestart"]))
        .or_else(|| bool_field(candidate, &["pendingRestart", "pending_restart"]))
        .unwrap_or(false);
    let readiness = candidate
        .get("readiness")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let state = state_from_status(candidate);
    let static_ip = string_field(candidate, &["staticIp", "static_ip"]);

    Some(StatusSnapshot {
        status: VmStatus {
            name,
            state,
            pending_restart,
            readiness,
            capabilities: capabilities_from_value(candidate),
        },
        static_ip,
    })
}

fn usb_claims_from_status_entry(entry: &Value) -> Vec<UsbClaim> {
    entry
        .get("usb")
        .and_then(|usb| usb.get("entries").or(Some(usb)))
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(usb_claim_from_value).collect())
        .unwrap_or_default()
}

fn status_candidate<'a>(payload: &'a Value, requested_vm: &str) -> Option<&'a Value> {
    if let Some(entries) = payload.get("entries").and_then(Value::as_array) {
        entries
            .iter()
            .find(|entry| string_field(entry, &["name", "vm"]).as_deref() == Some(requested_vm))
            .or_else(|| entries.first())
    } else if let Some(vm) = payload.get("vm").filter(|vm| vm.is_object()) {
        Some(vm)
    } else {
        Some(payload)
    }
}

fn state_from_status(value: &Value) -> RuntimeState {
    if let Some(state) = nested_string_field(value, &["lifecycle"], &["state"]) {
        return lifecycle_state(&state);
    }
    if let Some(api_ready) = value.get("apiReady").or_else(|| value.get("api_ready")) {
        if let Some(state) = api_ready_state(api_ready) {
            return state;
        }
    }
    if let Some(services) = value.get("services") {
        if let Some(microvm) = string_field(services, &["microvm", "ch", "cloudHypervisor"]) {
            return service_state(
                &microvm,
                value.get("apiReady").or_else(|| value.get("api_ready")),
            );
        }
    }
    runtime_text(value.get("runtime"))
        .map(|text| text_state(&text))
        .unwrap_or(RuntimeState::Unknown)
}

fn lifecycle_state(state: &str) -> RuntimeState {
    match state {
        "Booted" | "booted" | "Running" | "running" => RuntimeState::Running,
        "Starting" | "starting" => RuntimeState::Starting,
        "Restarting" | "restarting" | "Stopping" | "stopping" => RuntimeState::Stopping,
        "Stopped" | "stopped" => RuntimeState::Stopped,
        _ => RuntimeState::Unknown,
    }
}

fn service_state(microvm: &str, api_ready: Option<&Value>) -> RuntimeState {
    let state = text_state(microvm);
    if state == RuntimeState::Running {
        match api_ready.and_then(api_ready_state) {
            Some(RuntimeState::Starting) => RuntimeState::Starting,
            Some(RuntimeState::Unknown) => RuntimeState::Unknown,
            _ => RuntimeState::Running,
        }
    } else {
        state
    }
}

fn api_ready_state(value: &Value) -> Option<RuntimeState> {
    let text = if let Some(s) = value.as_str() {
        Some(s.to_owned())
    } else {
        value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }?;
    match text.as_str() {
        "yes" | "Yes" | "ready" | "Ready" => Some(RuntimeState::Running),
        "pending" | "Pending" => Some(RuntimeState::Starting),
        "timeout" | "Timeout" => Some(RuntimeState::Unknown),
        _ => None,
    }
}

fn text_state(text: &str) -> RuntimeState {
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("running") {
        RuntimeState::Running
    } else if lower.starts_with("starting")
        || lower.starts_with("booted")
        || lower.starts_with("restarting")
        || lower.contains("pending")
    {
        RuntimeState::Starting
    } else if lower.starts_with("stopping") {
        RuntimeState::Stopping
    } else if lower.starts_with("stopped") {
        RuntimeState::Stopped
    } else {
        RuntimeState::Unknown
    }
}

fn usb_claim_from_value(value: &Value) -> Option<UsbClaim> {
    let status = string_field(value, &["status"]).unwrap_or_default();
    Some(UsbClaim {
        vm: string_field(value, &["vm"])?,
        env: string_field(value, &["env"])?,
        bus_id: string_field(value, &["busId", "bus_id"])?,
        bound: bool_field(value, &["bound"]).unwrap_or_else(|| status == "bound"),
        owner_vm: string_field(value, &["ownerVm", "owner_vm"]),
    })
}

fn capabilities_from_value(value: &Value) -> VmCapabilities {
    let mut capabilities = VmCapabilities::default();
    if let Some(runtime) = value.get("runtime") {
        apply_positive_capabilities(&mut capabilities, runtime);
    }
    apply_unsupported_capabilities(&mut capabilities, value);
    capabilities
}

fn apply_positive_capabilities(capabilities: &mut VmCapabilities, runtime: &Value) {
    let Some(operation) = runtime
        .get("operationCapabilities")
        .or_else(|| runtime.get("operation_capabilities"))
    else {
        return;
    };
    if let Some(lifecycle) = operation.get("lifecycle") {
        if let Some(v) = bool_field(lifecycle, &["start"]) {
            capabilities.start = v;
        }
        if let Some(v) = bool_field(lifecycle, &["stop"]) {
            capabilities.stop = v;
        }
        if let Some(v) = bool_field(lifecycle, &["restart"]) {
            capabilities.restart = v;
        }
        if let Some(v) = bool_field(lifecycle, &["switch"]) {
            capabilities.switch = v;
            capabilities.build = v;
            capabilities.boot = v;
        }
    }
    if let Some(media) = operation.get("media") {
        if let Some(v) = bool_field(media, &["usbHotplug", "usb_hotplug"]) {
            capabilities.usb_hotplug = v;
        }
    }
    if let Some(guest) = operation.get("guest") {
        if let Some(v) = bool_field(guest, &["exec"]) {
            capabilities.terminal = v;
        }
    }
    if let Some(storage) = operation.get("storage") {
        if let Some(v) = bool_field(storage, &["storeSync", "store_sync"]) {
            capabilities.store_verify = v;
        }
    }
}

fn apply_unsupported_capabilities(capabilities: &mut VmCapabilities, value: &Value) {
    let Some(items) = value
        .get("unsupportedCapabilities")
        .or_else(|| value.get("unsupported_capabilities"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for item in items.iter().filter_map(Value::as_str) {
        match item {
            "exec" | "guest-control" => capabilities.terminal = false,
            "store-sync" => capabilities.store_verify = false,
            "config-sync" => {
                capabilities.switch = false;
                capabilities.build = false;
                capabilities.boot = false;
            }
            "usb-hotplug" => capabilities.usb_hotplug = false,
            _ => {}
        }
    }
}

fn map_auth_role(role: Option<&str>) -> AuthRole {
    match role.unwrap_or_default().to_ascii_lowercase().as_str() {
        "admin" => AuthRole::Admin,
        "launcher" => AuthRole::Launcher,
        _ => AuthRole::None,
    }
}

fn parse_store_verify(value: Value) -> WlResult<StoreVerifySummary> {
    let payload = response_payload(value, "storeVerifyResponse", "store verify", None)?;
    serde_json::from_value(payload).map_err(WlError::from)
}

fn string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .map(str::to_owned)
}

fn nested_string_field(value: &Value, parent_fields: &[&str], fields: &[&str]) -> Option<String> {
    parent_fields
        .iter()
        .find_map(|field| value.get(*field))
        .and_then(|nested| string_field(nested, fields))
}

fn bool_field(value: &Value, fields: &[&str]) -> Option<bool> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_bool))
}

fn runtime_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value.as_str().map(str::to_owned).or_else(|| {
        value
            .get("detail")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HelloFrame<'a> {
    #[serde(rename = "type")]
    type_name: &'a str,
    client_version: &'a str,
    supported_features: &'a [&'a str],
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonError {
    kind: String,
    #[serde(default, alias = "exitCode", alias = "code")]
    exit_code: Option<u8>,
    message: String,
    #[serde(default)]
    remediation: Option<String>,
}

impl DaemonError {
    fn new(kind: &str, message: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            exit_code: None,
            message: message.to_owned(),
            remediation: None,
        }
    }

    fn into_wl_error(self) -> WlError {
        let message = if let Some(remediation) = self.remediation.filter(|value| !value.is_empty())
        {
            format!("{}: {} ({remediation})", self.kind, self.message)
        } else {
            format!("{}: {}", self.kind, self.message)
        };
        if self.kind.starts_with("authz-") || matches!(self.exit_code, Some(10 | 11)) {
            WlError::Denied(message)
        } else if self.kind == "wire-version-mismatch" || self.kind.starts_with("wire-") {
            WlError::Protocol(message)
        } else {
            WlError::Nixling(message)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MutatingVerbResponse {
    verb: String,
    outcome: String,
    #[serde(default)]
    target_wave: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    remediation: Option<String>,
    #[serde(default)]
    api_ready: Option<String>,
}

impl MutatingVerbResponse {
    fn failure_message(&self) -> String {
        let mut message = format!("{} returned {}", self.verb, self.outcome);
        if let Some(summary) = &self.summary {
            message.push_str(": ");
            message.push_str(summary);
        }
        if let Some(target_wave) = &self.target_wave {
            message.push_str(" (target wave ");
            message.push_str(target_wave);
            message.push(')');
        }
        if let Some(remediation) = &self.remediation {
            message.push_str("; ");
            message.push_str(remediation);
        }
        if let Some(api_ready) = &self.api_ready {
            message.push_str("; api-ready=");
            message.push_str(api_ready);
        }
        message
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreVerifySummary {
    vm: String,
    status: String,
    #[serde(default)]
    checked: u32,
    #[serde(default)]
    drifted: u32,
    #[serde(default)]
    repaired: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_carries_config() {
        let config = Config {
            public_socket: "/run/nixling/test.sock".into(),
            command_timeout_ms: 1234,
            ..Default::default()
        };
        let client = NixlingClient::new(&config);
        assert_eq!(client.socket_path(), "/run/nixling/test.sock");
        assert_eq!(client.timeout(), Duration::from_millis(1234));
    }

    #[test]
    fn refresh_reports_daemon_down_for_absent_socket() {
        let client = NixlingClient::new(&Config {
            public_socket: "/run/nixling-wlcontrol-absent-public.sock".to_owned(),
            ..Default::default()
        });
        assert_eq!(client.refresh().connectivity, Connectivity::DaemonDown);
    }

    #[test]
    fn selected_version_range_matches_nixling_v04() {
        assert!(selected_version_supported("0.4.0").is_ok());
        assert!(selected_version_supported("0.4.9").is_ok());
        assert!(selected_version_supported("0.4.0+build.1").is_ok());
        assert!(selected_version_supported("0.4").is_err());
        assert!(selected_version_supported("0.4.0-alpha").is_err());
        assert!(selected_version_supported("0.5.0").is_err());
        assert!(selected_version_supported("0.04.0").is_err());
    }

    #[test]
    fn broker_socket_guard_rejects_priv_sock_paths() {
        assert!(reject_privileged_broker_socket("/run/nixling/priv.sock").is_err());
        assert!(reject_privileged_broker_socket("/custom/priv.sock").is_err());
        assert!(reject_privileged_broker_socket("/run/nixling/public.sock").is_ok());
    }
}
