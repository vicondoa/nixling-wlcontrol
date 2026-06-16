//! User configuration for nixling-wlcontrol.
//!
//! Owning wave: **Wave 1 — Core model agent**. Wave 0 ships a minimal,
//! compiling skeleton with sane defaults and the on-disk location contract.
//! The Wave 1 agent fleshes out validation, terminal-argv parsing rules,
//! favorites/ordering, and the full option surface described in the plan.

use serde::{Deserialize, Serialize};

use crate::error::{WlError, WlResult};

/// Default config file location: `${XDG_CONFIG_HOME:-~/.config}/nixling-wlcontrol/config.toml`.
pub const CONFIG_RELATIVE_PATH: &str = "nixling-wlcontrol/config.toml";

const PRIVILEGED_BROKER_SOCKET_MESSAGE: &str =
    "refusing to use the privileged broker socket; nixling-wlcontrol speaks only the public socket";

/// Returns true when `path` is acceptable as a nixling public-socket path.
///
/// This intentionally rejects the privileged broker socket by both its exact
/// canonical path and by basename so downstream protocol clients can share the
/// same fail-closed guard before connecting.
pub fn is_public_socket_path(path: &str) -> bool {
    let path = path.trim();
    if path.is_empty() || path == "/run/nixling/priv.sock" {
        return false;
    }

    std::path::Path::new(path).file_name() != Some(std::ffi::OsStr::new("priv.sock"))
}

/// Top-level configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct Config {
    /// Path to the nixling public socket.
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
}

/// Terminal launch configuration. The terminal command is always an argv
/// vector; no shell string interpolation is performed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct TerminalConfig {
    /// argv prefix used to spawn a terminal, e.g. `["foot", "--"]`.
    pub argv: Vec<String>,
    /// Guest shell to run inside the VM, e.g. `bash`.
    pub guest_shell: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            public_socket: "/run/nixling/public.sock".to_owned(),
            refresh_interval_ms: 2500,
            command_timeout_ms: 4000,
            hide_net_vms: true,
            show_pending_restart: true,
            favorites: Vec::new(),
            hidden_vms: Vec::new(),
            terminal: TerminalConfig::default(),
        }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            argv: vec!["foot".to_owned(), "--".to_owned()],
            guest_shell: "bash".to_owned(),
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
        if self.terminal.argv.is_empty() {
            return Err(WlError::Config(
                "terminal.argv must contain at least one argv element".into(),
            ));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let c = Config::default();
        assert_eq!(c.public_socket, "/run/nixling/public.sock");
        assert!(c.hide_net_vms);
        assert!(c.favorites.is_empty());
        assert!(c.hidden_vms.is_empty());
        assert_eq!(c.terminal.guest_shell, "bash");
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
    fn empty_terminal_argv_is_rejected() {
        let err = Config::from_toml(
            r#"
[terminal]
argv = []
"#,
        )
        .expect_err("empty argv should fail validation");
        assert!(matches!(err, WlError::Config(msg) if msg.contains("terminal.argv")));
    }

    #[test]
    fn malformed_toml_is_config_error() {
        let err = Config::from_toml("<malformed>").expect_err("malformed toml should fail");
        assert!(matches!(err, WlError::Config(_)));
    }

    #[test]
    fn rejects_privileged_broker_socket_paths() {
        for public_socket in [
            "/run/nixling/priv.sock",
            "/run/other/priv.sock",
            "priv.sock",
            "  /run/nixling/priv.sock  ",
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
        assert!(is_public_socket_path("/run/nixling/public.sock"));
        Config::from_toml("public_socket = \"/run/nixling/public.sock\"")
            .expect("public socket path should validate");
    }
}
