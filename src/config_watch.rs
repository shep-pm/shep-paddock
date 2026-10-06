//! Follows the dog's own section of `dogs.toml` and applies each valid change.

use core::fmt;
use std::{net::SocketAddr, sync::Arc, time::Duration};

use futures_util::StreamExt as _;
use shep_client::dogs::{Interrupted, Stop};
use tokio::{sync::watch, time::Instant};

use crate::{
    config::{Config, ConfigError},
    engine::EngineHandle,
    shepherd::{Shepherd, ShepherdError},
};

/// The least time between two subscriptions, so a connection that keeps dying is not asked again
/// in a tight loop. The engine paces its own subscription the same way.
const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(1);

/// Why a changed section was not applied
#[derive(Debug)]
pub(crate) enum ReloadError {
    /// The shepherd could not give the section.
    Read(ShepherdError),
    /// The section does not validate.
    Invalid(ConfigError),
}

impl fmt::Display for ReloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(err) => write!(
                f,
                "reading the section failed, keeping the old config: {err}"
            ),
            Self::Invalid(err) => write!(
                f,
                "the new section is not valid, keeping the old config: {err}"
            ),
        }
    }
}

impl core::error::Error for ReloadError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Read(err) => Some(err),
            Self::Invalid(err) => Some(err),
        }
    }
}

/// A changed `listen`, which the endpoint cannot follow without a restart
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListenNeedsRestart {
    bound: SocketAddr,
    wanted: SocketAddr,
}

impl fmt::Display for ListenNeedsRestart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "listen changed from {} to {}, which needs a restart; still listening on {}",
            self.bound, self.wanted, self.bound
        )
    }
}

/// Reads the section and applies it if it differs from `last`
///
/// The applied config keeps the `listen` the endpoint is bound to, so it says what the dog
/// does. The engine hears of the new config before the endpoint does, so no request is routed to a
/// model the engine does not know.
///
/// Returns the notice for a changed `listen`, if the section changed it.
///
/// # Errors
/// [`ReloadError::Read`] when the shepherd cannot give the section and [`ReloadError::Invalid`]
/// when it does not validate. The running config is left as it was in both cases.
async fn reload<S: Shepherd>(
    shepherd: &S,
    dog: &str,
    last: &mut String,
    engine: &EngineHandle,
    config: &watch::Sender<Arc<Config>>,
) -> Result<Option<ListenNeedsRestart>, ReloadError> {
    let text = shepherd.dog_config(dog).await.map_err(ReloadError::Read)?;
    if text == *last {
        return Ok(None);
    }
    // Remembered even when invalid, so the same bad text is not judged twice.
    last.clone_from(&text);
    let mut next = Config::from_toml(&text).map_err(ReloadError::Invalid)?;
    let bound = config.borrow().listen;
    let notice = (next.listen != bound).then_some(ListenNeedsRestart {
        bound,
        wanted: next.listen,
    });
    next.listen = bound;
    let applied = Arc::new(next);
    engine.reconfigure(Arc::clone(&applied)).await;
    config.send_replace(applied);
    Ok(notice)
}

/// Follows the section until a stop is requested
///
/// `current` is the section text the running config came from. A changed section that validates
/// goes to the engine first and then to the endpoint, and one that does not is logged and left.
/// Each new subscription reads the section once, so a change made while it was down is applied.
pub(crate) async fn watch<S: Shepherd>(
    shepherd: &S,
    dog: &str,
    current: String,
    engine: &EngineHandle,
    config: &watch::Sender<Arc<Config>>,
    mut stop: Stop,
) {
    let mut last = current;
    loop {
        let opened = Instant::now();
        match shepherd.config_changes(dog).await {
            Ok(mut changes) => {
                let mut changed = true;
                loop {
                    if core::mem::take(&mut changed) {
                        match reload(shepherd, dog, &mut last, engine, config).await {
                            Ok(Some(notice)) => eprintln!("paddock: {notice}"),
                            Ok(None) => {}
                            Err(err) => eprintln!("paddock: {err}"),
                        }
                    }
                    tokio::select! {
                        biased;
                        () = stop.wait() => return,
                        change = changes.next() => match change {
                            Some(()) => changed = true,
                            None => break,
                        },
                    }
                }
                eprintln!("paddock: config changes ended; subscribing again");
            }
            Err(err) => eprintln!("paddock: subscribing to config changes failed: {err}"),
        }
        let wait = RESUBSCRIBE_DELAY.saturating_sub(opened.elapsed());
        if stop.sleep(wait).await == Interrupted::Yes {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
