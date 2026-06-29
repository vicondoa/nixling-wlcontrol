//! User configuration for d2b-wlcontrol.
//!
//! Owning wave: **Wave 1 — Core model agent**. Wave 0 ships a minimal,
//! compiling skeleton with sane defaults and the on-disk location contract.
//! The Wave 1 agent fleshes out validation, terminal-argv parsing rules,
//! favorites/ordering, and the full option surface described in the plan.

use std::{collections::BTreeMap, path::Path};

use serde::{Deserialize, Serialize};

use crate::error::{WlError, WlResult};

/// Default config file location: `${XDG_CONFIG_HOME:-~/.config}/d2b-wlcontrol/config.toml`.
pub const CONFIG_RELATIVE_PATH: &str = "d2b-wlcontrol/config.toml";
pub const DEFAULT_COLOR_ARTIFACT_PATH: &str = "/etc/d2b/ui-colors.json";

const PRIVILEGED_BROKER_SOCKET_MESSAGE: &str =
    "refusing to use the privileged broker socket; d2b-wlcontrol speaks only the public socket";
const UI_COLOR_ARTIFACT_VERSION: u8 = 1;

/// Returns true when `path` is acceptable as a d2b public-socket path.
///
/// This intentionally rejects the privileged broker socket by both its exact
/// canonical path and by basename so downstream protocol clients can share the
/// same fail-closed guard before connecting.
pub fn is_public_socket_path(path: &str) -> bool {
    let path = path.trim();
    if path.is_empty() || path == "/run/d2b/priv.sock" {
        return false;
    }

    std::path::Path::new(path).file_name() != Some(std::ffi::OsStr::new("priv.sock"))
}

/// Top-level configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Config {
    /// Path to the d2b public socket.
    pub public_socket: String,
    /// Refresh cadence in milliseconds for the Waybar loop.
    pub refresh_interval_ms: u64,
    /// Per-operation timeout in milliseconds.
    pub command_timeout_ms: u64,
    /// Hide framework net VMs from the compact surfaces.
    pub hide_net_vms: bool,
    /// Show the pending-restart marker.
    pub show_pending_restart: bool,
    /// VM names pinned to the front of UI lists, in this order.
    ///
    /// Names not present in inventory are ignored by the reducer.
    pub favorites: Vec<String>,
    /// VM names hidden from compact surfaces while remaining present in
    /// [`crate::model::WlState`] for detail views and JSON consumers.
    pub hidden_vms: Vec<String>,
    /// Terminal launch configuration.
    pub terminal: TerminalConfig,
    /// Observability portal launch configuration.
    pub observability: ObservabilityConfig,
    /// Shell palette for the Quickshell popup.
    pub theme: ThemeConfig,
    /// Per-VM custom guest quick-launch icons.
    pub quick_launch: Vec<QuickLaunchConfig>,
    /// Path to d2b's resolved UI color JSON artifact.
    pub color_artifact_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiColorArtifact {
    pub version: u8,
    pub host: UiColorHost,
    pub states: UiColorStates,
    pub envs: BTreeMap<String, UiColorEnv>,
    pub vms: BTreeMap<String, UiColorVm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiColorHost {
    pub accent: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiColorStates {
    pub running: String,
    pub transitioning: String,
    pub pending_restart: String,
    pub error: String,
    pub denied: String,
    pub unknown: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiColorEnv {
    pub accent: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiColorVm {
    pub env: Option<String>,
    pub border: UiColorBorder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiColorBorder {
    pub active: String,
    pub inactive: String,
    pub urgent: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiColorErrorKind {
    Missing,
    Malformed,
    UnsupportedVersion,
}

/// Terminal launch configuration. The guest command is always an argv vector;
/// no shell string interpolation is performed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct TerminalConfig {
    /// Deprecated host-terminal argv prefix kept for config compatibility.
    pub argv: Vec<String>,
    /// Deprecated single guest command kept for config compatibility.
    pub guest_shell: String,
    /// Guest argv launched detached inside the VM.
    pub guest_argv: Vec<String>,
}

/// Observability portal configuration. Opening the browser is argv-only and
/// does not read credentials or mint login tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ObservabilityConfig {
    /// Whether to expose the observability portal action.
    pub enabled: bool,
    /// URL to open for the Signoz observability portal. `None` disables the
    /// header button until the operator configures a URL.
    pub url: Option<String>,
    /// argv prefix used to open the URL, e.g. `["xdg-open"]`.
    pub browser_argv: Vec<String>,
    /// Message shown by the popup after successfully launching observability.
    pub success_message: String,
}

/// Stylix-agnostic shell colors for the Quickshell popup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct ThemeConfig {
    pub background: String,
    pub surface: String,
    pub surface_alt: String,
    pub foreground: String,
    pub foreground_strong: String,
    pub foreground_disabled: String,
    pub muted: String,
    pub border: String,
    pub inverse_foreground: String,
    pub success_surface: String,
    pub warning_surface: String,
    pub error_surface: String,
    pub input_background: String,
    pub slider_track: String,
}

/// A per-VM quick-launch icon that runs a detached guest command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct QuickLaunchConfig {
    /// Stable action id used by `d2b-wlcontrol action quick-launch`.
    pub id: String,
    /// VM this icon appears on.
    pub vm: String,
    /// Material Symbols icon name.
    pub icon: String,
    /// Hover text shown in the popup.
    pub tooltip: String,
    /// Guest argv launched with `d2b vm exec -d <vm> -- ...`.
    pub guest_argv: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            public_socket: "/run/d2b/public.sock".to_owned(),
            refresh_interval_ms: 2500,
            command_timeout_ms: 10000,
            hide_net_vms: true,
            show_pending_restart: true,
            favorites: Vec::new(),
            hidden_vms: Vec::new(),
            terminal: TerminalConfig::default(),
            observability: ObservabilityConfig::default(),
            theme: ThemeConfig::default(),
            quick_launch: Vec::new(),
            color_artifact_path: DEFAULT_COLOR_ARTIFACT_PATH.to_owned(),
        }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            argv: vec!["foot".to_owned(), "--".to_owned()],
            guest_shell: "bash".to_owned(),
            guest_argv: vec!["/run/current-system/sw/bin/foot".to_owned()],
        }
    }
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            url: Some("http://sys-obs:8080".to_owned()),
            browser_argv: vec!["xdg-open".to_owned()],
            success_message: "Opened observability portal".to_owned(),
        }
    }
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            background: "#0f1117".to_owned(),
            surface: "#16181d".to_owned(),
            surface_alt: "#2a2d35".to_owned(),
            foreground: "#cdd6f4".to_owned(),
            foreground_strong: "#ffffff".to_owned(),
            foreground_disabled: "#bac2de".to_owned(),
            muted: "#9399b2".to_owned(),
            border: "#2a2d35".to_owned(),
            inverse_foreground: "#000000".to_owned(),
            success_surface: "#1a2e1a".to_owned(),
            warning_surface: "#2e2a1a".to_owned(),
            error_surface: "#2e1a1a".to_owned(),
            input_background: "#0d0d0d".to_owned(),
            slider_track: "#252832".to_owned(),
        }
    }
}

impl Config {
    /// Parse a configuration from a TOML string.
    pub fn from_toml(s: &str) -> WlResult<Self> {
        let config: Self = toml::from_str(s).map_err(|e| WlError::Config(e.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Validate a loaded configuration before any command planning uses it.
    pub fn validate(&self) -> WlResult<()> {
        if self.public_socket.trim().is_empty() {
            return Err(WlError::Config("public_socket must not be empty".into()));
        }
        if !is_public_socket_path(&self.public_socket) {
            return Err(WlError::Config(PRIVILEGED_BROKER_SOCKET_MESSAGE.into()));
        }
        if self.color_artifact_path.trim().is_empty() {
            return Err(WlError::Config(
                "color_artifact_path must not be empty".into(),
            ));
        }
        if self.terminal.guest_argv.is_empty() && self.terminal.guest_shell.trim().is_empty() {
            return Err(WlError::Config(
                "terminal.guest_argv must contain at least one argv element".into(),
            ));
        }
        if self.observability.enabled
            && self.observability.url.is_some()
            && self.observability.browser_argv.is_empty()
        {
            return Err(WlError::Config(
                "observability.browser_argv must contain at least one argv element".into(),
            ));
        }
        for (field, value) in self.theme.color_fields() {
            if !is_lower_hex_color(value) {
                return Err(WlError::Config(format!(
                    "theme.{field} must be a normalized #rrggbb color"
                )));
            }
        }
        for item in &self.quick_launch {
            if item.id.trim().is_empty() {
                return Err(WlError::Config("quick_launch.id must not be empty".into()));
            }
            if item.vm.trim().is_empty() {
                return Err(WlError::Config("quick_launch.vm must not be empty".into()));
            }
            if item.icon.trim().is_empty() {
                return Err(WlError::Config(
                    "quick_launch.icon must not be empty".into(),
                ));
            }
            if item.tooltip.trim().is_empty() {
                return Err(WlError::Config(
                    "quick_launch.tooltip must not be empty".into(),
                ));
            }
            if item.guest_argv.is_empty() {
                return Err(WlError::Config(
                    "quick_launch.guest_argv must contain at least one argv element".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn load_ui_colors(&self) -> WlResult<Option<UiColorArtifact>> {
        load_ui_colors_from_path(Path::new(&self.color_artifact_path))
    }

    /// Resolve the default config path under `$XDG_CONFIG_HOME`.
    pub fn default_path() -> Option<std::path::PathBuf> {
        directories::BaseDirs::new().map(|d| d.config_dir().join(CONFIG_RELATIVE_PATH))
    }

    /// Load configuration from the default path, falling back to built-in
    /// defaults when the file is absent. A present-but-malformed file is an
    /// error so the operator notices rather than silently getting defaults.
    pub fn load() -> WlResult<Self> {
        match Self::default_path() {
            Some(path) if path.exists() => {
                let text = std::fs::read_to_string(&path)?;
                Self::from_toml(&text)
            }
            _ => {
                let config = Self::default();
                config.validate()?;
                Ok(config)
            }
        }
    }
}

impl ThemeConfig {
    fn color_fields(&self) -> [(&'static str, &str); 14] {
        [
            ("background", &self.background),
            ("surface", &self.surface),
            ("surface_alt", &self.surface_alt),
            ("foreground", &self.foreground),
            ("foreground_strong", &self.foreground_strong),
            ("foreground_disabled", &self.foreground_disabled),
            ("muted", &self.muted),
            ("border", &self.border),
            ("inverse_foreground", &self.inverse_foreground),
            ("success_surface", &self.success_surface),
            ("warning_surface", &self.warning_surface),
            ("error_surface", &self.error_surface),
            ("input_background", &self.input_background),
            ("slider_track", &self.slider_track),
        ]
    }
}

pub fn load_ui_colors_from_path(path: &Path) -> WlResult<Option<UiColorArtifact>> {
    if !path.exists() {
        log_color_error(UiColorErrorKind::Missing, path, "artifact is missing");
        return Ok(None);
    }

    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            log_color_error(UiColorErrorKind::Malformed, path, &err.to_string());
            return Ok(None);
        }
    };

    let parsed: UiColorArtifact = match serde_json::from_str(&text) {
        Ok(parsed) => parsed,
        Err(err) => {
            log_color_error(UiColorErrorKind::Malformed, path, &err.to_string());
            return Ok(None);
        }
    };

    match validate_ui_colors(parsed) {
        Ok(colors) => Ok(Some(colors)),
        Err((reason, message)) => {
            log_color_error(reason, path, &message);
            Ok(None)
        }
    }
}

fn validate_ui_colors(
    colors: UiColorArtifact,
) -> Result<UiColorArtifact, (UiColorErrorKind, String)> {
    if colors.version != UI_COLOR_ARTIFACT_VERSION {
        return Err((
            UiColorErrorKind::UnsupportedVersion,
            format!(
                "unsupported UI color artifact version {}; expected {UI_COLOR_ARTIFACT_VERSION}",
                colors.version
            ),
        ));
    }

    let mut values = vec![
        ("host.accent", &colors.host.accent),
        ("states.running", &colors.states.running),
        ("states.transitioning", &colors.states.transitioning),
        ("states.pendingRestart", &colors.states.pending_restart),
        ("states.error", &colors.states.error),
        ("states.denied", &colors.states.denied),
        ("states.unknown", &colors.states.unknown),
    ];
    for (env, value) in &colors.envs {
        values.push((env.as_str(), &value.accent));
    }
    for (vm, value) in &colors.vms {
        values.push((vm.as_str(), &value.border.active));
        values.push((vm.as_str(), &value.border.inactive));
        values.push((vm.as_str(), &value.border.urgent));
    }
    for (field, value) in values {
        if !is_lower_hex_color(value) {
            return Err((
                UiColorErrorKind::Malformed,
                format!("UI color artifact field {field} is not a normalized #rrggbb color"),
            ));
        }
    }

    Ok(colors)
}

fn is_lower_hex_color(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7
        && bytes[0] == b'#'
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

fn log_color_error(kind: UiColorErrorKind, path: &Path, detail: &str) {
    let reason = match kind {
        UiColorErrorKind::Missing => "missing",
        UiColorErrorKind::Malformed => "malformed",
        UiColorErrorKind::UnsupportedVersion => "unsupported_version",
    };
    let path_basename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown");
    eprintln!(
        "event=ui_color_artifact_error reason={reason} path_basename={path_basename} detail={}",
        sanitize_log_detail(detail)
    );
}

fn sanitize_log_detail(detail: &str) -> String {
    detail
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let c = Config::default();
        assert_eq!(c.public_socket, "/run/d2b/public.sock");
        assert_eq!(c.color_artifact_path, DEFAULT_COLOR_ARTIFACT_PATH);
        assert!(c.hide_net_vms);
        assert!(c.favorites.is_empty());
        assert!(c.hidden_vms.is_empty());
        assert_eq!(c.terminal.guest_shell, "bash");
        assert_eq!(c.terminal.guest_argv, ["/run/current-system/sw/bin/foot"]);
        assert!(c.observability.enabled);
        assert_eq!(c.observability.url.as_deref(), Some("http://sys-obs:8080"));
        assert_eq!(c.observability.browser_argv, ["xdg-open"]);
        assert_eq!(
            c.observability.success_message,
            "Opened observability portal"
        );
        assert_eq!(c.theme.background, "#0f1117");
        assert_eq!(c.theme.foreground_strong, "#ffffff");
        assert!(c.quick_launch.is_empty());
    }

    #[test]
    fn empty_toml_uses_defaults() {
        let c = Config::from_toml("").expect("parse empty");
        assert_eq!(c, Config::default());
    }

    #[test]
    fn parses_favorites_and_hidden_vms() {
        let c = Config::from_toml(
            r#"
favorites = ["corp-vm", "dev-vm"]
hidden_vms = ["noisy-vm"]
"#,
        )
        .expect("parse config");
        assert_eq!(c.favorites, ["corp-vm", "dev-vm"]);
        assert_eq!(c.hidden_vms, ["noisy-vm"]);
    }

    #[test]
    fn parses_color_artifact_path() {
        let c = Config::from_toml(
            r#"
color_artifact_path = "/tmp/d2b-ui-colors.json"
"#,
        )
        .expect("parse config");
        assert_eq!(c.color_artifact_path, "/tmp/d2b-ui-colors.json");
    }

    #[test]
    fn parses_theme_palette() {
        let c = Config::from_toml(
            r##"
[theme]
background = "#010203"
surface = "#111213"
surface_alt = "#212223"
foreground = "#313233"
foreground_strong = "#414243"
foreground_disabled = "#515253"
muted = "#616263"
border = "#717273"
inverse_foreground = "#818283"
success_surface = "#919293"
warning_surface = "#a1a2a3"
error_surface = "#b1b2b3"
input_background = "#c1c2c3"
slider_track = "#d1d2d3"
"##,
        )
        .expect("parse theme");
        assert_eq!(c.theme.background, "#010203");
        assert_eq!(c.theme.slider_track, "#d1d2d3");
    }

    #[test]
    fn rejects_malformed_theme_color() {
        let err = Config::from_toml(
            r##"
[theme]
background = "#ABCDEF"
"##,
        )
        .expect_err("theme colors should be normalized lowercase hex");
        assert!(matches!(err, WlError::Config(msg) if msg.contains("theme.background")));
    }

    #[test]
    fn rejects_empty_color_artifact_path() {
        let err = Config::from_toml(r#"color_artifact_path = "" "#)
            .expect_err("empty color artifact path should fail validation");
        assert!(matches!(err, WlError::Config(msg) if msg.contains("color_artifact_path")));
    }

    #[test]
    fn missing_color_artifact_uses_no_colors() {
        let colors = load_ui_colors_from_path(Path::new("/tmp/d2b-wlcontrol-missing-colors.json"))
            .expect("missing d2b color artifact should not abort wlcontrol");
        assert_eq!(colors, None);
    }

    #[test]
    fn malformed_color_artifact_uses_no_colors() {
        let dir = std::env::temp_dir().join(format!("d2b-wlcontrol-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("ui-colors.json");
        std::fs::write(&path, "{not json").expect("write malformed");
        let colors = load_ui_colors_from_path(&path)
            .expect("malformed d2b color artifact should not abort wlcontrol");
        assert_eq!(colors, None);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn valid_color_artifact_loads() {
        let dir = std::env::temp_dir().join(format!("d2b-wlcontrol-valid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("ui-colors.json");
        std::fs::write(
            &path,
            r##"{
  "version": 1,
  "host": { "accent": "#010203" },
  "states": {
    "running": "#a6e3a1",
    "transitioning": "#f9e2af",
    "pendingRestart": "#fab387",
    "error": "#f38ba8",
    "denied": "#cba6f7",
    "unknown": "#6c7086"
  },
  "envs": { "work": { "accent": "#ffa500" } },
  "vms": {
    "work-aad": {
      "env": "work",
      "border": {
        "active": "#ffa500",
        "inactive": "#ffa500",
        "urgent": "#ffa500"
      }
    }
  }
}"##,
        )
        .expect("write valid artifact");
        let colors = load_ui_colors_from_path(&path)
            .expect("valid d2b color artifact loads")
            .expect("valid artifact should provide colors");
        assert_eq!(colors.host.accent, "#010203");
        assert_eq!(colors.envs["work"].accent, "#ffa500");
        assert_eq!(colors.vms["work-aad"].border.active, "#ffa500");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn empty_terminal_guest_command_is_rejected() {
        let err = Config::from_toml(
            r#"
[terminal]
guest_shell = ""
guest_argv = []
"#,
        )
        .expect_err("empty guest argv should fail validation");
        assert!(matches!(err, WlError::Config(msg) if msg.contains("terminal.guest_argv")));
    }

    #[test]
    fn parses_terminal_guest_argv_and_observability() {
        let c = Config::from_toml(
            r#"
[terminal]
guest_argv = ["/run/current-system/sw/bin/ghostty"]

[observability]
enabled = true
url = "http://signoz.example"
browser_argv = ["xdg-open"]
"#,
        )
        .expect("parse config");
        assert_eq!(
            c.terminal.guest_argv,
            ["/run/current-system/sw/bin/ghostty"]
        );
        assert_eq!(
            c.observability.url.as_deref(),
            Some("http://signoz.example")
        );
    }

    #[test]
    fn disabled_observability_allows_empty_browser_argv() {
        let c = Config::from_toml(
            r#"
[observability]
enabled = false
browser_argv = []
"#,
        )
        .expect("disabled observability should not require a browser");
        assert!(!c.observability.enabled);
    }

    #[test]
    fn parses_quick_launch_items() {
        let c = Config::from_toml(
            r#"
[[quick_launch]]
id = "run-openterface"
vm = "work-ssd"
icon = "desktop_windows"
tooltip = "Run Openterface"
guest_argv = ["/run/current-system/sw/bin/openterface-run"]
"#,
        )
        .expect("parse quick launch");

        assert_eq!(c.quick_launch.len(), 1);
        assert_eq!(c.quick_launch[0].id, "run-openterface");
        assert_eq!(c.quick_launch[0].vm, "work-ssd");
    }

    #[test]
    fn rejects_incomplete_quick_launch_items() {
        let err = Config::from_toml(
            r#"
[[quick_launch]]
id = "broken"
vm = "work-ssd"
icon = "desktop_windows"
tooltip = "Broken"
"#,
        )
        .expect_err("quick launch without argv should fail");
        assert!(matches!(err, WlError::Config(msg) if msg.contains("quick_launch.guest_argv")));
    }

    #[test]
    fn observability_browser_argv_is_required_when_url_is_set() {
        let err = Config::from_toml(
            r#"
[observability]
url = "http://signoz.example"
browser_argv = []
"#,
        )
        .expect_err("empty browser argv should fail validation");
        assert!(matches!(err, WlError::Config(msg) if msg.contains("observability.browser_argv")));
    }

    #[test]
    fn malformed_toml_is_config_error() {
        let err = Config::from_toml("<malformed>").expect_err("malformed toml should fail");
        assert!(matches!(err, WlError::Config(_)));
    }

    #[test]
    fn rejects_privileged_broker_socket_paths() {
        for public_socket in [
            "/run/d2b/priv.sock",
            "/run/other/priv.sock",
            "priv.sock",
            "  /run/d2b/priv.sock  ",
        ] {
            assert!(!is_public_socket_path(public_socket));
            let err = Config::from_toml(&format!("public_socket = \"{public_socket}\""))
                .expect_err("privileged broker socket path should fail");
            assert!(matches!(
                err,
                WlError::Config(msg) if msg == PRIVILEGED_BROKER_SOCKET_MESSAGE
            ));
        }
    }

    #[test]
    fn rejects_empty_public_socket() {
        let err =
            Config::from_toml("public_socket = \"\"").expect_err("empty public socket should fail");
        assert!(matches!(
            err,
            WlError::Config(msg) if msg.contains("public_socket")
        ));
        assert!(!is_public_socket_path(""));
    }

    #[test]
    fn accepts_public_socket_path() {
        assert!(is_public_socket_path("/run/d2b/public.sock"));
        Config::from_toml("public_socket = \"/run/d2b/public.sock\"")
            .expect("public socket path should validate");
    }
}
