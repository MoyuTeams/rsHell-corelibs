use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::oneshot;

use crate::UiCommand;

use super::{NewLocalTabCompletion, NewLocalTabReceipt, NewLocalTabReceiptClosed};

pub(super) type CompletionSender = oneshot::Sender<NewLocalTabCompletion>;

pub(super) struct CommandEnvelope {
    pub(super) command: UiCommand,
    pub(super) new_local_completion: Option<CompletionSender>,
}

pub(super) fn receipt_channel() -> (CompletionSender, NewLocalTabReceipt) {
    let (sender, receiver) = oneshot::channel();
    let receipt = Box::pin(async move { receiver.await.map_err(|_| NewLocalTabReceiptClosed) });
    (sender, receipt)
}

pub(super) fn complete(sender: Option<CompletionSender>, completion: NewLocalTabCompletion) {
    if let Some(sender) = sender {
        let _ = sender.send(completion);
    }
}

/// The sole receiver owner must exist before spawning the command-loop future.
pub(super) struct CommandQueue {
    pub(super) receiver: async_channel::Receiver<CommandEnvelope>,
    pub(super) accepting: Arc<AtomicBool>,
    pub(super) closed: Arc<AtomicBool>,
}

impl CommandQueue {
    pub(super) fn close(&self) {
        self.accepting.store(false, Ordering::Release);
        self.receiver.close();
        // Closing alone retains buffered senders while an external UI port is held.
        while self.receiver.try_recv().is_ok() {}
    }
}

impl Drop for CommandQueue {
    fn drop(&mut self) {
        self.close();
        self.closed.store(true, Ordering::Release);
    }
}
