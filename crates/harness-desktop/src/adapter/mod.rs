pub mod action;
pub mod input;
pub mod observation;
pub mod system;
pub mod types;
pub mod unsupported;

pub use action::{ActionOps, ActionOutcome, ActionRequest};
pub use input::{InputOps, ScreenshotInfo, WindowRef};
pub use observation::ObservationOps;
pub use system::{DesktopStatus, SystemOps};
pub use types::{AppInfo, ObservedTree, WindowInfo};
pub use unsupported::UnsupportedAdapter;

/// What a platform implements.
///
/// The trait is split into four so a platform crate implements only the parts
/// it can: observation, action, input and system. Every method has a default
/// that refuses with `PLATFORM_NOT_SUPPORTED`, which is the honest answer for a
/// capability the platform does not have — and the reason a missing capability
/// is a refusal rather than a silent empty list.
pub trait DesktopAdapter: ObservationOps + ActionOps + InputOps + SystemOps {}

impl<T: ObservationOps + ActionOps + InputOps + SystemOps> DesktopAdapter for T {}
