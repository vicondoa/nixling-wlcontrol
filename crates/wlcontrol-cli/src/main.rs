//! `nixling-wlcontrol` — Waybar module, control-center launcher, and action
//! dispatcher for nixling VMs.
//!
//! Owning wave: **Wave 0 (integrator) skeleton**; **Wave 2 — CLI/action shell
//! agent** hardens the Waybar loop (signals, no-overlap refresh, backoff),
//! single-instance `open`, and the full action surface.

use std::env;
use std::fs;
use std::io::Write as _;
use std::os::raw::c_int;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use wlcontrol_core::model::{ActionKind, Connectivity, Unavailable};
use wlcontrol_core::{plan, reduce, Config, PlannedAction, WlState};
use wlcontrol_nixling::NixlingClient;
use wlcontrol_waybar::DisplayMode;

/// Starter Waybar config snippet, kept in sync with `data/`.
const WAYBAR_CONFIG_SNIPPET: &str = include_str!("../../../data/waybar-module.jsonc");
/// Starter CSS, kept in sync with `data/`.
const STYLE_SNIPPET: &str = include_str!("../../../data/style.css");
/// Waybar's `"signal"` value. Waybar sends SIGRTMIN+N for this integer N.
const WAYBAR_REFRESH_SIGNAL_OFFSET: c_int = 8;
const WAYBAR_MAX_DAEMON_DOWN_BACKOFF: Duration = Duration::from_secs(30);
const WAYBAR_SLEEP_GRANULARITY: Duration = Duration::from_millis(100);
const STATE_APP_DIR: &str = "nixling-wlcontrol";
const DISPLAY_MODE_FILE: &str = "display-mode";
const WAYBAR_PID_FILE: &str = "waybar.pid";

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
    name = "nixling-wlcontrol",
    version,
    about = "Clean Waybar indicator and control center for nixling VMs."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the continuous Waybar custom JSON module.
    Waybar,
    /// Open (or focus) the GTK control center.
    Open,
    /// Print the normalized control-surface state as JSON.
    StatusJson,
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
    /// Launch a terminal running an interactive guest shell.
    Terminal { vm: String },
    /// Attach a USB busid to a VM.
    UsbAttach { vm: String, bus_id: String },
    /// Detach a USB busid from a VM.
    UsbDetach { vm: String, bus_id: String },
    /// Verify a VM's store live pool.
    StoreVerify { vm: String },
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
            ActionCommand::Terminal { vm } => ActionKind::LaunchTerminal { vm },
            ActionCommand::UsbAttach { vm, bus_id } => ActionKind::UsbAttach { vm, bus_id },
            ActionCommand::UsbDetach { vm, bus_id } => ActionKind::UsbDetach { vm, bus_id },
            ActionCommand::StoreVerify { vm } => ActionKind::StoreVerify { vm },
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("nixling-wlcontrol: {err}");
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
    let client = NixlingClient::new(config);
    reduce::reduce(client.refresh())
}

fn run_status_json(config: &Config) -> wlcontrol_core::WlResult<ExitCode> {
    let state = current_state(config);
    let mut json = serde_json::to_string_pretty(&state)?;
    json.push('\n');
    print!("{json}");
    Ok(ExitCode::SUCCESS)
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
        let _ = stdout.flush();

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
    let client = NixlingClient::new(config);

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
        Ok(PlannedAction::Process { argv }) => spawn_process(argv),
        Err(reason) => {
            eprintln!(
                "nixling-wlcontrol: action unavailable: {}",
                describe_unavailable(&reason)
            );
            Ok(ExitCode::from(1))
        }
    }
}

fn action_planning_state(config: &Config, action: &ActionKind) -> WlState {
    if matches!(
        action,
        ActionKind::Refresh | ActionKind::OpenControlCenter | ActionKind::CycleDisplay
    ) {
        WlState::default()
    } else {
        current_state(config)
    }
}

/// Spawn an argv-only host process (terminal launch). There is no shell
/// interpretation: the first element is the program and the rest are arguments.
fn spawn_process(argv: Vec<String>) -> wlcontrol_core::WlResult<ExitCode> {
    let Some((program, args)) = argv.split_first() else {
        return Err(wlcontrol_core::WlError::Config(
            "empty terminal argv; check [terminal] config".to_owned(),
        ));
    };
    std::process::Command::new(program).args(args).spawn()?;
    Ok(ExitCode::SUCCESS)
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
        "// For signal-driven refresh, add \"signal\": {offset}; Waybar sends SIGRTMIN+{offset}.\n{WAYBAR_CONFIG_SNIPPET}",
        offset = WAYBAR_REFRESH_SIGNAL_OFFSET
    )
}

fn cycle_display_mode() -> wlcontrol_core::WlResult<DisplayMode> {
    let path = display_mode_path()?;
    let next = toggle_display_mode(read_display_mode_at(&path));
    write_display_mode_at(&path, next)?;
    signal_waybar_from_pidfile();
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
    fs::read_to_string(path)
        .map(|contents| parse_display_mode(&contents))
        .unwrap_or(DisplayMode::Compact)
}

fn write_display_mode_at(path: &Path, mode: DisplayMode) -> wlcontrol_core::WlResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, format!("{}\n", display_mode_name(mode)))?;
    Ok(())
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
    pid_text: String,
}

impl WaybarPidFile {
    fn write() -> Option<Self> {
        let path = waybar_pid_path().ok()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).ok()?;
        }
        let pid_text = std::process::id().to_string();
        fs::write(&path, format!("{pid_text}\n")).ok()?;
        Some(Self { path, pid_text })
    }
}

impl Drop for WaybarPidFile {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path)
            .map(|contents| contents.trim() == self.pid_text)
            .unwrap_or(false)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn signal_waybar_from_pidfile() {
    let Some(path) = waybar_pid_path().ok() else {
        return;
    };
    let Some(pid) = fs::read_to_string(path)
        .ok()
        .and_then(|contents| contents.trim().parse::<u32>().ok())
    else {
        return;
    };
    if process_matches_waybar(pid) {
        let Ok(pid) = c_int::try_from(pid) else {
            return;
        };
        unsafe {
            kill(pid, waybar_refresh_signal_number());
        };
    }
}

fn process_matches_waybar(pid: u32) -> bool {
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
    let program = String::from_utf8_lossy(program);
    program.ends_with("nixling-wlcontrol") && args.iter().any(|arg| *arg == b"waybar")
}

fn describe_unavailable(reason: &Unavailable) -> String {
    match reason {
        Unavailable::DaemonDown => "nixlingd is unreachable".to_owned(),
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
            b"/nix/store/bin/nixling-wlcontrol\0waybar\0"
        ));
        assert!(!cmdline_matches_waybar(
            b"/nix/store/bin/nixling-wlcontrol\0open\0"
        ));
        assert!(!cmdline_matches_waybar(b"/bin/other\0waybar\0"));
    }
}
