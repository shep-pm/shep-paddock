use core::time::Duration;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::{InFlight, OneSmi, arguments, exec, smi};

const PID: u32 = 190_784;

/// A derived `Debug` would print the arguments, and one can carry a key.
#[test]
fn args_debug_counts_the_arguments_and_prints_none() {
    let read = super::Args::Read(vec!["serve".to_owned(), "--api-key=hunter2".to_owned()]);
    assert_eq!(format!("{read:?}"), "Read(2 arguments)");
    assert_eq!(format!("{:?}", super::Args::Unknown), "Unknown");
}

// Real time: the read runs on a blocking-pool thread, which a paused clock does not wait for.
#[tokio::test]
async fn a_read_that_hangs_is_given_up_on() {
    let reads = InFlight::default();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let read = reads.read(PID, Duration::from_millis(50), move || held.recv().ok());

    let got = tokio::time::timeout(Duration::from_secs(5), read).await;

    assert_eq!(got, Ok(None), "given up on, not waited for");
    release.send(()).expect("the stuck read still waits");
}

// Real time, as above.
#[tokio::test]
async fn a_pid_whose_read_is_stuck_is_not_read_again_until_it_returns() {
    let reads = InFlight::default();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let stuck = reads.read(PID, Duration::from_millis(50), move || held.recv().ok());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), stuck).await,
        Ok(None)
    );

    let ran = Arc::new(AtomicBool::new(false));
    let again = {
        let ran = Arc::clone(&ran);
        reads.read(PID, Duration::from_secs(5), move || {
            ran.store(true, Ordering::SeqCst);
            Some(())
        })
    };
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), again).await,
        Ok(None)
    );
    assert!(
        !ran.load(Ordering::SeqCst),
        "no second thread for a stuck pid"
    );
    let other = reads.read(PID + 1, Duration::from_secs(5), || Some(1));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), other).await,
        Ok(Some(1))
    );

    release.send(()).expect("the stuck read still waits");
    let freed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if reads.read(PID, Duration::from_secs(5), || Some(2)).await == Some(2) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(freed.is_ok(), "read again once the stuck read returned");
}

// Real time, as above.
#[tokio::test]
async fn a_read_that_answers_is_returned() {
    let reads = InFlight::default();
    let read = reads.read(PID, Duration::from_secs(5), || Some(7));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), read).await,
        Ok(Some(7))
    );
}

// Real time, as above. The panic's message on stderr is expected.
#[tokio::test]
async fn a_read_that_panics_does_not_leave_its_pid_unread() {
    let reads = InFlight::default();
    let panics = reads.read(PID, Duration::from_secs(5), || {
        let parsed: u32 = "no number".parse().expect("a read that panics");
        Some(parsed)
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), panics).await,
        Ok(None)
    );

    let next = reads.read(PID, Duration::from_secs(5), || Some(3));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), next).await,
        Ok(Some(3))
    );
}

// Real time: the query runs as a task the test cannot step through.
#[tokio::test]
async fn a_query_still_running_is_not_started_again_until_it_ends() {
    let smis = OneSmi::default();
    let (release, held) = tokio::sync::oneshot::channel::<()>();
    let stuck = smis.run(Duration::from_millis(50), async { held.await.ok() });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), stuck).await,
        Ok(None)
    );

    let ran = Arc::new(AtomicBool::new(false));
    let again = {
        let ran = Arc::clone(&ran);
        smis.run(Duration::from_secs(5), async move {
            ran.store(true, Ordering::SeqCst);
            Some(())
        })
    };
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), again).await,
        Ok(None)
    );
    assert!(
        !ran.load(Ordering::SeqCst),
        "no second nvidia-smi beside a stuck one"
    );

    release.send(()).expect("the stuck query still waits");
    let freed = tokio::time::timeout(Duration::from_secs(5), async {
        while smis.run(Duration::from_secs(5), async { Some(2) }).await != Some(2) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(freed.is_ok(), "queried again once the stuck query ended");
}

// Real time, as above. The panic's message on stderr is expected.
#[tokio::test]
async fn a_query_that_panics_does_not_leave_the_gpu_unread() {
    let smis = OneSmi::default();
    let panics = smis.run(Duration::from_secs(5), async {
        let parsed: u32 = "no number".parse().expect("a query that panics");
        Some(parsed)
    });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), panics).await,
        Ok(None)
    );

    let next = smis.run(Duration::from_secs(5), async { Some(3) });
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), next).await,
        Ok(Some(3))
    );
}

// Real time: a real process, given up on after half a second and then killed.
#[cfg(unix)]
#[tokio::test]
async fn a_hung_query_is_given_up_on_then_killed_and_reaped() {
    let dir = tempfile::tempdir().expect("scratch directory");
    let pid_file = dir.path().join("pid");
    // The query runs as a task of its own, so what it borrows must live forever.
    let path: &'static str = Box::leak(pid_file.display().to_string().into_boxed_str());
    let args: &'static [&'static str] = Box::leak(Box::new([
        "-c",
        "echo $$ > \"$1\"; exec sleep 30",
        "sh",
        path,
    ]));
    let smis = OneSmi::default();
    let hung = smis.run(
        Duration::from_millis(500),
        smi("sh".as_ref(), args, Duration::from_millis(500)),
    );
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), hung).await,
        Ok(None)
    );
    let pid = std::fs::read_to_string(&pid_file).expect("the query wrote its pid");
    // `kill -0` finds a zombie too, so its failing means killed and reaped.
    let gone = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let alive = tokio::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .is_ok_and(|status| status.success());
            if !alive {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(gone.is_ok(), "the hung query was killed and reaped");

    let freed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let echo = smi("sh".as_ref(), &["-c", "echo up"], Duration::from_secs(5));
            if smis.run(Duration::from_secs(5), echo).await.as_deref() == Some("up\n") {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        freed.is_ok(),
        "the killed query was reaped, so another runs"
    );
}

#[test]
fn cmdline_bytes_split_at_each_nul() {
    assert_eq!(
        arguments(b"/usr/bin/llama-server\0--model\0/m/blobs/sha256-ab\0"),
        ["/usr/bin/llama-server", "--model", "/m/blobs/sha256-ab"]
    );
    assert!(arguments(b"").is_empty());
}

// Real time: a real process.
#[cfg(unix)]
#[tokio::test]
async fn exec_gives_the_exit_code_and_what_was_printed() {
    let ran = exec(
        "sh".as_ref(),
        &["-c", "echo up; exit 1"],
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(ran, Some((Some(1), "up\n".to_owned())));
    let missing = exec(
        "paddock-no-such-program".as_ref(),
        &[],
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(missing, None);
}
