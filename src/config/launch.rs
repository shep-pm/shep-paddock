//! The settings a sheep can only take by restarting, which tell whether a loaded model is stale.

use core::{fmt, num::NonZeroU32};
use std::collections::BTreeMap;

use super::{Backend, Model, PlacementName};

/// What a sheep model was started with: the sheep, what is parked on it before it starts, and the
/// turns and container its footprint counts.
///
/// Settings the dog applies without the backend, such as `idle`, `excludes` and `url`, are not here.
/// `Debug` does not leak env values or arguments.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    sheep: String,
    script: Option<String>,
    args: Option<Vec<String>>,
    env: BTreeMap<String, String>,
    turn_ram: Option<u64>,
    sequences: Option<NonZeroU32>,
    container: Option<String>,
}

// Env values and arguments, such as an --api-key, can carry credentials (IR-41), so only the
// env keys and the argument count are printed, as `Backend`'s `Debug` does.
impl fmt::Debug for Launch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Launch")
            .field("sheep", &self.sheep)
            .field("script", &self.script)
            .field("arg_count", &self.args.as_ref().map(Vec::len))
            .field("env_keys", &self.env.keys().collect::<Vec<_>>())
            .field("turn_ram", &self.turn_ram)
            .field("sequences", &self.sequences)
            .field("container", &self.container)
            .finish()
    }
}

impl Model {
    /// How this model starts as it runs in `placement`, or `None` for one that is not on a sheep
    ///
    /// A `placement` that is `None`, or not one of this model's, is the model as it is written.
    pub fn launch_in(&self, placement: Option<&PlacementName>) -> Option<Launch> {
        let placed = match placement {
            Some(placement) => self.placed(placement),
            None => self.clone(),
        };
        let Backend::Sheep {
            sheep,
            script,
            args,
            env,
            ..
        } = placed.backend
        else {
            return None;
        };
        Some(Launch {
            sheep,
            script,
            args,
            env,
            turn_ram: placed.turn_ram,
            sequences: placed.sequences,
            container: placed.container,
        })
    }
}
