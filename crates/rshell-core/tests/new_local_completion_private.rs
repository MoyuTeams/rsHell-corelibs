#[allow(dead_code)]
#[path = "support/mod.rs"]
pub mod recording;

use rshell_core::{
    AppDependencies, AppEvent, AppFailureCategory, AppViewModel, ApplicationHandle,
    ApplicationService, NewLocalTabCompletion, SessionBinding, SessionPort, SessionState,
    UiCommand,
};

use recording::{RecordingPorts, bootstrap_state};

pub async fn missing_terminal_validation(
    spawn: impl FnOnce(AppDependencies, AppViewModel, SessionBinding) -> ApplicationHandle,
) {
    let bootstrap = bootstrap_state();
    let ports = RecordingPorts::new(&bootstrap);
    let initial = ApplicationService::start(ports.dependencies(), bootstrap.clone())
        .await
        .unwrap();
    let mut view = initial.view_model();
    initial.shutdown().await.unwrap();
    let pane = view.workspace.tabs[0].active_pane;
    let terminal = bootstrap.terminal_profiles[0]
        .settings
        .resolve(&Default::default());
    let binding = ports.launch_local(pane, terminal).await.unwrap();
    view.workspace.tabs[0]
        .pane_tree
        .replace_session(pane, Some(binding.id))
        .unwrap();
    view.session_states.clear();
    view.session_states
        .insert(binding.id, SessionState::Created);
    // No public command can remove the default profile; corrupt only this private fixture.
    view.terminal_profiles.clear();
    let app = spawn(ports.dependencies(), view, binding);
    let before = app.view_model();
    let events = app.event_receiver();
    ports.clear_calls();
    let receipt = app.ui_port().try_new_local_tab_with_completion().unwrap();
    assert_eq!(receipt.await, Ok(NewLocalTabCompletion::NoCreation));
    let AppEvent::OperationFailed(failure) = events.recv().await.unwrap() else {
        panic!("missing validation event")
    };
    assert_eq!(failure.category, AppFailureCategory::Validation);
    assert!(failure.retryable);
    assert_eq!(failure.context, "workspace operation is invalid");
    assert_eq!(app.view_model(), before);
    assert!(ports.calls().is_empty(), "validation must not launch");
    app.ui_port()
        .try_send(UiCommand::SaveTerminalProfile(
            bootstrap.terminal_profiles[0].clone(),
        ))
        .unwrap();
    assert!(matches!(
        events.recv().await.unwrap(),
        AppEvent::TerminalProfilesChanged(_)
    ));
    let receipt = app.ui_port().try_new_local_tab_with_completion().unwrap();
    let Ok(NewLocalTabCompletion::Created(id)) = receipt.await else {
        panic!("fixture retry not created")
    };
    let workspace = app.view_model().workspace;
    assert_eq!(workspace.tabs[1].id, id.tab);
    assert_eq!(
        workspace.tabs[1].pane_tree.session_id(id.pane).unwrap(),
        Some(id.session)
    );
    assert_eq!(
        ports.calls(),
        ["repository.save_terminal_profile", "session.launch_local"]
    );
    app.shutdown().await.unwrap();
    assert_eq!(ports.live_session_count(), 0);
}
