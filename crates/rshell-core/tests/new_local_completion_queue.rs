#[path = "new_local_completion_support.rs"]
pub mod fixture;

use rshell_core::{
    AppError, NewLocalTabCompletion, NewLocalTabReceiptClosed, NewLocalTabSubmitError,
    SessionFailure, SessionPort, UI_COMMAND_CAPACITY, UiCommand, UiPortError,
};
use tokio::runtime::{Builder, Runtime};

use fixture::*;

#[tokio::test]
async fn full_queue_rejects_atomically_and_shutdown_drains_before_session_wait() {
    for fail in [false, true] {
        let (app, sessions) = start().await;
        let port = app.ui_port();
        let launch = sessions.control_launch();
        let stop = sessions.control_shutdown();
        let inflight = port.try_new_local_tab_with_completion().unwrap();
        let pane = bounded(launch.started).await.unwrap();
        let queued = (0..UI_COMMAND_CAPACITY)
            .map(|_| port.try_new_local_tab_with_completion().unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(
            port.try_new_local_tab_with_completion(),
            Err(NewLocalTabSubmitError::Rejected(UiPortError::Busy))
        ));
        assert_eq!(
            port.try_send(UiCommand::NewLocalTab),
            Err(UiPortError::Busy)
        );
        assert_eq!(port.try_send(UiCommand::Shutdown), Ok(()));
        assert!(matches!(
            port.try_new_local_tab_with_completion(),
            Err(NewLocalTabSubmitError::Rejected(UiPortError::Closed))
        ));
        assert_eq!(
            port.try_send(UiCommand::NewLocalTab),
            Err(UiPortError::Closed)
        );
        launch
            .release
            .send(if fail {
                LaunchOutcome::Fail(SessionFailure::Pty)
            } else {
                LaunchOutcome::Succeed
            })
            .ok()
            .unwrap();
        let completion = receive(inflight).await;
        if fail {
            assert_eq!(completion, NewLocalTabCompletion::NoCreation);
        } else {
            let NewLocalTabCompletion::Created(id) = completion else {
                panic!("inflight must publish")
            };
            assert_eq!(id.pane, pane);
            assert_published(&app, &sessions, id);
        }
        bounded(stop.started).await.unwrap();
        for receipt in queued {
            assert_eq!(bounded(receipt).await, Err(NewLocalTabReceiptClosed));
        }
        assert_eq!(
            sessions.attempts().len(),
            2,
            "rejection/queued shutdown must never launch"
        );
        assert_eq!(
            port.try_send(UiCommand::Shutdown),
            Ok(()),
            "old shutdown ordering before finish is retained"
        );
        stop.release.send(()).unwrap();
        bounded(app.shutdown()).await.unwrap();
        assert_eq!(sessions.recording.live_session_count(), 0);
        assert_eq!(port.try_send(UiCommand::Shutdown), Err(UiPortError::Closed));
        assert!(matches!(
            port.try_new_local_tab_with_completion(),
            Err(NewLocalTabSubmitError::Rejected(UiPortError::Closed))
        ));
    }
}

fn runtime() -> Runtime {
    Builder::new_current_thread().enable_time().build().unwrap()
}

#[test]
fn task_cancellation_closes_inflight_and_buffered_receipts_with_external_port_held() {
    let executor = runtime();
    let (app, sessions) = executor.block_on(start());
    let port = app.ui_port();
    let gate = sessions.control_launch();
    let inflight = port.try_new_local_tab_with_completion().unwrap();
    executor.block_on(bounded(gate.started)).unwrap();
    let b = port.try_new_local_tab_with_completion().unwrap();
    let c = port.try_new_local_tab_with_completion().unwrap();
    drop(executor);
    runtime().block_on(async {
        for receipt in [inflight, b, c] {
            assert_eq!(bounded(receipt).await, Err(NewLocalTabReceiptClosed));
        }
        assert_eq!(port.try_send(UiCommand::Shutdown), Err(UiPortError::Closed));
        assert!(matches!(
            port.try_new_local_tab_with_completion(),
            Err(NewLocalTabSubmitError::Rejected(UiPortError::Closed))
        ));
        assert_eq!(bounded(app.shutdown()).await, Err(AppError::Closed));
        sessions.shutdown_all().await.unwrap();
        assert_eq!(sessions.recording.live_session_count(), 0);
    });
}

#[test]
fn unpolled_loop_cancellation_also_drains_buffered_receipts() {
    let executor = runtime();
    let (app, sessions) = executor.block_on(start());
    let port = app.ui_port();
    let receipts = (0..3)
        .map(|_| port.try_new_local_tab_with_completion().unwrap())
        .collect::<Vec<_>>();
    drop(executor);
    runtime().block_on(async {
        for receipt in receipts {
            assert_eq!(bounded(receipt).await, Err(NewLocalTabReceiptClosed));
        }
        assert_eq!(sessions.attempts().len(), 1, "loop has never dispatched");
        assert_eq!(
            port.try_send(UiCommand::NewLocalTab),
            Err(UiPortError::Closed)
        );
        sessions.shutdown_all().await.unwrap();
    });
}

#[tokio::test]
async fn producer_panic_closes_inflight_and_buffered_receipts_with_external_port_held() {
    let (app, sessions) = start().await;
    let port = app.ui_port();
    let gate = sessions.control_launch();
    let inflight = port.try_new_local_tab_with_completion().unwrap();
    bounded(gate.started).await.unwrap();
    let b = port.try_new_local_tab_with_completion().unwrap();
    let c = port.try_new_local_tab_with_completion().unwrap();
    gate.release.send(LaunchOutcome::Panic).ok().unwrap();
    for receipt in [inflight, b, c] {
        assert_eq!(bounded(receipt).await, Err(NewLocalTabReceiptClosed));
    }
    assert_eq!(
        port.try_send(UiCommand::NewLocalTab),
        Err(UiPortError::Closed)
    );
    assert_eq!(port.try_send(UiCommand::Shutdown), Err(UiPortError::Closed));
    assert_eq!(bounded(app.shutdown()).await, Err(AppError::Closed));
    assert_eq!(sessions.attempts().len(), 2);
    sessions.shutdown_all().await.unwrap();
    assert_eq!(sessions.recording.live_session_count(), 0);
}
