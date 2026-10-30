use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::watch;

use crate::{
    AppEvent, PaneId, PaneLaunchTarget, PaneTree, SessionState, TabState, UiCommand, WorkspaceState,
};

use super::{
    AppBootstrapState, AppDependencies, AppError, AppEventStream, AppViewModel, LatestViewStream,
    NewLocalTabReceipt, NewLocalTabSubmitError, SessionBinding, UI_COMMAND_CAPACITY, UiCommandPort,
    UiPortError,
    completion_queue::{CommandEnvelope, CommandQueue, receipt_channel},
    runtime::{CommandLoop, LoopControl},
};

const EVENT_CAPACITY: usize = 256;
const INTERNAL_CAPACITY: usize = 256;

pub struct ApplicationService;

impl ApplicationService {
    pub async fn start(
        dependencies: AppDependencies,
        bootstrap: AppBootstrapState,
    ) -> Result<ApplicationHandle, AppError> {
        let mut view_model = AppViewModel::from(bootstrap);
        let terminal = CommandLoop::resolve_terminal_from(&view_model, None)
            .ok_or(AppError::InvalidBootstrap)?;
        let pane = PaneId::new();
        let binding = dependencies
            .sessions
            .launch_local(pane, terminal)
            .await
            .map_err(AppError::InitialSession)?;
        open_initial_tab(&mut view_model.workspace, pane, binding.id);
        view_model
            .pane_launches
            .insert(pane, PaneLaunchTarget::Local);
        view_model
            .session_states
            .insert(binding.id, SessionState::Created);

        Ok(Self::spawn_loop(dependencies, view_model, binding))
    }

    fn spawn_loop(
        dependencies: AppDependencies,
        view_model: AppViewModel,
        binding: SessionBinding,
    ) -> ApplicationHandle {
        let (command_tx, command_rx) = async_channel::bounded(UI_COMMAND_CAPACITY);
        let (event_tx, event_rx) = async_channel::bounded(EVENT_CAPACITY);
        let (internal_tx, internal_rx) = async_channel::bounded(INTERNAL_CAPACITY);
        let (view_tx, view_rx) = watch::channel(view_model.clone());
        let (done_tx, done_rx) = watch::channel::<Option<Result<(), AppError>>>(None);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let accepting = Arc::new(AtomicBool::new(true));
        let closed = Arc::new(AtomicBool::new(false));
        let command_queue = CommandQueue {
            receiver: command_rx,
            accepting: Arc::clone(&accepting),
            closed: Arc::clone(&closed),
        };
        let commands = Arc::new(CommandSender {
            sender: command_tx,
            accepting: Arc::clone(&accepting),
            closed: Arc::clone(&closed),
            shutdown: shutdown_tx,
        });
        let initial = view_model.clone();
        let mut command_loop = CommandLoop::new(
            dependencies,
            view_model,
            event_tx,
            internal_tx,
            view_tx,
            LoopControl {
                closed,
                shutdown: shutdown_rx,
            },
        );
        command_loop.bind(binding);
        tokio::spawn(command_loop.run(command_queue, internal_rx, done_tx));

        ApplicationHandle {
            commands,
            events: event_rx,
            initial,
            view: view_rx,
            done: done_rx,
        }
    }
}

pub struct ApplicationHandle {
    commands: Arc<CommandSender>,
    events: async_channel::Receiver<AppEvent>,
    initial: AppViewModel,
    view: watch::Receiver<AppViewModel>,
    done: watch::Receiver<Option<Result<(), AppError>>>,
}

impl ApplicationHandle {
    pub fn ui_port(&self) -> Arc<dyn UiCommandPort> {
        self.commands.clone()
    }

    pub fn event_receiver(&self) -> async_channel::Receiver<AppEvent> {
        self.events.clone()
    }

    pub fn event_stream(&self) -> AppEventStream {
        AppEventStream::new(self.events.clone())
    }

    pub fn initial_view_model(&self) -> &AppViewModel {
        &self.initial
    }

    pub fn view_model(&self) -> AppViewModel {
        self.view.borrow().clone()
    }

    pub fn view_stream(&self) -> LatestViewStream {
        LatestViewStream::new(self.view.clone())
    }

    pub async fn shutdown(&self) -> Result<(), AppError> {
        if self.done.borrow().is_none() {
            self.commands.initiate_shutdown().await?;
        }
        let mut done = self.done.clone();
        loop {
            if let Some(result) = *done.borrow() {
                return result;
            }
            done.changed().await.map_err(|_| AppError::Closed)?;
        }
    }
}

struct CommandSender {
    sender: async_channel::Sender<CommandEnvelope>,
    accepting: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    shutdown: watch::Sender<bool>,
}

impl CommandSender {
    async fn initiate_shutdown(&self) -> Result<(), AppError> {
        self.signal_shutdown();
        Ok(())
    }

    fn signal_shutdown(&self) {
        self.accepting.store(false, Ordering::Release);
        self.shutdown.send_replace(true);
    }

    fn enqueue(&self, envelope: CommandEnvelope) -> Result<(), UiPortError> {
        if self.closed.load(Ordering::Acquire) || !self.accepting.load(Ordering::Acquire) {
            return Err(UiPortError::Closed);
        }
        self.sender.try_send(envelope).map_err(|error| match error {
            async_channel::TrySendError::Full(_) => UiPortError::Busy,
            async_channel::TrySendError::Closed(_) => UiPortError::Closed,
        })
    }
}

impl UiCommandPort for CommandSender {
    fn try_send(&self, command: UiCommand) -> Result<(), UiPortError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(UiPortError::Closed);
        }
        if matches!(command, UiCommand::Shutdown) {
            self.signal_shutdown();
            return Ok(());
        }
        self.enqueue(CommandEnvelope {
            command,
            new_local_completion: None,
        })
    }

    fn try_new_local_tab_with_completion(
        &self,
    ) -> Result<NewLocalTabReceipt, NewLocalTabSubmitError> {
        let (sender, receipt) = receipt_channel();
        self.enqueue(CommandEnvelope {
            command: UiCommand::NewLocalTab,
            new_local_completion: Some(sender),
        })
        .map_err(NewLocalTabSubmitError::Rejected)?;
        Ok(receipt)
    }
}

fn open_initial_tab(workspace: &mut WorkspaceState, pane: PaneId, session: crate::SessionId) {
    let id = uuid::Uuid::new_v4();
    workspace.tabs.push(TabState {
        id,
        title: "Local".into(),
        pane_tree: PaneTree::with_session(pane, session),
        active_pane: pane,
    });
    workspace.active_tab = Some(id);
}

#[cfg(test)]
#[path = "../../tests/new_local_completion_private.rs"]
mod completion_tests;

#[cfg(test)]
#[tokio::test]
async fn missing_terminal_completes_no_creation_on_the_real_loop() {
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        completion_tests::missing_terminal_validation(ApplicationService::spawn_loop),
    )
    .await
    .expect("private validation fixture timed out");
}
