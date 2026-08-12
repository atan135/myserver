use crate::config::{LoadModel, LoadStage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledAction {
    pub planned_at_ms: u64,
    pub actual_at_ms: u64,
    pub scheduler_lag_ms: u64,
    /// `staged` actions belong to a bounded wave and must be admitted and
    /// completed before this monotonic offset. Other load models have no
    /// per-action window beyond the run deadline.
    pub window_end_ms: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SchedulerTick {
    pub actions: Vec<ScheduledAction>,
    pub dropped: u64,
    pub queue_depth: u64,
}

#[derive(Debug, Clone)]
enum Source {
    Fixed {
        remaining: u32,
    },
    Arrival {
        next_at_ms: f64,
        interval_ms: f64,
        end_ms: u64,
    },
    Staged {
        stages: Vec<LoadStage>,
        stage_index: usize,
        stage_start_ms: u64,
        remaining: u32,
    },
    Burst {
        burst_start_ms: u64,
        remaining: u32,
        burst_size: u32,
        every_ms: u64,
        end_ms: u64,
    },
}

#[derive(Debug, Clone)]
pub struct MonotonicScheduler {
    source: Source,
    max_lag_ms: u64,
    max_dispatch_per_tick: usize,
    dropped: u64,
}

impl MonotonicScheduler {
    pub fn new(model: &LoadModel, max_lag_ms: u64, max_dispatch_per_tick: usize) -> Self {
        let source = match model {
            LoadModel::FixedConcurrency {
                virtual_players, ..
            } => Source::Fixed {
                remaining: *virtual_players,
            },
            LoadModel::ArrivalRate {
                arrivals_per_second,
                duration_secs,
            } => Source::Arrival {
                next_at_ms: 0.0,
                interval_ms: 1000.0 / arrivals_per_second,
                end_ms: duration_secs.saturating_mul(1000),
            },
            LoadModel::Staged { stages } => Source::Staged {
                stages: stages.clone(),
                stage_index: 0,
                stage_start_ms: 0,
                remaining: stages.first().map_or(0, |stage| stage.virtual_players),
            },
            LoadModel::Burst {
                burst_size,
                every_secs,
                duration_secs,
            } => Source::Burst {
                burst_start_ms: 0,
                remaining: *burst_size,
                burst_size: *burst_size,
                every_ms: every_secs.saturating_mul(1000),
                end_ms: duration_secs.saturating_mul(1000),
            },
        };
        Self {
            source,
            max_lag_ms,
            max_dispatch_per_tick: max_dispatch_per_tick.max(1),
            dropped: 0,
        }
    }

    pub fn due(&mut self, now_ms: u64) -> SchedulerTick {
        let mut tick = SchedulerTick::default();
        loop {
            let Some(planned_at_ms) = self.next_planned() else {
                break;
            };
            if planned_at_ms > now_ms {
                break;
            }
            let window_end_ms = self.current_window_end_ms();
            if tick.actions.len() >= self.max_dispatch_per_tick {
                // Staged waves must reach the live controller so its stage
                // deadline can fail closed; silently dropping them would make
                // a partial stage look complete.
                if window_end_ms.is_some() {
                    break;
                }
                self.consume();
                self.dropped += 1;
                tick.dropped += 1;
                continue;
            }
            if now_ms.saturating_sub(planned_at_ms) > self.max_lag_ms && window_end_ms.is_none() {
                self.consume();
                self.dropped += 1;
                tick.dropped += 1;
                continue;
            }
            self.consume();
            tick.actions.push(ScheduledAction {
                planned_at_ms,
                actual_at_ms: now_ms,
                scheduler_lag_ms: now_ms.saturating_sub(planned_at_ms),
                window_end_ms,
            });
        }
        tick.queue_depth = self.queue_depth(now_ms);
        tick
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn exhausted(&mut self) -> bool {
        self.next_planned().is_none()
    }

    fn next_planned(&mut self) -> Option<u64> {
        match &mut self.source {
            Source::Fixed { remaining } => (*remaining > 0).then_some(0),
            Source::Arrival {
                next_at_ms, end_ms, ..
            } => ((*next_at_ms as u64) < *end_ms).then_some(*next_at_ms as u64),
            Source::Staged {
                stages,
                stage_index,
                stage_start_ms,
                remaining,
            } => {
                while *remaining == 0 && *stage_index + 1 < stages.len() {
                    *stage_start_ms = stage_start_ms
                        .saturating_add(stages[*stage_index].duration_secs.saturating_mul(1000));
                    *stage_index += 1;
                    *remaining = stages[*stage_index].virtual_players;
                }
                (*remaining > 0).then_some(*stage_start_ms)
            }
            Source::Burst {
                burst_start_ms,
                remaining,
                burst_size,
                every_ms,
                end_ms,
            } => {
                while *remaining == 0 {
                    *burst_start_ms = burst_start_ms.saturating_add(*every_ms);
                    if *burst_start_ms >= *end_ms {
                        return None;
                    }
                    *remaining = *burst_size;
                }
                (*burst_start_ms < *end_ms).then_some(*burst_start_ms)
            }
        }
    }

    fn consume(&mut self) {
        match &mut self.source {
            Source::Fixed { remaining }
            | Source::Staged { remaining, .. }
            | Source::Burst { remaining, .. } => *remaining = remaining.saturating_sub(1),
            Source::Arrival {
                next_at_ms,
                interval_ms,
                ..
            } => *next_at_ms += *interval_ms,
        }
    }

    fn current_window_end_ms(&self) -> Option<u64> {
        match &self.source {
            Source::Staged {
                stages,
                stage_index,
                stage_start_ms,
                ..
            } => stages.get(*stage_index).map(|stage| {
                stage_start_ms.saturating_add(stage.duration_secs.saturating_mul(1_000))
            }),
            _ => None,
        }
    }

    fn queue_depth(&mut self, now_ms: u64) -> u64 {
        let Some(next) = self.next_planned() else {
            return 0;
        };
        u64::from(next <= now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arrival_rate_records_lag_and_drops_instead_of_replaying_debt() {
        let model = LoadModel::ArrivalRate {
            arrivals_per_second: 10.0,
            duration_secs: 2,
        };
        let mut scheduler = MonotonicScheduler::new(&model, 50, 2);
        let tick = scheduler.due(500);
        assert_eq!(tick.actions.len(), 1);
        assert_eq!(tick.actions[0].planned_at_ms, 500);
        assert_eq!(tick.dropped, 5);
        assert_eq!(scheduler.dropped(), 5);
    }
    #[test]
    fn fixed_staged_and_burst_models_emit_planned_actions() {
        for model in [
            LoadModel::FixedConcurrency {
                virtual_players: 2,
                duration_secs: 1,
            },
            LoadModel::Staged {
                stages: vec![LoadStage {
                    name: "warm".into(),
                    virtual_players: 2,
                    duration_secs: 1,
                }],
            },
            LoadModel::Burst {
                burst_size: 2,
                every_secs: 1,
                duration_secs: 2,
            },
        ] {
            assert_eq!(
                MonotonicScheduler::new(&model, 10, 10).due(0).actions.len(),
                2
            );
        }
    }

    #[test]
    fn staged_waves_launch_at_ordered_boundaries_with_explicit_windows() {
        let model = LoadModel::Staged {
            stages: vec![
                LoadStage {
                    name: "warm".into(),
                    virtual_players: 2,
                    duration_secs: 1,
                },
                LoadStage {
                    name: "step".into(),
                    virtual_players: 1,
                    duration_secs: 2,
                },
            ],
        };
        let mut scheduler = MonotonicScheduler::new(&model, 10, 2);
        let warm = scheduler.due(0);
        assert_eq!(warm.actions.len(), 2);
        assert!(
            warm.actions
                .iter()
                .all(|action| action.planned_at_ms == 0 && action.window_end_ms == Some(1_000))
        );
        assert!(scheduler.due(999).actions.is_empty());

        let step = scheduler.due(1_000);
        assert_eq!(step.actions.len(), 1);
        assert_eq!(step.actions[0].planned_at_ms, 1_000);
        assert_eq!(step.actions[0].window_end_ms, Some(3_000));
    }

    #[test]
    fn overdue_staged_actions_are_preserved_for_fail_closed_window_handling() {
        let model = LoadModel::Staged {
            stages: vec![LoadStage {
                name: "warm".into(),
                virtual_players: 1,
                duration_secs: 1,
            }],
        };
        let mut scheduler = MonotonicScheduler::new(&model, 10, 1);
        let tick = scheduler.due(1_001);
        assert_eq!(tick.dropped, 0);
        assert_eq!(tick.actions.len(), 1);
        assert_eq!(tick.actions[0].window_end_ms, Some(1_000));
    }
}
