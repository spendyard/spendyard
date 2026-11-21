//! The first models: JEV, Clef and SystemOne, one client for every model
//! that speaks JEV's request format.

mod systemone;

// What the provider builds on, as it had it beside it before the split.
#[allow(unused_imports)]
use spendyard_core::decide::*;
use spendyard_core::extract::ExtractError;

/// Builds the decider `settings` describe.
pub fn build(settings: Settings) -> Result<Box<dyn Decider>, ExtractError> {
    let decider = systemone::SystemOne::new(settings)?;

    Ok(Box::new(decider))
}
