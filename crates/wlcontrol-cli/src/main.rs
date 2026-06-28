//! `d2b-wlcontrol` — Waybar module, control-center launcher, and action
//! dispatcher for d2b VMs.
//!
//! Owning wave: **Wave 0 (integrator) skeleton**; **Wave 2 — CLI/action shell
//! agent** hardens the Waybar loop (signals, no-overlap refresh, backoff),
//! single-instance `open`, and the full action surface.

use std::env;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::raw::c_int;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde::Serialize;
use wlcontrol_core::model::{ActionKind, Connectivity, Unavailable};
use wlcontrol_core::{plan, reduce, Config, PlannedAction, WlState};
use wlcontrol_d2b::D2bClient;
use wlcontrol_waybar::DisplayMode;

/// Starter Waybar config snippet, kept in sync with `data/`.
const WAYBAR_CONFIG_SNIPPET: &str = include_str!("../../../data/waybar-module.jsonc");
/// Starter CSS, kept in sync with `data/`.
const STYLE_SNIPPET: &str = include_str!("../../../data/style.css");
/// Waybar's `"signal"` value. Waybar sends SIGRTMIN+N for this integer N.
const WAYBAR_REFRESH_SIGNAL_OFFSET: c_int = 8;
const WAYBAR_MAX_DAEMON_DOWN_BACKOFF: Duration = Duration::from_secs(30);
const WAYBAR_SLEEP_GRANULARITY: Duration = Duration::from_millis(100);
const STATE_APP_DIR: &str = "d2b-wlcontrol";
const DISPLAY_MODE_FILE: &str = "display-mode";
const WAYBAR_PID_FILE: &str = "waybar.pid";
const O_NOFOLLOW_FLAG: i32 = 0o400000;

static WAYBAR_REFRESH_REQUESTED: AtomicBool = AtomicBool::new(false);

type SignalHandler = usize;

unsafe extern "C" {
    #[link_name = "__libc_current_sigrtmin"]
    fn libc_current_sigrtmin() -> c_int;
    fn signal(signum: c_int, handler: SignalHandler) -> SignalHandler;
    fn kill(pid: c_int, sig: c_int) -> c_int;
}

#[derive(Debug, Parser)]
#[command(
    name = "d2b-wlcontrol",
    version,
    about = "Clean Waybar indicator and control center for d2b VMs."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the continuous Waybar custom JSON module.
    Waybar,
    /// Open (or focus) the Quickshell control center.
    Open,
    /// Print the normalized control-surface state as JSON.
    StatusJson,
    /// Print detected host USB devices as JSON for the control popup.
    UsbDevicesJson,
    /// Dispatch a single control action.
    Action {
        #[command(subcommand)]
        action: ActionCommand,
    },
    /// Print a starter Waybar custom-module config snippet.
    PrintWaybarConfig,
    /// Print a starter CSS snippet.
    PrintCss,
}

#[derive(Debug, Subcommand)]
enum ActionCommand {
    /// Refresh state (used by Waybar middle-click).
    Refresh,
    /// Cycle the Waybar compact/detail display mode.
    CycleDisplay,
    /// Open / focus the control center.
    Open,
    /// Start a VM.
    Start { vm: String },
    /// Stop a VM.
    Stop { vm: String },
    /// Restart a VM.
    Restart { vm: String },
    /// Activate a VM's current closure.
    Switch { vm: String },
    /// Build/evaluate a VM without activating it.
    Build { vm: String },
    /// Stage a VM closure for the next boot.
    Boot { vm: String },
    /// Launch a guest terminal via detached guest-control exec.
    Terminal { vm: String },
    /// Run a configured guest quick-launch command.
    QuickLaunch { vm: String, id: String },
    /// Attach a USB busid to a VM.
    UsbAttach { vm: String, bus_id: String },
    /// Detach a USB busid from a VM.
    UsbDetach { vm: String, bus_id: String },
    /// Verify a VM's store live pool.
    StoreVerify { vm: String },
    /// Enable or disable a VM microphone.
    AudioMic { vm: String, state: AudioToggle },
    /// Enable or disable a VM speaker.
    AudioSpeaker { vm: String, state: AudioToggle },
    /// Set speaker playback volume, 0-100.
    AudioSpeakerVolume { vm: String, level_percent: u8 },
    /// Set microphone input gain, 0-100.
    AudioMicGain { vm: String, level_percent: u8 },
    /// Disable microphone and speaker forwarding.
    AudioOff { vm: String },
    /// Open the configured Signoz observability portal.
    Observability,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum AudioToggle {
    On,
    Off,
}

impl ActionCommand {
    fn into_kind(self) -> ActionKind {
        match self {
            ActionCommand::Refresh => ActionKind::Refresh,
            ActionCommand::CycleDisplay => ActionKind::CycleDisplay,
            ActionCommand::Open => ActionKind::OpenControlCenter,
            ActionCommand::Start { vm } => ActionKind::Start { vm },
            ActionCommand::Stop { vm } => ActionKind::Stop { vm },
            ActionCommand::Restart { vm } => ActionKind::Restart { vm },
            ActionCommand::Switch { vm } => ActionKind::Switch { vm },
            ActionCommand::Build { vm } => ActionKind::Build { vm },
            ActionCommand::Boot { vm } => ActionKind::Boot { vm },
            ActionCommand::Terminal { vm } => ActionKind::LaunchTerminal { vm },
            ActionCommand::QuickLaunch { vm, id } => ActionKind::QuickLaunch { vm, id },
            ActionCommand::UsbAttach { vm, bus_id } => ActionKind::UsbAttach { vm, bus_id },
            ActionCommand::UsbDetach { vm, bus_id } => ActionKind::UsbDetach { vm, bus_id },
            ActionCommand::StoreVerify { vm } => ActionKind::StoreVerify { vm },
            ActionCommand::AudioMic { vm, state } => ActionKind::AudioMic {
                vm,
                on: matches!(state, AudioToggle::On),
            },
            ActionCommand::AudioSpeaker { vm, state } => ActionKind::AudioSpeaker {
                vm,
                on: matches!(state, AudioToggle::On),
            },
            ActionCommand::AudioSpeakerVolume { vm, level_percent } => {
                ActionKind::AudioSpeakerVolume { vm, level_percent }
            }
            ActionCommand::AudioMicGain { vm, level_percent } => {
                ActionKind::AudioMicGain { vm, level_percent }
            }
            ActionCommand::AudioOff { vm } => ActionKind::AudioOff { vm },
            ActionCommand::Observability => ActionKind::OpenObservability,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("d2b-wlcontrol: {err}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> wlcontrol_core::WlResult<ExitCode> {
    let config = Config::load()?;
    match cli.command {
        Command::Waybar => run_waybar(&config),
        Command::Open => run_open(&config),
        Command::StatusJson => run_status_json(&config),
        Command::UsbDevicesJson => run_usb_devices_json(),
        Command::Action { action } => run_action(&config, action.into_kind()),
        Command::PrintWaybarConfig => {
            print!("{}", waybar_config_output());
            Ok(ExitCode::SUCCESS)
        }
        Command::PrintCss => {
            print!("{STYLE_SNIPPET}");
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Build the current reduced state from one refresh cycle.
fn current_state(config: &Config) -> WlState {
    let client = D2bClient::new(config);
    reduce::reduce_with_config(client.refresh(), config)
}

fn run_status_json(config: &Config) -> wlcontrol_core::WlResult<ExitCode> {
    let state = current_state(config);
    let mut json = serde_json::to_string_pretty(&state)?;
    json.push('\n');
    print!("{json}");
    Ok(ExitCode::SUCCESS)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsbDeviceOutput {
    bus_id: String,
    vendor_id: String,
    product_id: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    product: Option<String>,
}

fn run_usb_devices_json() -> wlcontrol_core::WlResult<ExitCode> {
    let devices = scan_usb_devices();
    let mut json = serde_json::to_string_pretty(&devices)?;
    json.push('\n');
    print!("{json}");
    Ok(ExitCode::SUCCESS)
}

fn scan_usb_devices() -> Vec<UsbDeviceOutput> {
    let mut devices = fs::read_dir("/sys/bus/usb/devices")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| usb_device_from_sysfs(&entry.path()))
        .collect::<Vec<_>>();

    devices.sort_by(|a, b| {
        // Most d2b USB passthrough use is YubiKey; surface Yubico devices
        // first, then stable bus-id ordering.
        let a_yubi = a.vendor_id.eq_ignore_ascii_case("1050");
        let b_yubi = b.vendor_id.eq_ignore_ascii_case("1050");
        b_yubi.cmp(&a_yubi).then_with(|| a.bus_id.cmp(&b.bus_id))
    });
    devices
}

fn usb_device_from_sysfs(path: &Path) -> Option<UsbDeviceOutput> {
    let bus_id = path.file_name()?.to_string_lossy().to_string();
    // Interface directories look like `1-2:1.0`; attach expects the device
    // busid (`1-2`), so ignore interface rows.
    if bus_id.contains(':') {
        return None;
    }
    let vendor_id = read_trimmed(path.join("idVendor"))?;
    let product_id = read_trimmed(path.join("idProduct"))?;
    let manufacturer = read_trimmed(path.join("manufacturer"));
    let product = read_trimmed(path.join("product"));
    // Root hubs and generic hubs are rarely useful attach targets; keep
    // anything with a product string, but skip empty/generic hub-only rows.
    if product.as_deref().is_some_and(|p| {
        p.eq_ignore_ascii_case("USB2.0 Hub") || p.eq_ignore_ascii_case("USB3.0 Hub")
    }) {
        return None;
    }
    let label = match (manufacturer.as_deref(), product.as_deref()) {
        (Some(m), Some(p)) if !m.is_empty() && !p.is_empty() => format!("{m} {p} ({bus_id})"),
        (_, Some(p)) if !p.is_empty() => format!("{p} ({bus_id})"),
        _ => format!("{vendor_id}:{product_id} ({bus_id})"),
    };
    Some(UsbDeviceOutput {
        bus_id,
        vendor_id,
        product_id,
        label,
        manufacturer,
        product,
    })
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn run_waybar(config: &Config) -> wlcontrol_core::WlResult<ExitCode> {
    install_waybar_signal_handler()?;
    let _pid_file = WaybarPidFile::write();
    let interval = Duration::from_millis(config.refresh_interval_ms.max(250));
    let mut stdout = std::io::stdout();
    let mut daemon_down_cycles = 0_u32;

    loop {
        let state = current_state(config);
        let mode = read_display_mode_from_default_path();
        let line = wlcontrol_waybar::render_mode(&state, mode);
        // Best-effort write; a closed pipe means Waybar went away.
        if stdout.write_all(line.to_json_line().as_bytes()).is_err() {
            break;
        }
        if stdout.flush().is_err() {
            break;
        }

        let sleep_for = if state.connectivity == Connectivity::DaemonDown {
            let sleep_for = daemon_down_backoff(interval, daemon_down_cycles);
            daemon_down_cycles = daemon_down_cycles.saturating_add(1);
            sleep_for
        } else {
            daemon_down_cycles = 0;
            interval
        };
        wait_for_refresh_or_timeout(sleep_for);
    }
    Ok(ExitCode::SUCCESS)
}

fn run_open(config: &Config) -> wlcontrol_core::WlResult<ExitCode> {
    wlcontrol_ui::open(config)?;
    Ok(ExitCode::SUCCESS)
}

fn run_action(config: &Config, action: ActionKind) -> wlcontrol_core::WlResult<ExitCode> {
    let state = action_planning_state(config, &action);
    let client = D2bClient::new(config);

    match plan::plan(&action, &state, config) {
        Err(Unavailable::Blocked { .. }) if action == ActionKind::OpenControlCenter => {
            run_open(config)
        }
        Err(Unavailable::Blocked { .. }) if action == ActionKind::CycleDisplay => {
            let mode = cycle_display_mode()?;
            println!("display mode: {}", display_mode_name(mode));
            Ok(ExitCode::SUCCESS)
        }
        Ok(PlannedAction::Socket { intent }) => match client.dispatch(&intent) {
            Ok(outcome) => {
                if action == ActionKind::Refresh {
                    signal_waybar_from_pidfile();
                }
                println!("{}", outcome.summary);
                Ok(ExitCode::SUCCESS)
            }
            Err(err) => Err(err),
        },
        Ok(PlannedAction::Process { argv, wait }) => run_process(argv, wait, config),
        Err(reason) => {
            eprintln!(
                "d2b-wlcontrol: action unavailable: {}",
                describe_unavailable(&reason)
            );
            Ok(ExitCode::from(1))
        }
    }
}

fn action_planning_state(config: &Config, action: &ActionKind) -> WlState {
    if matches!(
        action,
        ActionKind::Refresh
            | ActionKind::OpenControlCenter
            | ActionKind::OpenObservability
            | ActionKind::CycleDisplay
    ) {
        WlState::default()
    } else {
        current_state(config)
    }
}

/// Run an argv-only host process. There is no shell interpretation: the first
/// element is the program and the rest are arguments.
fn run_process(
    argv: Vec<String>,
    wait: bool,
    config: &Config,
) -> wlcontrol_core::WlResult<ExitCode> {
    let Some((program, args)) = argv.split_first() else {
        return Err(wlcontrol_core::WlError::Config(
            "empty process argv".to_owned(),
        ));
    };
    let mut command = std::process::Command::new(program);
    command.args(args);
    if is_d2b_program(program) {
        command.env("D2B_PUBLIC_SOCKET", &config.public_socket);
    }
    if wait {
        let output = command.output()?;
        std::io::stdout().write_all(&output.stdout)?;
        std::io::stderr().write_all(&output.stderr)?;
        let code = output
            .status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from);
        return Ok(code);
    }
    command.spawn()?;
    Ok(ExitCode::SUCCESS)
}

fn is_d2b_program(program: &str) -> bool {
    Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("d2b")
}

extern "C" fn waybar_signal_handler(_signal: c_int) {
    WAYBAR_REFRESH_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_waybar_signal_handler() -> wlcontrol_core::WlResult<()> {
    let signal_number = waybar_refresh_signal_number();
    let handler = waybar_signal_handler as *const () as SignalHandler;
    if unsafe { signal(signal_number, handler) } == usize::MAX {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn waybar_refresh_signal_number() -> c_int {
    (unsafe { libc_current_sigrtmin() }) + WAYBAR_REFRESH_SIGNAL_OFFSET
}

fn wait_for_refresh_or_timeout(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if WAYBAR_REFRESH_REQUESTED.swap(false, Ordering::SeqCst) {
            return;
        }
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        thread::sleep(WAYBAR_SLEEP_GRANULARITY.min(deadline.saturating_duration_since(now)));
    }
}

fn daemon_down_backoff(base: Duration, daemon_down_cycles: u32) -> Duration {
    let shift = daemon_down_cycles.min(10);
    let base_ms = base.as_millis().max(250);
    let backed_off = base_ms.saturating_mul(1_u128 << shift);
    Duration::from_millis(
        backed_off
            .min(WAYBAR_MAX_DAEMON_DOWN_BACKOFF.as_millis())
            .try_into()
            .unwrap_or(u64::MAX),
    )
}

fn waybar_config_output() -> String {
    format!(
        "// The \"signal\": {offset} key below lets `action cycle-display` refresh the bar immediately via SIGRTMIN+{offset}.\n{WAYBAR_CONFIG_SNIPPET}",
        offset = WAYBAR_REFRESH_SIGNAL_OFFSET
    )
}

fn cycle_display_mode() -> wlcontrol_core::WlResult<DisplayMode> {
    let path = display_mode_path()?;
    let next = cycle_display_mode_at(&path)?;
    signal_waybar_from_pidfile();
    Ok(next)
}

fn cycle_display_mode_at(path: &Path) -> wlcontrol_core::WlResult<DisplayMode> {
    let next = toggle_display_mode(read_display_mode_at(path));
    write_display_mode_at(path, next)?;
    Ok(next)
}

fn read_display_mode_from_default_path() -> DisplayMode {
    display_mode_path()
        .ok()
        .as_deref()
        .map(read_display_mode_at)
        .unwrap_or(DisplayMode::Compact)
}

fn read_display_mode_at(path: &Path) -> DisplayMode {
    read_state_file_at(path)
        .map(|contents| parse_display_mode(&contents))
        .unwrap_or(DisplayMode::Compact)
}

fn write_display_mode_at(path: &Path, mode: DisplayMode) -> wlcontrol_core::WlResult<()> {
    write_state_file_at(path, &format!("{}\n", display_mode_name(mode)))
}

fn parse_display_mode(contents: &str) -> DisplayMode {
    match contents
        .trim()
        .trim_matches('"')
        .to_ascii_lowercase()
        .as_str()
    {
        "detail" => DisplayMode::Detail,
        _ => DisplayMode::Compact,
    }
}

fn toggle_display_mode(mode: DisplayMode) -> DisplayMode {
    match mode {
        DisplayMode::Compact => DisplayMode::Detail,
        DisplayMode::Detail => DisplayMode::Compact,
    }
}

fn display_mode_name(mode: DisplayMode) -> &'static str {
    match mode {
        DisplayMode::Compact => "compact",
        DisplayMode::Detail => "detail",
    }
}

fn display_mode_path() -> wlcontrol_core::WlResult<PathBuf> {
    Ok(state_app_dir()?.join(DISPLAY_MODE_FILE))
}

fn waybar_pid_path() -> wlcontrol_core::WlResult<PathBuf> {
    Ok(state_app_dir()?.join(WAYBAR_PID_FILE))
}

fn read_state_file_at(path: &Path) -> std::io::Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW_FLAG)
        .open(path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

fn write_state_file_at(path: &Path, contents: &str) -> wlcontrol_core::WlResult<()> {
    if let Some(parent) = path.parent() {
        ensure_private_state_dir(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW_FLAG)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

fn ensure_private_state_dir(path: &Path) -> wlcontrol_core::WlResult<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn state_app_dir() -> wlcontrol_core::WlResult<PathBuf> {
    Ok(state_root()?.join(STATE_APP_DIR))
}

fn state_root() -> wlcontrol_core::WlResult<PathBuf> {
    if let Some(path) = non_empty_env_path("XDG_STATE_HOME") {
        return Ok(path);
    }
    if let Some(home) = non_empty_env_path("HOME") {
        return Ok(home.join(".local/state"));
    }
    Err(wlcontrol_core::WlError::Config(
        "HOME or XDG_STATE_HOME must be set to persist Waybar display mode".to_owned(),
    ))
}

fn non_empty_env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

struct WaybarPidFile {
    path: PathBuf,
    record: WaybarPidRecord,
}

impl WaybarPidFile {
    fn write() -> Option<Self> {
        let path = waybar_pid_path().ok()?;
        let record = WaybarPidRecord::current()?;
        write_state_file_at(&path, &record.serialize()).ok()?;
        Some(Self { path, record })
    }
}

impl Drop for WaybarPidFile {
    fn drop(&mut self) {
        if read_state_file_at(&self.path)
            .ok()
            .and_then(|contents| WaybarPidRecord::parse(&contents))
            .map(|record| record == self.record)
            .unwrap_or(false)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WaybarPidRecord {
    pid: u32,
    start_time: u64,
}

impl WaybarPidRecord {
    fn current() -> Option<Self> {
        let pid = std::process::id();
        let start_time = process_start_time(pid)?;
        Some(Self { pid, start_time })
    }

    fn parse(contents: &str) -> Option<Self> {
        let mut fields = contents.split_whitespace();
        let pid = fields.next()?.parse().ok()?;
        let start_time = fields.next()?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        Some(Self { pid, start_time })
    }

    fn serialize(self) -> String {
        format!("{} {}\n", self.pid, self.start_time)
    }
}

fn process_start_time(pid: u32) -> Option<u64> {
    let stat = Path::new("/proc").join(pid.to_string()).join("stat");
    fs::read_to_string(stat)
        .ok()
        .and_then(|contents| parse_proc_start_time(&contents))
}

fn parse_proc_start_time(stat: &str) -> Option<u64> {
    let (_comm, rest) = stat.rsplit_once(") ")?;
    rest.split_whitespace().nth(19)?.parse().ok()
}

fn process_matches_waybar(record: WaybarPidRecord) -> bool {
    process_start_time(record.pid)
        .map(|start_time| start_time == record.start_time)
        .unwrap_or(false)
        && process_cmdline_matches_waybar(record.pid)
}

fn process_cmdline_matches_waybar(pid: u32) -> bool {
    let cmdline = Path::new("/proc").join(pid.to_string()).join("cmdline");
    fs::read(cmdline)
        .map(|bytes| cmdline_matches_waybar(&bytes))
        .unwrap_or(false)
}

fn cmdline_matches_waybar(bytes: &[u8]) -> bool {
    let args = bytes
        .split(|b| *b == 0)
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>();
    let Some(program) = args.first() else {
        return false;
    };
    program_basename_matches(program) && args.get(1) == Some(&b"waybar".as_slice())
}

fn program_basename_matches(program: &[u8]) -> bool {
    program.rsplit(|b| *b == b'/').next() == Some(b"d2b-wlcontrol".as_slice())
}

fn signal_waybar_from_pidfile() {
    let Some(path) = waybar_pid_path().ok() else {
        return;
    };
    let Some(record) = read_state_file_at(&path)
        .ok()
        .and_then(|contents| WaybarPidRecord::parse(&contents))
    else {
        return;
    };
    if process_matches_waybar(record) {
        let Ok(pid) = c_int::try_from(record.pid) else {
            return;
        };
        unsafe {
            kill(pid, waybar_refresh_signal_number());
        };
    }
}

fn describe_unavailable(reason: &Unavailable) -> String {
    match reason {
        Unavailable::DaemonDown => "d2bd is unreachable".to_owned(),
        Unavailable::InsufficientRole { required } => {
            format!("requires {} role", auth_role_name(*required))
        }
        Unavailable::VmState { detail } | Unavailable::Blocked { detail } => detail.clone(),
        Unavailable::UsbOwnedElsewhere { owner } => format!("USB device is owned by {owner}"),
        Unavailable::NotYetImplemented => "not yet implemented".to_owned(),
    }
}

fn auth_role_name(role: wlcontrol_core::model::AuthRole) -> &'static str {
    match role {
        wlcontrol_core::model::AuthRole::None => "none",
        wlcontrol_core::model::AuthRole::Launcher => "launcher",
        wlcontrol_core::model::AuthRole::Admin => "admin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_state_file(name: &str) -> PathBuf {
        let root = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target"));
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is before Unix epoch")
            .as_nanos();
        root.join("wlcontrol-cli-tests")
            .join(format!("{name}-{}-{now}", std::process::id()))
            .join("state-file")
    }

    fn cleanup_state_file(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn parses_display_mode_file_contents() {
        assert_eq!(parse_display_mode("detail\n"), DisplayMode::Detail);
        assert_eq!(parse_display_mode("\"detail\""), DisplayMode::Detail);
        assert_eq!(parse_display_mode("compact\n"), DisplayMode::Compact);
        assert_eq!(parse_display_mode("unexpected"), DisplayMode::Compact);
    }

    #[test]
    fn toggles_display_mode_names() {
        assert_eq!(
            toggle_display_mode(DisplayMode::Compact),
            DisplayMode::Detail
        );
        assert_eq!(
            toggle_display_mode(DisplayMode::Detail),
            DisplayMode::Compact
        );
        assert_eq!(display_mode_name(DisplayMode::Detail), "detail");
    }

    #[test]
    fn missing_display_mode_file_defaults_to_compact() {
        let path = test_state_file("missing-display-mode");
        assert_eq!(read_display_mode_at(&path), DisplayMode::Compact);
        cleanup_state_file(&path);
    }

    #[test]
    fn display_mode_round_trips_with_private_state_permissions() {
        let path = test_state_file("display-mode-roundtrip");
        for mode in [DisplayMode::Compact, DisplayMode::Detail] {
            write_display_mode_at(&path, mode).expect("write display mode");
            assert_eq!(read_display_mode_at(&path), mode);
        }

        let parent = path.parent().expect("state file has parent");
        assert_eq!(
            fs::metadata(parent)
                .expect("state dir metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path)
                .expect("state file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        cleanup_state_file(&path);
    }

    #[test]
    fn cycle_display_mode_at_path_toggles_and_persists() {
        let path = test_state_file("cycle-display-mode");
        write_display_mode_at(&path, DisplayMode::Compact).expect("write compact mode");

        assert_eq!(
            cycle_display_mode_at(&path).expect("cycle compact to detail"),
            DisplayMode::Detail
        );
        assert_eq!(read_display_mode_at(&path), DisplayMode::Detail);
        assert_eq!(
            cycle_display_mode_at(&path).expect("cycle detail to compact"),
            DisplayMode::Compact
        );
        assert_eq!(read_display_mode_at(&path), DisplayMode::Compact);
        cleanup_state_file(&path);
    }

    #[test]
    fn daemon_down_backoff_caps_at_thirty_seconds() {
        let base = Duration::from_millis(1_000);
        assert_eq!(daemon_down_backoff(base, 0), Duration::from_millis(1_000));
        assert_eq!(daemon_down_backoff(base, 2), Duration::from_millis(4_000));
        assert_eq!(daemon_down_backoff(base, 20), Duration::from_secs(30));
    }

    #[test]
    fn generated_waybar_config_documents_signal_offset() {
        let output = waybar_config_output();
        assert!(output.contains("\"signal\": 8"));
        assert!(output.contains("SIGRTMIN+8"));
        assert!(output.contains(WAYBAR_CONFIG_SNIPPET));
    }

    #[test]
    fn pidfile_matching_requires_waybar_subcommand() {
        assert!(cmdline_matches_waybar(
            b"/nix/store/bin/d2b-wlcontrol\0waybar\0"
        ));
        assert!(!cmdline_matches_waybar(
            b"/nix/store/bin/d2b-wlcontrol\0open\0"
        ));
        assert!(!cmdline_matches_waybar(
            b"/nix/store/bin/d2b-wlcontrol\0action\0stop\0waybar\0"
        ));
        assert!(!cmdline_matches_waybar(
            b"/nix/store/bin/not-d2b-wlcontrol\0waybar\0"
        ));
        assert!(!cmdline_matches_waybar(b"/bin/other\0waybar\0"));
    }

    #[test]
    fn pidfile_record_round_trips_pid_and_start_time() {
        let record = WaybarPidRecord {
            pid: 42,
            start_time: 99,
        };
        assert_eq!(WaybarPidRecord::parse(&record.serialize()), Some(record));
        assert_eq!(WaybarPidRecord::parse("42\n"), None);
        assert_eq!(WaybarPidRecord::parse("42 99 extra\n"), None);
    }

    #[test]
    fn parses_proc_stat_start_time_after_process_name() {
        let stat =
            "123 (name with ) paren) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 4242 23 24";
        assert_eq!(parse_proc_start_time(stat), Some(4242));
    }
}
