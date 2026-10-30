#[path = "support/mod.rs"]
pub mod recording;

use std::{
    collections::VecDeque,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use rshell_core::{
    AppDependencies, AppEvent, AppFailureCategory, ApplicationHandle, ApplicationService,
    ConnectionProfile, NewLocalTabCompletion, NewLocalTabIdentity, NewLocalTabReceipt, PaneId,
    ResolvedTerminalProfile, SessionBinding, SessionFailure, SessionId, SessionPort,
    SessionUiCommand, TerminalSize,
};
use secrecy::SecretString;
use tokio::sync::oneshot;

use recording::{RecordingPorts, bootstrap_state};

pub async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(2), future)
        .await
        .expect("completion test timed out")
}

pub async fn receive(receipt: NewLocalTabReceipt) -> NewLocalTabCompletion {
    bounded(receipt)
        .await
        .expect("producer unexpectedly closed")
}

pub fn assert_pending(receipt: &mut NewLocalTabReceipt) {
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(receipt.as_mut().poll(&mut context).is_pending());
}

pub async fn next_event(events: &async_channel::Receiver<AppEvent>) -> AppEvent {
    bounded(events.recv()).await.unwrap()
}

pub async fn expect_failure(
    events: &async_channel::Receiver<AppEvent>,
    category: AppFailureCategory,
) {
    let AppEvent::OperationFailed(failure) = next_event(events).await else {
        panic!("expected operation failure");
    };
    assert_eq!(failure.category, category);
}

pub fn assert_published(
    app: &ApplicationHandle,
    sessions: &ControlledSessions,
    id: NewLocalTabIdentity,
) {
    let view = app.view_model();
    let tab = view
        .workspace
        .tabs
        .iter()
        .find(|tab| tab.id == id.tab)
        .expect("receipt tab already published");
    assert_eq!(tab.active_pane, id.pane);
    assert_eq!(tab.pane_tree.session_id(id.pane).unwrap(), Some(id.session));
    assert_eq!(
        view.pane_launches[&id.pane],
        rshell_core::PaneLaunchTarget::Local
    );
    assert!(view.session_states.contains_key(&id.session));
    assert!(sessions.launched().contains(&(id.pane, id.session)));
}

pub async fn start() -> (ApplicationHandle, Arc<ControlledSessions>) {
    let bootstrap = bootstrap_state();
    let sessions = Arc::new(ControlledSessions::new(RecordingPorts::new(&bootstrap)));
    let app = ApplicationService::start(sessions.dependencies(), bootstrap)
        .await
        .unwrap();
    (app, sessions)
}

pub enum LaunchOutcome {
    Succeed,
    Fail(SessionFailure),
    Panic,
}

pub struct LaunchControl {
    pub started: oneshot::Receiver<PaneId>,
    pub release: oneshot::Sender<LaunchOutcome>,
}

struct LaunchGate {
    started: oneshot::Sender<PaneId>,
    release: oneshot::Receiver<LaunchOutcome>,
}

pub struct ShutdownControl {
    pub started: oneshot::Receiver<()>,
    pub release: oneshot::Sender<()>,
}

struct ShutdownGate {
    started: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

pub struct ControlledSessions {
    pub recording: RecordingPorts,
    gates: Mutex<VecDeque<LaunchGate>>,
    shutdown_gate: Mutex<Option<ShutdownGate>>,
    attempts: Mutex<Vec<PaneId>>,
    launched: Mutex<Vec<(PaneId, SessionId)>>,
}

impl ControlledSessions {
    pub fn new(recording: RecordingPorts) -> Self {
        Self {
            recording,
            gates: Mutex::new(VecDeque::new()),
            shutdown_gate: Mutex::new(None),
            attempts: Mutex::new(Vec::new()),
            launched: Mutex::new(Vec::new()),
        }
    }

    pub fn dependencies(self: &Arc<Self>) -> AppDependencies {
        let mut dependencies = self.recording.dependencies();
        dependencies.sessions = self.clone();
        dependencies
    }

    pub fn control_launch(&self) -> LaunchControl {
        let (started_tx, started) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        self.gates.lock().unwrap().push_back(LaunchGate {
            started: started_tx,
            release: release_rx,
        });
        LaunchControl { started, release }
    }

    pub fn control_shutdown(&self) -> ShutdownControl {
        let (started_tx, started) = oneshot::channel();
        let (release, release_rx) = oneshot::channel();
        *self.shutdown_gate.lock().unwrap() = Some(ShutdownGate {
            started: started_tx,
            release: release_rx,
        });
        ShutdownControl { started, release }
    }

    pub fn attempts(&self) -> Vec<PaneId> {
        self.attempts.lock().unwrap().clone()
    }

    pub fn launched(&self) -> Vec<(PaneId, SessionId)> {
        self.launched.lock().unwrap().clone()
    }
}

#[async_trait]
impl SessionPort for ControlledSessions {
    async fn launch_local(
        &self,
        pane: PaneId,
        terminal: ResolvedTerminalProfile,
    ) -> Result<SessionBinding, SessionFailure> {
        self.attempts.lock().unwrap().push(pane);
        let gate = self.gates.lock().unwrap().pop_front();
        if let Some(gate) = gate {
            gate.started.send(pane).unwrap();
            match gate.release.await.expect("launch control dropped") {
                LaunchOutcome::Succeed => {}
                LaunchOutcome::Fail(failure) => return Err(failure),
                LaunchOutcome::Panic => panic!("intentional completion producer panic"),
            }
        }
        let binding = self.recording.launch_local(pane, terminal).await?;
        self.launched.lock().unwrap().push((pane, binding.id));
        Ok(binding)
    }

    async fn launch_ssh(
        &self,
        pane: PaneId,
        profile: ConnectionProfile,
        terminal: ResolvedTerminalProfile,
        initial_size: TerminalSize,
        secret: Option<SecretString>,
    ) -> Result<SessionBinding, SessionFailure> {
        self.recording
            .launch_ssh(pane, profile, terminal, initial_size, secret)
            .await
    }

    async fn command(
        &self,
        session: SessionId,
        command: SessionUiCommand,
    ) -> Result<(), SessionFailure> {
        self.recording.command(session, command).await
    }

    async fn shutdown(&self, session: SessionId) -> Result<(), SessionFailure> {
        self.recording.shutdown(session).await
    }

    async fn shutdown_all(&self) -> Result<(), SessionFailure> {
        let gate = self.shutdown_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.started.send(()).unwrap();
            gate.release.await.expect("shutdown control dropped");
        }
        self.recording.shutdown_all().await
    }
}
