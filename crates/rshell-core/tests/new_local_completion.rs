#[path = "new_local_completion_support.rs"]
pub mod fixture;

use std::sync::Mutex;

use rshell_core::{
    AppEvent, AppFailureCategory, NewLocalTabCompletion, NewLocalTabReceipt,
    NewLocalTabSubmitError, PaneId, SessionFailure, UiCommand, UiCommandPort, UiPortError,
};

use fixture::*;

#[derive(Default)]
struct LegacyPort(Mutex<Vec<UiCommand>>);

impl UiCommandPort for LegacyPort {
    fn try_send(&self, command: UiCommand) -> Result<(), UiPortError> {
        self.0.lock().unwrap().push(command);
        Ok(())
    }
}

struct CustomPort;

impl UiCommandPort for CustomPort {
    fn try_send(&self, _: UiCommand) -> Result<(), UiPortError> {
        panic!("not used")
    }

    fn try_new_local_tab_with_completion(
        &self,
    ) -> Result<NewLocalTabReceipt, NewLocalTabSubmitError> {
        Ok(Box::pin(async { Ok(NewLocalTabCompletion::NoCreation) }))
    }
}

#[tokio::test]
async fn legacy_is_unsupported_without_enqueue_and_receipts_allow_custom_futures() {
    let legacy = LegacyPort::default();
    assert!(matches!(
        legacy.try_new_local_tab_with_completion(),
        Err(NewLocalTabSubmitError::Unsupported)
    ));
    assert!(legacy.0.lock().unwrap().is_empty());
    legacy.try_send(UiCommand::NewLocalTab).unwrap();
    assert!(matches!(
        legacy.0.lock().unwrap().as_slice(),
        [UiCommand::NewLocalTab]
    ));
    assert_eq!(
        receive(CustomPort.try_new_local_tab_with_completion().unwrap()).await,
        NewLocalTabCompletion::NoCreation
    );
}

#[tokio::test]
async fn created_is_exactly_the_launch_identity_and_already_published_once() {
    let (app, sessions) = start().await;
    let events = app.event_receiver();
    let NewLocalTabCompletion::Created(id) =
        receive(app.ui_port().try_new_local_tab_with_completion().unwrap()).await
    else {
        panic!("not created")
    };
    assert_published(&app, &sessions, id);
    assert_eq!(sessions.attempts().len(), 2);
    assert_eq!(sessions.launched().len(), 2);
    assert_eq!(app.view_model().workspace.tabs.len(), 2);
    assert_eq!(
        app.view_model().revision,
        app.initial_view_model().revision + 1
    );
    assert_eq!(
        next_event(&events).await,
        AppEvent::WorkspaceChanged(app.view_model().workspace)
    );
    app.ui_port().try_send(UiCommand::NewLocalTab).unwrap();
    let AppEvent::WorkspaceChanged(workspace) = next_event(&events).await else {
        panic!("missing plain creation")
    };
    assert_eq!(workspace.tabs.len(), 3);
    assert_eq!(sessions.attempts().len(), 3);
    assert_eq!(sessions.launched().len(), 3);
    bounded(app.shutdown()).await.unwrap();
    assert_eq!(sessions.recording.live_session_count(), 0);
}

#[tokio::test]
async fn launch_failure_a_is_no_creation_and_retry_c_has_its_exact_identity() {
    let (app, sessions) = start().await;
    let before = app.view_model();
    let events = app.event_receiver();
    sessions.recording.fail_launch(true);
    assert_eq!(
        receive(app.ui_port().try_new_local_tab_with_completion().unwrap()).await,
        NewLocalTabCompletion::NoCreation
    );
    expect_failure(&events, AppFailureCategory::Pty).await;
    assert_eq!(app.view_model(), before);
    sessions.recording.fail_launch(false);
    let NewLocalTabCompletion::Created(c) =
        receive(app.ui_port().try_new_local_tab_with_completion().unwrap()).await
    else {
        panic!("retry not created")
    };
    assert_published(&app, &sessions, c);
    assert_eq!(sessions.launched().last(), Some(&(c.pane, c.session)));
    assert_eq!(sessions.attempts().len(), 3);
    assert_eq!(app.view_model().workspace.tabs.len(), 2);
    assert_eq!(
        next_event(&events).await,
        AppEvent::WorkspaceChanged(app.view_model().workspace)
    );
    bounded(app.shutdown()).await.unwrap();
}

#[tokio::test]
async fn controlled_partial_failure_and_unrelated_failure_do_not_resolve_b_or_c() {
    let (app, sessions) = start().await;
    let events = app.event_receiver();
    let a_gate = sessions.control_launch();
    let b_gate = sessions.control_launch();
    let c_gate = sessions.control_launch();
    let port = app.ui_port();
    let mut a = port.try_new_local_tab_with_completion().unwrap();
    port.try_send(UiCommand::ClosePane(PaneId::new())).unwrap();
    let mut b = port.try_new_local_tab_with_completion().unwrap();
    let a_pane = bounded(a_gate.started).await.unwrap();
    assert_pending(&mut a);
    assert_pending(&mut b);
    a_gate
        .release
        .send(LaunchOutcome::Fail(SessionFailure::Pty))
        .ok()
        .unwrap();
    assert_eq!(receive(a).await, NewLocalTabCompletion::NoCreation);
    let b_pane = bounded(b_gate.started).await.unwrap();
    let mut c = port.try_new_local_tab_with_completion().unwrap();
    expect_failure(&events, AppFailureCategory::Pty).await;
    expect_failure(&events, AppFailureCategory::Validation).await;
    assert_pending(&mut b);
    assert_pending(&mut c);
    assert_eq!(app.view_model().workspace.tabs.len(), 1);
    b_gate.release.send(LaunchOutcome::Succeed).ok().unwrap();
    let NewLocalTabCompletion::Created(b_id) = receive(b).await else {
        panic!("B not created")
    };
    assert_eq!(b_id.pane, b_pane);
    assert_published(&app, &sessions, b_id);
    let c_pane = bounded(c_gate.started).await.unwrap();
    assert_pending(&mut c);
    c_gate.release.send(LaunchOutcome::Succeed).ok().unwrap();
    let NewLocalTabCompletion::Created(c_id) = receive(c).await else {
        panic!("C not created")
    };
    assert_eq!(c_id.pane, c_pane);
    assert_ne!(b_id, c_id);
    assert_published(&app, &sessions, c_id);
    assert_eq!(&sessions.attempts()[1..], [a_pane, b_pane, c_pane]);
    assert_eq!(
        app.view_model()
            .workspace
            .tabs
            .iter()
            .skip(1)
            .map(|tab| tab.id)
            .collect::<Vec<_>>(),
        [b_id.tab, c_id.tab]
    );
    bounded(app.shutdown()).await.unwrap();
}

#[tokio::test]
async fn dropping_receipt_does_not_cancel_the_accepted_creation_or_event() {
    let (app, sessions) = start().await;
    let gate = sessions.control_launch();
    drop(app.ui_port().try_new_local_tab_with_completion().unwrap());
    let pane = bounded(gate.started).await.unwrap();
    gate.release.send(LaunchOutcome::Succeed).ok().unwrap();
    let AppEvent::WorkspaceChanged(workspace) = next_event(&app.event_receiver()).await else {
        panic!("creation cancelled")
    };
    assert_eq!(workspace.tabs.len(), 2);
    assert_eq!(workspace.tabs[1].active_pane, pane);
    assert_eq!(sessions.attempts().len(), 2);
    bounded(app.shutdown()).await.unwrap();
}

#[tokio::test]
async fn saturated_events_do_not_delay_success_or_failure_completion() {
    for fail in [false, true] {
        let (app, sessions) = start().await;
        let events = app.event_receiver();
        for _ in 0..events.capacity().unwrap() {
            app.ui_port()
                .try_send(UiCommand::SearchConnections(String::new()))
                .unwrap();
        }
        bounded(async {
            while !events.is_full() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let before = app.view_model();
        let gate = sessions.control_launch();
        let receipt = app.ui_port().try_new_local_tab_with_completion().unwrap();
        let pane = bounded(gate.started).await.unwrap();
        gate.release
            .send(if fail {
                LaunchOutcome::Fail(SessionFailure::Pty)
            } else {
                LaunchOutcome::Succeed
            })
            .ok()
            .unwrap();
        let completion = receive(receipt).await;
        assert!(
            events.is_full(),
            "no event was consumed to unblock completion"
        );
        if fail {
            assert_eq!(completion, NewLocalTabCompletion::NoCreation);
            assert_eq!(app.view_model(), before);
        } else {
            let NewLocalTabCompletion::Created(id) = completion else {
                panic!("not created")
            };
            assert_eq!(id.pane, pane);
            assert_published(&app, &sessions, id);
        }
        for _ in 0..events.capacity().unwrap() {
            assert!(matches!(
                next_event(&events).await,
                AppEvent::SearchResults(_)
            ));
        }
        if fail {
            expect_failure(&events, AppFailureCategory::Pty).await;
        } else {
            assert!(matches!(
                next_event(&events).await,
                AppEvent::WorkspaceChanged(_)
            ));
        }
        bounded(app.shutdown()).await.unwrap();
    }
}
