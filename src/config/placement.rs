//! A placement is one way a model can run. The dog picks one at each load, and a running model
//! never moves.

use core::fmt;
use std::collections::{BTreeMap, BTreeSet};

use super::{
    Backend, ConfigError, Model, ModelName, PlacementName,
    section::PlacementSection,
    values::{parse_ram, parse_vram},
};
use crate::footprint::{Footprint, Host};

#[cfg(test)]
mod tests;

/// One way a model can run: what it holds there, and what it parks on its sheep.
///
/// `Debug` does not leak env values or arguments.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Placement {
    /// What the status calls it.
    pub name: PlacementName,
    /// What the model holds while loaded here.
    pub footprint: Footprint,
    /// The sheep's script here, in place of its own.
    pub script: Option<String>,
    /// The sheep's arguments here, in place of the backend's.
    pub args: Option<Vec<String>>,
    /// Environment set on the sheep here, over the backend's.
    pub env: BTreeMap<String, String>,
}

// Env values and arguments can carry credentials (IR-41), so only keys and a count are printed.
impl fmt::Debug for Placement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Placement")
            .field("name", &self.name)
            .field("footprint", &self.footprint)
            .field("script", &self.script)
            .field("arg_count", &self.args.as_ref().map(Vec::len))
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl Model {
    /// The figures `placement` declares, or the largest of each resource across them all
    /// when it is `None` or not one of this model's.
    pub fn footprint_at(&self, placement: Option<&PlacementName>) -> Footprint {
        placement
            .and_then(|name| self.placements.iter().find(|p| p.name == *name))
            .map_or(self.footprint, |p| p.footprint)
    }

    /// This model as it runs in `placement`: the placement's env set over the backend's,
    /// its `args` and `script` in place of the backend's when it sets them, and its footprint.
    pub fn placed(&self, placement: &PlacementName) -> Model {
        let mut placed = self.clone();
        let Some(chosen) = self.placements.iter().find(|p| p.name == *placement) else {
            return placed;
        };
        placed.footprint = chosen.footprint;
        if let Backend::Sheep {
            script, args, env, ..
        } = &mut placed.backend
        {
            env.extend(chosen.env.clone());
            if chosen.args.is_some() {
                args.clone_from(&chosen.args);
            }
            if chosen.script.is_some() {
                script.clone_from(&chosen.script);
            }
        }
        placed
    }
}

/// Reads `raw`, in order, refusing a repeated name, a placement bigger than the host,
/// and placements that set different sheep fields
///
/// # Errors
/// [`ConfigError::Size`], [`ConfigError::DuplicatePlacement`],
/// [`ConfigError::PlacementNeverFits`] and [`ConfigError::PlacementKeysDiffer`].
pub(super) fn build(
    model: &ModelName,
    raw: Vec<PlacementSection>,
    host: &Host,
) -> Result<Vec<Placement>, ConfigError> {
    let mut placements: Vec<Placement> = Vec::with_capacity(raw.len());
    for section in raw {
        let name = PlacementName::from(section.name);
        if placements.iter().any(|placement| placement.name == name) {
            return Err(ConfigError::DuplicatePlacement {
                model: model.clone(),
                placement: name,
            });
        }
        let field = |leaf: &str| format!("models.{model}.placements.{name}.{leaf}");
        let footprint = Footprint {
            vram: parse_vram(section.vram.as_deref(), &field("vram"))?,
            ram: parse_ram(section.ram.as_deref(), &field("ram"))?,
        };
        if !host.ever_fits(&footprint) {
            return Err(ConfigError::PlacementNeverFits {
                model: model.clone(),
                placement: name,
            });
        }
        placements.push(Placement {
            name,
            footprint,
            script: section.script,
            args: section.args,
            env: section.env,
        });
    }
    check_fields(model, &placements)?;
    Ok(placements)
}

/// Refuses placements that set different sheep fields: a field one sets would outlive it into
/// the next
///
/// # Errors
/// [`ConfigError::PlacementKeysDiffer`] naming the first two placements that differ, and how.
fn check_fields(model: &ModelName, placements: &[Placement]) -> Result<(), ConfigError> {
    let Some((first, rest)) = placements.split_first() else {
        return Ok(());
    };
    let keys = |placement: &Placement| placement.env.keys().cloned().collect::<BTreeSet<_>>();
    for other in rest {
        let what = if first.script.is_some() != other.script.is_some() {
            Some("script".to_owned())
        } else if first.args.is_some() != other.args.is_some() {
            Some("args".to_owned())
        } else {
            keys(first)
                .symmetric_difference(&keys(other))
                .next()
                .map(|key| format!("env key {key}"))
        };
        if let Some(what) = what {
            return Err(ConfigError::PlacementKeysDiffer {
                model: model.clone(),
                first: first.name.clone(),
                second: other.name.clone(),
                what,
            });
        }
    }
    Ok(())
}
