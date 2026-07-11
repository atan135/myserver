use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::schemas::{CommandExecute, CommandRejection};

#[derive(Clone, Debug)]
pub struct CommandControl {
    cancellation: CancellationToken,
    received_at_ms: u64,
}

impl CommandControl {
    pub(crate) const fn new(cancellation: CancellationToken, received_at_ms: u64) -> Self {
        Self {
            cancellation,
            received_at_ms,
        }
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub const fn received_at_ms(&self) -> u64 {
        self.received_at_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandHandlerOutcome {
    PreStartError(CommandRejection),
    CancelledBeforeStart,
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
