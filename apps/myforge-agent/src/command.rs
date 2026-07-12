use async_trait::async_trait;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;

use crate::schemas::{CommandExecute, CommandRejection, CommandResultSemantic};

#[derive(Clone, Debug)]
pub struct CommandCancellation {
    token: CancellationToken,
    deadline_at_ms: Arc<AtomicU64>,
}

impl CommandCancellation {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
            deadline_at_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    #[cfg(test)]
    fn from_token(token: CancellationToken) -> Self {
        Self {
            token,
            deadline_at_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    pub fn deadline_at_ms(&self) -> Option<u64> {
        match self.deadline_at_ms.load(Ordering::Acquire) {
            0 => None,
            deadline => Some(deadline),
        }
    }

    pub fn cancel_at(&self, deadline_at_ms: u64) {
        debug_assert_ne!(deadline_at_ms, 0);
        self.deadline_at_ms.store(deadline_at_ms, Ordering::Release);
        self.token.cancel();
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }
}

impl Default for CommandCancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub struct CommandControl {
    cancellation: CommandCancellation,
    received_at_ms: u64,
}

impl CommandControl {
    #[cfg(test)]
    pub(crate) fn new(cancellation: CancellationToken, received_at_ms: u64) -> Self {
        Self {
            cancellation: CommandCancellation::from_token(cancellation),
            received_at_ms,
        }
    }

    pub(crate) const fn from_cancellation(
        cancellation: CommandCancellation,
        received_at_ms: u64,
    ) -> Self {
        Self {
            cancellation,
            received_at_ms,
        }
    }

    pub fn cancellation(&self) -> &CancellationToken {
        self.cancellation.token()
    }

    pub fn cancel_deadline_at_ms(&self) -> Option<u64> {
        self.cancellation.deadline_at_ms()
    }

    pub const fn received_at_ms(&self) -> u64 {
        self.received_at_ms
    }
}

pub struct StartedExecution {
    started_at_ms: u64,
    completion: Pin<Box<dyn Future<Output = StartedExecutionOutcome> + Send>>,
}

pub enum StartedExecutionOutcome {
    Result(Box<CommandResultSemantic>),
    FailClosed { reason: &'static str },
}

impl From<CommandResultSemantic> for StartedExecutionOutcome {
    fn from(result: CommandResultSemantic) -> Self {
        Self::Result(Box::new(result))
    }
}

impl StartedExecution {
    pub fn new(
        started_at_ms: u64,
        completion: impl Future<Output = CommandResultSemantic> + Send + 'static,
    ) -> Self {
        Self {
            started_at_ms,
            completion: Box::pin(async move {
                StartedExecutionOutcome::Result(Box::new(completion.await))
            }),
        }
    }

    pub fn new_outcome(
        started_at_ms: u64,
        completion: impl Future<Output = StartedExecutionOutcome> + Send + 'static,
    ) -> Self {
        Self {
            started_at_ms,
            completion: Box::pin(completion),
        }
    }

    pub const fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    pub async fn finish(self) -> StartedExecutionOutcome {
        self.completion.await
    }
}

pub enum CommandHandlerOutcome {
    PreStartError(CommandRejection),
    CancelledBeforeStart,
    CompletedBeforeStart(Box<CommandResultSemantic>),
    Started(StartedExecution),
}

#[async_trait]
pub trait CommandHandler: Send + Sync {
    async fn execute(
        &self,
        command: CommandExecute,
        control: CommandControl,
    ) -> CommandHandlerOutcome;
}

#[derive(Debug, Default)]
pub struct PendingCommandHandler;

#[async_trait]
impl CommandHandler for PendingCommandHandler {
    async fn execute(
        &self,
        _command: CommandExecute,
        _control: CommandControl,
    ) -> CommandHandlerOutcome {
        CommandHandlerOutcome::PreStartError(CommandRejection::new(
            "MYFORGE_CODEX_UNAVAILABLE",
            "local command execution is not configured",
            false,
        ))
    }
}
