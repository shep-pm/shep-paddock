//! Loading and unloading a model on a shep sheep.

use core::time::Duration;

use futures_util::{StreamExt as _, stream::LocalBoxStream};
use serde_json::json;
use shep_client::shep_core::status::ProcStatus;

use super::{Backends, LoadError, wait_ready};
use crate::{
    config::{Backend, Model},
    shepherd::{ProcessEvent, Shepherd},
};

// A subscription that keeps ending is asked for again at this pace, not in a tight loop.
const RESUBSCRIBE: Duration = Duration::from_secs(1);

impl<S: Shepherd> Backends<S> {
    /// Parks env and args, restarts, then waits for the ready check, or for the
    /// sheep to come online when the model has no check
    ///
    /// Restart's answer is never the signal: it may come while the sheep is
    /// still Starting, and online is not a model loaded. Neither wait has a
    /// bound of its own.
    ///
    /// # Errors
    /// [`LoadError::NotASheep`] or [`LoadError::NoUrl`] before anything is
    /// asked of the shepherd, then as [`Self::load`].
    pub(super) async fn load_sheep(&self, model: &Model) -> Result<(), LoadError> {
        let Backend::Sheep { sheep, args, env } = &model.backend else {
            return Err(LoadError::NotASheep {
                model: model.name.clone(),
            });
        };
        let ready = match (&model.ready, model.url.as_deref()) {
            (Some(ready), Some(base)) => Some((ready, base)),
            (Some(_), None) => {
                return Err(LoadError::NoUrl {
                    model: model.name.clone(),
                });
            }
            (None, _) => None,
        };
        for (key, value) in env {
            self.shepherd.set_env(sheep, key, value).await?;
        }
        if let Some(args) = args {
            self.shepherd.set_field(sheep, "args", json!(args)).await?;
        }
        match ready {
            Some((ready, base)) => {
                self.shepherd.restart(sheep).await?;
                wait_ready(&self.http, base, ready, model.key()).await
            }
            None => {
                // Taken first: a sheep with no probe is online before Restart answers.
                let online = self.shepherd.sheep_online().await?;
                let pid = self.shepherd.restart(sheep).await?;
                self.wait_online(sheep, pid, online).await
            }
        }
    }

    /// Waits for the process the restart started as `pid` to come online,
    /// reading `online` from before the restart
    ///
    /// An `online` from an earlier process of the sheep is skipped. With no
    /// `pid` any process counts. A subscription that ends may have dropped
    /// the event, so the flock is asked after subscribing again.
    ///
    /// # Errors
    /// [`LoadError::Stopped`] when the flock shows the sheep stopped or
    /// errored, and [`LoadError::Shepherd`] when a request fails.
    async fn wait_online(
        &self,
        sheep: &str,
        pid: Option<u32>,
        mut online: LocalBoxStream<'static, ProcessEvent>,
    ) -> Result<(), LoadError> {
        let ours = |seen: Option<u32>| pid.is_none() || seen == pid;
        loop {
            while let Some(event) = online.next().await {
                if event.sheep == sheep && ours(event.pid) {
                    return Ok(());
                }
            }
            tokio::time::sleep(RESUBSCRIBE).await;
            online = self.shepherd.sheep_online().await?;
            let flock = self.shepherd.list_flock().await?;
            match flock.iter().find(|row| row.name == sheep) {
                Some(row) if row.status == ProcStatus::Online && ours(row.pid) => return Ok(()),
                Some(row) if matches!(row.status, ProcStatus::Stopped | ProcStatus::Errored) => {
                    return Err(LoadError::Stopped {
                        sheep: sheep.to_owned(),
                    });
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use serde_json::json;

    use crate::{
        backend::{Backends, LoadError},
        config::Backend,
        shepherd::ShepherdError,
        test_support::{Call, FakeShepherd, fake_http, model},
    };

    fn sheep_model(env: &[(&str, &str)], args: Option<&[&str]>) -> crate::config::Model {
        let mut model = model("iq3_s");
        model.ready = None;
        model.backend = Backend::Sheep {
            sheep: "iq3_s".to_owned(),
            args: args.map(|a| a.iter().map(|s| (*s).to_owned()).collect()),
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        };
        model
    }

    #[tokio::test(start_paused = true)]
    async fn loading_sets_env_then_args_then_restarts() {
        let shepherd = FakeShepherd::new();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        let model = sheep_model(&[("A", "1"), ("B", "2")], Some(&["--ctx", "8"]));
        tokio::time::timeout(Duration::from_secs(5), backends.load(&model))
            .await
            .expect("finishes")
            .expect("loads");
        assert_eq!(
            shepherd.calls(),
            vec![
                Call::SetEnv("iq3_s".into(), "A".into(), "1".into()),
                Call::SetEnv("iq3_s".into(), "B".into(), "2".into()),
                Call::SetField("iq3_s".into(), "args".into(), json!(["--ctx", "8"])),
                Call::Restart("iq3_s".into()),
            ]
        );
    }

    // Real time: the fake server is a real loopback socket.
    #[tokio::test]
    async fn loading_waits_for_the_ready_field() {
        let (base, server) = fake_http(vec![(
            "GET",
            "/health",
            vec![(503, "starting"), (200, r#"{"loaded":["m"]}"#)],
        )]);
        let mut model = model("laya");
        model.url = Some(base);
        let shepherd = FakeShepherd::new();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        tokio::time::timeout(Duration::from_secs(10), backends.load(&model))
            .await
            .expect("finishes")
            .expect("loads");
        assert_eq!(shepherd.calls(), vec![Call::Restart("laya".into())]);
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test]
    async fn a_falsy_ready_field_is_not_ready() {
        let (base, server) = fake_http(vec![(
            "GET",
            "/health",
            vec![(200, r#"{"loaded":false}"#), (200, r#"{"loaded":true}"#)],
        )]);
        let mut model = model("iq3_s");
        model.url = Some(base);
        let backends = Backends::new(FakeShepherd::new(), crate::outbound::http_client());
        tokio::time::timeout(Duration::from_secs(10), backends.load(&model))
            .await
            .expect("finishes")
            .expect("loads");
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_model_without_a_ready_check_waits_for_its_sheep_to_come_online() {
        let shepherd = FakeShepherd::starting_restart();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        let model = sheep_model(&[], None);
        let mut load = Box::pin(backends.load(&model));

        let early = tokio::time::timeout(Duration::from_secs(60), load.as_mut()).await;
        assert!(
            early.is_err(),
            "loaded before the sheep came online: {early:?}"
        );
        shepherd.come_online("iq3_s");
        tokio::time::timeout(Duration::from_secs(5), load)
            .await
            .expect("finishes once online")
            .expect("loads");
    }

    /// A subscription that ends may have dropped the `online`, so the flock is asked.
    #[tokio::test(start_paused = true)]
    async fn an_online_missed_when_the_subscription_ends_is_read_from_the_flock() {
        let shepherd = FakeShepherd::starting_restart();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        let model = sheep_model(&[], None);
        let mut load = Box::pin(backends.load(&model));

        let early = tokio::time::timeout(Duration::from_secs(60), load.as_mut()).await;
        assert!(
            early.is_err(),
            "loaded before the sheep came online: {early:?}"
        );
        shepherd.come_online_unheard("iq3_s");
        tokio::time::timeout(Duration::from_secs(5), load)
            .await
            .expect("finishes once online")
            .expect("loads");
    }

    /// The sheep's old process comes online while the restart is under way.
    #[tokio::test(start_paused = true)]
    async fn an_online_from_the_process_before_the_restart_is_not_the_load() {
        let shepherd = FakeShepherd::starting_restart().gated();
        shepherd.running("iq3_s");
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        let model = sheep_model(&[], None);
        let mut load = Box::pin(backends.load(&model));

        let restarting = tokio::time::timeout(Duration::from_secs(1), load.as_mut()).await;
        assert!(restarting.is_err(), "the restart answered: {restarting:?}");
        shepherd.come_online("iq3_s");
        shepherd.open_gate();
        let early = tokio::time::timeout(Duration::from_secs(60), load.as_mut()).await;
        assert!(
            early.is_err(),
            "the old process's online loaded it: {early:?}"
        );
        shepherd.come_online("iq3_s");
        tokio::time::timeout(Duration::from_secs(5), load)
            .await
            .expect("finishes once the new process is online")
            .expect("loads");
    }

    /// The sheep exits for good while the subscription is down, so no `online` will come.
    #[tokio::test(start_paused = true)]
    async fn a_sheep_the_flock_shows_stopped_fails_its_load_at_once() {
        let shepherd = FakeShepherd::starting_restart();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        let model = sheep_model(&[], None);
        let mut load = Box::pin(backends.load(&model));

        let early = tokio::time::timeout(Duration::from_secs(60), load.as_mut()).await;
        assert!(
            early.is_err(),
            "loaded before the sheep came online: {early:?}"
        );
        shepherd.stop_unheard("iq3_s");
        let err = tokio::time::timeout(Duration::from_secs(5), load)
            .await
            .expect("fails without waiting out the load timeout")
            .expect_err("the sheep stopped");
        assert_eq!(
            err,
            LoadError::Stopped {
                sheep: "iq3_s".into()
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_sheep_load_of_a_model_on_ollama_is_an_error() {
        let shepherd = FakeShepherd::new();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        let err = tokio::time::timeout(
            Duration::from_secs(5),
            backends.load_sheep(&model("qwen3.8:27b")),
        )
        .await
        .expect("finishes")
        .expect_err("not a sheep");
        assert_eq!(
            err,
            LoadError::NotASheep {
                model: "qwen3.8:27b".into()
            }
        );
        assert!(shepherd.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_ready_check_with_no_url_is_an_error_before_the_restart() {
        let shepherd = FakeShepherd::new();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        let mut laya = model("laya");
        laya.url = None;
        let err = tokio::time::timeout(Duration::from_secs(5), backends.load(&laya))
            .await
            .expect("finishes")
            .expect_err("no url");
        assert_eq!(
            err,
            LoadError::NoUrl {
                model: "laya".into()
            }
        );
        assert!(shepherd.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn unloading_stops_the_sheep() {
        let shepherd = FakeShepherd::new();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        tokio::time::timeout(
            Duration::from_secs(5),
            backends.unload(&sheep_model(&[], None)),
        )
        .await
        .expect("finishes")
        .expect("unloads");
        assert_eq!(shepherd.calls(), vec![Call::Stop("iq3_s".into())]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_restart_is_a_load_error() {
        let shepherd = FakeShepherd::refusing_restart("iq3_s: no such sheep");
        let backends = Backends::new(shepherd, crate::outbound::http_client());
        let err = tokio::time::timeout(
            Duration::from_secs(5),
            backends.load(&sheep_model(&[], None)),
        )
        .await
        .expect("finishes")
        .expect_err("refused");
        assert_eq!(
            err,
            LoadError::Shepherd(ShepherdError::Refused {
                what: "iq3_s: no such sheep".into()
            })
        );
    }
}
