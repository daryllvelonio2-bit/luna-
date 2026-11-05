mod code;
mod disposition;
mod failure;

pub use code::ErrorCode;
pub use disposition::{DeliveryDisposition, DeliverySemantics, RetryDisposition};
pub use failure::DesktopError;
