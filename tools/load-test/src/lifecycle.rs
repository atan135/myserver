use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Created,
    Validated,
    WarmingUp,
    Ramping,
    Steady,
    CoolingDown,
    Completed,
    Aborting,
    Aborted,
    Failed,
}

#[derive(Debug, Clone)]
pub struct Lifecycle {
    state: RunState,
    history: Vec<RunState>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            state: RunState::Created,
            history: vec![RunState::Created],
        }
    }
}

impl Lifecycle {
    pub fn state(&self) -> RunState {
        self.state
    }
    pub fn history(&self) -> &[RunState] {
        &self.history
    }
    pub fn transition(&mut self, next: RunState) -> Result<(), &'static str> {
        if !allowed(self.state, next) {
            return Err("invalid run lifecycle transition");
        }
        self.state = next;
        self.history.push(next);
        Ok(())
    }
    pub fn abort(&mut self) {
        if !matches!(
            self.state,
            RunState::Completed | RunState::Aborted | RunState::Failed
        ) {
            let _ = self.transition(RunState::Aborting);
            let _ = self.transition(RunState::Aborted);
        }
    }
    pub fn fail(&mut self) {
        if !matches!(
            self.state,
            RunState::Completed | RunState::Aborted | RunState::Failed
        ) {
            let _ = self.transition(RunState::Failed);
        }
    }
}

fn allowed(from: RunState, to: RunState) -> bool {
    use RunState::*;
    matches!(
        (from, to),
        (Created, Validated)
            | (Validated, WarmingUp)
            | (WarmingUp, Ramping)
            | (Ramping, Steady)
            | (Steady, CoolingDown)
            | (CoolingDown, Completed)
            | (
                Created | Validated | WarmingUp | Ramping | Steady | CoolingDown,
                Aborting
            )
            | (Aborting, Aborted)
            | (
                Created | Validated | WarmingUp | Ramping | Steady | CoolingDown | Aborting,
                Failed
            )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normal_and_abort_paths_are_explicit() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.transition(RunState::Validated).unwrap();
        assert!(lifecycle.transition(RunState::Steady).is_err());
        lifecycle.abort();
        assert_eq!(
            lifecycle.history(),
            &[
                RunState::Created,
                RunState::Validated,
                RunState::Aborting,
                RunState::Aborted
            ]
        );
    }
}
