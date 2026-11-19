//! Building the models from the config and command-line flags, in one
//! place for every command: `read`, `decide`, `label`, `eval`, `serve` and
//! `mcp`.

use std::sync::Arc;

use anyhow::Result;
use spendyard::{Decider, Extractor, Pipeline};

use super::config::{self, Config};
use super::exec::Exec;

/// The extractor `flag` (or the config) names. `exec` runs a command.
pub fn extractor(config: &Config, flag: Option<&str>) -> Result<Arc<dyn Extractor>> {
    let settings = config::extractor_settings(config, flag);

    if settings.provider == "exec" {
        let exec = Exec::new(config.extractor.command.clone(), "extractor")?;

        return Ok(Arc::new(exec));
    }

    Ok(Arc::from(spendyard::extract::build(settings)?))
}

/// The first model `flag` (or the config) names, if any.
pub fn decider(config: &Config, flag: Option<&str>) -> Result<Option<Arc<dyn Decider>>> {
    let Some(settings) = config::decider_settings(config, flag) else {
        return Ok(None);
    };

    if settings.provider == "exec" {
        let exec = Exec::new(config.decider.command.clone(), "decider")?;

        return Ok(Some(Arc::new(exec)));
    }

    Ok(Some(Arc::from(spendyard::decide::build(settings)?)))
}

/// Both models and the threshold, shared between tasks.
pub struct Models {
    pub decider: Option<Arc<dyn Decider>>,
    pub extractor: Arc<dyn Extractor>,
    pub threshold: f64,
}

impl Models {
    pub fn load(
        config: &Config,
        extractor_flag: Option<&str>,
        decider_flag: Option<&str>,
    ) -> Result<Models> {
        let models = Models {
            decider: decider(config, decider_flag)?,
            extractor: extractor(config, extractor_flag)?,
            threshold: config::threshold(config),
        };

        Ok(models)
    }

    pub fn pipeline(&self) -> Pipeline<'_> {
        Pipeline {
            decider: self.decider.as_deref(),
            extractor: self.extractor.as_ref(),
            threshold: self.threshold,
        }
    }
}
