use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::TransportError;

use super::pty_error;

/// 已入队但尚未写入 PTY 的字节上限；队列满时 `write` 异步等待空间。
pub(in super::super) const WRITER_QUEUE_LIMIT: usize = 4 * 1024 * 1024;
/// 大输入按块入队，取消时只丢弃尚未入队的块。
const WRITE_CHUNK_SIZE: usize = 64 * 1024;

/// 块写完或被丢弃时归还其占用的字节额度。
type WriteChunk = (Vec<u8>, OwnedSemaphorePermit);

struct WriterShared {
    budget: Arc<Semaphore>,
    failed: AtomicBool,
    closed: AtomicBool,
}

/// 本地 PTY 输入端：异步入队，实际写入只发生在专用线程上。
pub(in super::super) struct PtyWriter {
    sender: mpsc::Sender<WriteChunk>,
    shared: Arc<WriterShared>,
    #[cfg(unix)]
    _wake: Option<io::PipeWriter>,
}

impl PtyWriter {
    /// 输入端关闭时管道随之关闭，唤醒等待 PTY 可写的线程。
    #[cfg(unix)]
    pub(in super::super) fn wake_on_close(mut self, wake: io::PipeWriter) -> Self {
        self._wake = Some(wake);
        self
    }

    /// 额度申请可被取消：已入队的块保持顺序写出，未入队的部分不写。
    pub(in super::super) async fn write(&self, bytes: &[u8]) -> Result<(), TransportError> {
        if self.shared.failed.load(Ordering::Acquire) {
            return Err(pty_error());
        }
        for chunk in bytes.chunks(WRITE_CHUNK_SIZE) {
            let permits = u32::try_from(chunk.len()).map_err(|_| pty_error())?;
            let permit = Arc::clone(&self.shared.budget)
                .acquire_many_owned(permits)
                .await
                .map_err(|_| pty_error())?;
            if self.shared.failed.load(Ordering::Acquire) {
                return Err(pty_error());
            }
            self.sender
                .send((chunk.to_vec(), permit))
                .map_err(|_| pty_error())?;
        }
        Ok(())
    }
}

impl Drop for PtyWriter {
    fn drop(&mut self) {
        // 丢弃队列；Unix 随后关闭唤醒管道，Windows 由进程树终止或 PTY 关闭打断写入。
        self.shared.closed.store(true, Ordering::Release);
    }
}

pub(in super::super) fn spawn_writer(
    writer: Box<dyn Write + Send>,
) -> io::Result<(PtyWriter, JoinHandle<()>)> {
    let (sender, receiver) = mpsc::channel();
    let shared = Arc::new(WriterShared {
        budget: Arc::new(Semaphore::new(WRITER_QUEUE_LIMIT)),
        failed: AtomicBool::new(false),
        closed: AtomicBool::new(false),
    });
    let thread_shared = Arc::clone(&shared);
    let thread = thread::Builder::new()
        .name("rshell-local-pty-writer".to_owned())
        .spawn(move || write_loop(writer, &receiver, &thread_shared))?;
    Ok((
        PtyWriter {
            sender,
            shared,
            #[cfg(unix)]
            _wake: None,
        },
        thread,
    ))
}

fn write_loop(
    mut writer: Box<dyn Write + Send>,
    receiver: &mpsc::Receiver<WriteChunk>,
    shared: &WriterShared,
) {
    while let Ok((bytes, permit)) = receiver.recv() {
        if shared.closed.load(Ordering::Acquire) {
            continue;
        }
        let result = writer.write_all(&bytes).and_then(|()| writer.flush());
        if result.is_err() {
            // 先关闭额度申请，再退款，避免等待者因退款被唤醒后误报入队成功。
            shared.failed.store(true, Ordering::Release);
            shared.budget.close();
            drop(permit);
            return;
        }
        drop(permit);
    }
}
