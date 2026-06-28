//! `wlcontrol-core` — the frozen domain contract, configuration, reducer, and
//! action planner shared by every d2b-wlcontrol surface.
//!
//! See [`model`] for the cross-crate contract that downstream fleet agents
//! build against.

pub mod config;
pub mod error;
pub mod model;
pub mod plan;
pub mod reduce;
pub mod sources;

pub use config::{
    is_public_socket_path, load_ui_colors_from_path, Config, UiColorArtifact, UiColorBorder,
    UiColorEnv, UiColorHost, UiColorStates, UiColorVm, DEFAULT_COLOR_ARTIFACT_PATH,
};
pub use error::{WlError, WlResult};
pub use model::{
    ActionAvailability, ActionKind, AuthRole, Connectivity, PlannedAction, RuntimeState,
    SocketIntent, Unavailable, UsbClaim, Vm, VmFeatures, WlState,
};
