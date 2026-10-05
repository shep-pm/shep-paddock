//! Loading and unloading a model on a shep sheep.

use serde_json::json;

use super::{Backends, LoadError, wait_ready};
use crate::{
    config::{Backend, Model},
    shepherd::Shepherd,
};

impl<S: Shepherd> Backends<S> {
    /// Parks env and args, restarts, then waits for the ready check when the model has one.
    ///
    /// Restart's answer is not "loaded": shep calls a sheep online after its probe or its
    /// `listen_timeout`, so the ready check is the only signal.
    pub(super) async fn load_sheep(&self, model: &Model) -> Result<(), LoadError> {
        let Backend::Sheep { sheep, args, env } = &model.backend else {
            return Ok(());
        };
        for (key, value) in env {
            self.shepherd.set_env(sheep, key, value).await?;
        }
        if let Some(args) = args {
            self.shepherd.set_field(sheep, "args", json!(args)).await?;
        }
        self.shepherd.restart(sheep).await?;
        match &model.ready {
            Some(ready) => {
                let base = model.url.as_deref().unwrap_or_default();
                wait_ready(&self.http, base, ready, model.key()).await
            }
            None => Ok(()),
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
        let backends = Backends::new(shepherd.clone(), reqwest::Client::new());
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
        let backends = Backends::new(shepherd.clone(), reqwest::Client::new());
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
        let backends = Backends::new(FakeShepherd::new(), reqwest::Client::new());
        tokio::time::timeout(Duration::from_secs(10), backends.load(&model))
            .await
            .expect("finishes")
            .expect("loads");
        assert_eq!(server.seen().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn unloading_stops_the_sheep() {
        let shepherd = FakeShepherd::new();
        let backends = Backends::new(shepherd.clone(), reqwest::Client::new());
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
        let backends = Backends::new(shepherd, reqwest::Client::new());
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
