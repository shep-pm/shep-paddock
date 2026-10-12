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

// The keys a sheep script reads to size its backend's cache, set after the model's own env.
const SLOTS_KEY: &str = "PADDOCK_SLOTS";
const TURN_RAM_MIB_KEY: &str = "PADDOCK_TURN_RAM_MIB";
const MIB: u64 = 1024 * 1024;

// A subscription that keeps ending is asked for again at this pace, not in a tight loop.
const RESUBSCRIBE: Duration = Duration::from_secs(1);

impl<S: Shepherd> Backends<S> {
    /// Parks env, the slots and turn RAM when the model has `turn_ram`, script and args, restarts, then waits for the ready check, or for the
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
        let Backend::Sheep {
            sheep,
            script,
            args,
            env,
            ..
        } = &model.backend
        else {
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
        if let Some(turn_ram) = model.turn_ram {
            let slots = model.sequences.map_or(1, |n| n.get().saturating_add(1));
            self.shepherd
                .set_env(sheep, SLOTS_KEY, &slots.to_string())
                .await?;
            let mib = turn_ram.div_ceil(MIB);
            self.shepherd
                .set_env(sheep, TURN_RAM_MIB_KEY, &mib.to_string())
                .await?;
        }
        if let Some(script) = script {
            self.shepherd
                .set_field(sheep, "script", json!(script))
                .await?;
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
    use core::{num::NonZeroU32, time::Duration};
    use serde_json::json;

    use crate::{
        backend::{Backends, LoadError},
        config::Backend,
        shepherd::ShepherdError,
        test_support::{Call, FakeShepherd, fake_http, model},
    };

    const MIB_BYTES: u64 = 1024 * 1024;

    fn sheep_model(env: &[(&str, &str)], args: Option<&[&str]>) -> crate::config::Model {
        let mut model = model("iq3_s");
        model.ready = None;
        model.backend = Backend::Sheep {
            sheep: "iq3_s".to_owned(),
            name: None,
            script: None,
            args: args.map(|a| a.iter().map(|s| (*s).to_owned()).collect()),
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        };
        model
    }

    #[tokio::test(start_paused = true)]
    async fn loading_sets_env_then_script_then_args_then_restarts() {
        let shepherd = FakeShepherd::new();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        let mut model = sheep_model(&[("A", "1")], Some(&["--ctx", "8"]));
        if let Backend::Sheep { script, .. } = &mut model.backend {
            *script = Some("/opt/serve".to_owned());
        }
        tokio::time::timeout(Duration::from_secs(5), backends.load(&model))
            .await
            .expect("finishes")
            .expect("loads");
        assert_eq!(
            shepherd.calls(),
            vec![
                Call::SetEnv("iq3_s".into(), "A".into(), "1".into()),
                Call::SetField("iq3_s".into(), "script".into(), json!("/opt/serve")),
                Call::SetField("iq3_s".into(), "args".into(), json!(["--ctx", "8"])),
                Call::Restart("iq3_s".into()),
            ]
        );
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

    /// The shepherd calls made loading `model`.
    async fn calls_loading(model: &crate::config::Model) -> Vec<Call> {
        let shepherd = FakeShepherd::new();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        tokio::time::timeout(Duration::from_secs(5), backends.load(model))
            .await
            .expect("finishes")
            .expect("loads");
        shepherd.calls()
    }

    fn turns_model(
        env: &[(&str, &str)],
        sequences: u32,
        turn_ram: Option<u64>,
    ) -> crate::config::Model {
        let mut model = sheep_model(env, Some(&["--ctx", "8"]));
        model.sequences = NonZeroU32::new(sequences);
        model.turn_ram = turn_ram;
        model
    }

    #[tokio::test(start_paused = true)]
    async fn a_model_with_turn_ram_parks_slots_and_turn_ram_mib_after_its_env() {
        let model = turns_model(&[("A", "1")], 2, Some(4 * 1024 * 1024 * 1024));
        assert_eq!(
            calls_loading(&model).await,
            vec![
                Call::SetEnv("iq3_s".into(), "A".into(), "1".into()),
                Call::SetEnv("iq3_s".into(), "PADDOCK_SLOTS".into(), "3".into()),
                Call::SetEnv("iq3_s".into(), "PADDOCK_TURN_RAM_MIB".into(), "4096".into()),
                Call::SetField("iq3_s".into(), "args".into(), json!(["--ctx", "8"])),
                Call::Restart("iq3_s".into()),
            ]
        );
    }

    fn parked(calls: &[Call], key: &str) -> Option<String> {
        calls.iter().find_map(|call| match call {
            Call::SetEnv(_, k, v) if k == key => Some(v.clone()),
            _ => None,
        })
    }

    #[tokio::test(start_paused = true)]
    async fn slots_is_sequences_plus_one() {
        for (sequences, slots) in [(1, "2"), (4, "5")] {
            let calls = calls_loading(&turns_model(&[], sequences, Some(MIB_BYTES))).await;
            assert_eq!(parked(&calls, "PADDOCK_SLOTS").as_deref(), Some(slots));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn turn_ram_mib_rounds_a_fraction_up() {
        let calls = calls_loading(&turns_model(&[], 1, Some(1536 * 1024))).await;
        assert_eq!(parked(&calls, "PADDOCK_TURN_RAM_MIB").as_deref(), Some("2"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_model_without_turn_ram_parks_no_paddock_keys() {
        let calls = calls_loading(&turns_model(&[("A", "1")], 2, None)).await;
        assert_eq!(
            calls,
            vec![
                Call::SetEnv("iq3_s".into(), "A".into(), "1".into()),
                Call::SetField("iq3_s".into(), "args".into(), json!(["--ctx", "8"])),
                Call::Restart("iq3_s".into()),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_placed_model_parks_them_too() {
        let toml = crate::test_support::placed_toml().replace(
            "prefix = \"/laya\"",
            "prefix = \"/laya\"\nsequences = 2\nturn_ram = \"512M\"",
        );
        let config = crate::test_support::config(&toml);
        let laya = crate::config::ModelName::from("laya");
        let mut placed = config.models[&laya].placed(&"ram".into());
        placed.ready = None;
        let calls = calls_loading(&placed).await;
        assert_eq!(parked(&calls, "PADDOCK_SLOTS").as_deref(), Some("3"));
        assert_eq!(
            parked(&calls, "PADDOCK_TURN_RAM_MIB").as_deref(),
            Some("512")
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_parked_keys_win_over_an_env_key_of_the_same_name() {
        let model = turns_model(
            &[("PADDOCK_SLOTS", "9"), ("PADDOCK_TURN_RAM_MIB", "9")],
            2,
            Some(MIB_BYTES),
        );
        let sets: Vec<(String, String)> = calls_loading(&model)
            .await
            .into_iter()
            .filter_map(|call| match call {
                Call::SetEnv(_, k, v) => Some((k, v)),
                _ => None,
            })
            .collect();
        assert_eq!(
            sets.last_chunk::<2>().expect("four sets"),
            &[
                ("PADDOCK_SLOTS".to_owned(), "3".to_owned()),
                ("PADDOCK_TURN_RAM_MIB".to_owned(), "1".to_owned()),
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

    // Real time: the fake server is a real loopback socket.
    #[tokio::test]
    async fn the_ready_poll_starts_only_after_the_restart_answers() {
        let (base, server) =
            fake_http(vec![("GET", "/health", vec![(200, r#"{"loaded":["m"]}"#)])]);
        let mut model = model("laya");
        model.url = Some(base);
        let shepherd = FakeShepherd::new().gated();
        let backends = Backends::new(shepherd.clone(), crate::outbound::http_client());
        let mut load = Box::pin(backends.load(&model));

        let early = tokio::time::timeout(Duration::from_millis(300), load.as_mut()).await;
        assert!(early.is_err(), "the restart has not answered: {early:?}");
        assert_eq!(server.seen().len(), 0, "polled before the restart answered");

        shepherd.open_gate();
        tokio::time::timeout(Duration::from_secs(10), load)
            .await
            .expect("finishes once the restart answers")
            .expect("loads");
        assert_eq!(server.seen().len(), 1);
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
