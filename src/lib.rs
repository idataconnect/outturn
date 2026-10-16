//! The whole platform under one name, for the integration tests and the tools
//! that drive more than one tier. The tiers themselves are crates of their
//! own (`crates/`), so a change to one does not rebuild the others.

pub use outturn_api as api;
pub use outturn_core::*;
pub use outturn_gateway as gateway;
pub use outturn_runtime as runtime;
