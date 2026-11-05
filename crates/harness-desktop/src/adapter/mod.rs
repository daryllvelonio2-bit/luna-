pub mod action;
pub mod observation;
pub mod system;
pub mod types;
pub mod unsupported;

pub use action::{ActionOps, ActionOutcome, ActionRequest};
pub use observation::ObservationOps;
pub use system::{DesktopStatus, SystemOps};
pub use types::{AppInfo, ObservedTree, WindowInfo};
pub use unsupported::UnsupportedAdapter;

/// What a platform implements.
///
/// The trait is split into three so a platform crate implements only the parts
/// it can: observation, action and system. Every method has a default that
/// refuses with `PLATFORM_NOT_SUPPORTED`, which is the honest answer for a
/// capability the platform does not have — and the reason a missing capability
/// is a refusal rather than a silent empty list.
pub trait DesktopAdapter: ObservationOps + ActionOps + SystemOps {}

impl<T: ObservationOps + ActionOps + SystemOps> DesktopAdapter for T {}
