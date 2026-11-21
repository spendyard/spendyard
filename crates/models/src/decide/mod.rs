//! The first models: JEV, Clef and SystemOne, one client for every model
//! that speaks JEV's request format.

mod systemone;

// What the provider builds on, as it had it beside it before the split.
#[allow(unused_imports)]
use spendyard_core::decide::*;
use spendyard_core::extract::ExtractError;

use std::sync::Arc;

use crate::extract::Transport;

/// Builds the decider `settings` describe, posting natively.
#[cfg(feature = "reqwest")]
pub fn build(settings: Settings) -> Result<Box<dyn Decider>, ExtractError> {
    build_with(settings, crate::extract::http::native())
}

/// Builds the decider `settings` describe, posting through `transport`: a
/// host's own, in WebAssembly.
pub fn build_with(
    settings: Settings,
    transport: Arc<dyn Transport>,
) -> Result<Box<dyn Decider>, ExtractError> {
    let decider = systemone::SystemOne::new(settings, transport)?;

    Ok(Box::new(decider))
}
