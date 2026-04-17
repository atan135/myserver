use serde::Serialize;
use tracing::info;

use crate::core::logic::{RoomLogic, RoomLogicBroadcast};
use crate::core::room::PlayerInputRecord;
use crate::core::system::combat::{
    CombatCommandResult, CombatEntityBlueprint, CombatEvent, CombatEventKind, CombatSnapshot,
    NoopCombatHooks, Position, RoomCombatEcs, SharedCombatCatalog, parse_player_input,
};
use crate::pb::GameMessagePush;
use crate::protocol::{MessageType, encode_body};

const SNAPSHOT_INTERVAL_FRAMES: u32 = 5;
const DUMMY_TEAM_ID: u16 = 90;

pub struct CombatDemoLogic {
    room_id: String,
    tick_count: u64,
    roster: Vec<String>,
    combat: RoomCombatEcs,
    catalog: SharedCombatCatalog,
    pending_broadcasts: Vec<RoomLogicBroadcast>,
}

impl CombatDemoLogic {
    pub fn new(catalog: SharedCombatCatalog) -> Self {
        Self {
            room_id: String::new(),
            tick_count: 0,
            roster: Vec::new(),
            combat: RoomCombatEcs::new(),
            catalog,
            pending_broadcasts: Vec::new(),
        }
    }

    fn ensure_player_in_roster(&mut self, player_id: &str) {
        if !self.roster.iter().any(|existing| existing == player_id) {
            self.roster.push(player_id.to_string());
        }
    }

    fn rebuild_combat_state(&mut self) {
        self.combat.clear();
        let roster = self.roster.clone();
        for (index, player_id) in roster.iter().enumerate() {
            self.spawn_player_with_index(index, player_id);
        }
        self.spawn_training_dummies();
    }

    fn spawn_player_with_index(&mut self, index: usize, player_id: &str) {
        if self.combat.entity_id_by_player(player_id).is_some() {
            return;
        }

        let team_id = if index % 2 == 0 { 1 } else { 2 };
        let row = (index / 2) as f32;
        let x = if team_id == 1 { 20.0 } else { 220.0 };
        let y = row * 32.0;
        let facing = if team_id == 1 {
            Position { x: 1.0, y: 0.0 }
        } else {
            Position { x: -1.0, y: 0.0 }
        };

        let _ = self.combat.spawn_entity(
            CombatEntityBlueprint::player(player_id, team_id, Position { x, y })
                .with_facing(facing)
                .with_skills(&[1, 2, 3, 4, 5]),
        );
    }

    fn spawn_training_dummies(&mut self) {
        let dummy_positions = [
            Position { x: 120.0, y: -16.0 },
            Position { x: 150.0, y: 18.0 },
        ];

        for position in dummy_positions {
            let _ = self.combat.spawn_entity(
                CombatEntityBlueprint::monster(DUMMY_TEAM_ID, position).with_skills(&[1, 5]),
            );
        }
    }

    fn queue_game_push<T: Serialize>(
        &mut self,
        event: &str,
        action: &str,
        player_id: &str,
        payload: &T,
    ) {
        let payload_json = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
        let message = GameMessagePush {
            event: event.to_string(),
            room_id: self.room_id.clone(),
            player_id: player_id.to_string(),
            action: action.to_string(),
            payload_json,
        };
        self.pending_broadcasts.push(RoomLogicBroadcast {
            message_type: MessageType::GameMessagePush,
            body: encode_body(&message),
        });
    }

    fn queue_snapshot_push(&mut self, frame_id: u32, reason: &str, full_sync: bool) {
        #[derive(Serialize)]
        struct SnapshotEnvelope {
            frame_id: u32,
            reason: String,
            full_sync: bool,
            snapshot: CombatSnapshot,
        }

        let snapshot = self.combat.snapshot(frame_id, self.catalog.as_ref());
        self.queue_game_push(
            "combat",
            "snapshot",
            "",
            &SnapshotEnvelope {
                frame_id,
                reason: reason.to_string(),
                full_sync,
                snapshot,
            },
        );
    }

    fn queue_event_push(&mut self, event: &CombatEvent) {
        let player_id = event
            .source_entity
            .and_then(|entity_id| self.combat.entity_player_id(entity_id))
            .unwrap_or_default()
            .to_string();
        self.queue_game_push("combat", event.kind.as_str(), &player_id, event);
    }

    fn queue_input_reject_push(&mut self, frame_id: u32, player_id: &str, error_code: &str) {
        #[derive(Serialize)]
        struct RejectEnvelope<'a> {
            frame_id: u32,
            player_id: &'a str,
            error_code: &'a str,
        }

        self.queue_game_push(
            "combat",
            "input_reject",
            player_id,
            &RejectEnvelope {
                frame_id,
                player_id,
                error_code,
            },
        );
    }
}

impl RoomLogic for CombatDemoLogic {
    fn on_room_created(&mut self, room_id: &str) {
        self.room_id = room_id.to_string();
        info!(room_id, "[RoomLogic/combat_demo] room created");
    }

    fn on_player_join(&mut self, player_id: &str) {
        self.ensure_player_in_roster(player_id);
        let index = self
            .roster
            .iter()
            .position(|existing| existing == player_id)
            .unwrap_or_default();
        self.spawn_player_with_index(index, player_id);
    }

    fn on_player_leave(&mut self, player_id: &str) {
        if let Some(entity_id) = self.combat.entity_id_by_player(player_id) {
            self.combat.remove_entity(entity_id);
        }
        self.roster.retain(|existing| existing != player_id);
    }

    fn on_game_started(&mut self, _room_id: &str) {
        self.rebuild_combat_state();
        self.queue_snapshot_push(0, "game_started", true);
    }

    fn on_game_ended(&mut self, _room_id: &str) {
        self.queue_snapshot_push(self.combat.last_tick_frame(), "game_ended", true);
        self.combat.clear();
    }

    fn on_tick(&mut self, frame_id: u32, fps: u16, inputs: &[PlayerInputRecord]) {
        self.tick_count = self.tick_count.saturating_add(1);

        let mut hooks = NoopCombatHooks;
        for input in inputs {
            match parse_player_input(input, &self.combat) {
                Ok(Some(command)) => match self
                    .combat
                    .execute_command(command, self.catalog.as_ref(), &mut hooks)
                {
                    CombatCommandResult::Accepted | CombatCommandResult::Ignored => {}
                    CombatCommandResult::Rejected { reason } => {
                        self.queue_input_reject_push(frame_id, &input.player_id, &reason);
                    }
                },
                Ok(None) => {}
                Err(error) => {
                    self.queue_input_reject_push(frame_id, &input.player_id, error.error_code);
                }
            }
        }

        self.combat
            .tick(frame_id, fps, self.catalog.as_ref(), &mut hooks);
        let events = self.combat.drain_events();
        let force_snapshot = events.iter().any(|event| {
            matches!(
                event.kind,
                CombatEventKind::Spawned
                    | CombatEventKind::Removed
                    | CombatEventKind::BuffApplied
                    | CombatEventKind::BuffExpired
                    | CombatEventKind::Defeated
            )
        });

        for event in &events {
            self.queue_event_push(event);
        }

        if frame_id % SNAPSHOT_INTERVAL_FRAMES == 0 || force_snapshot {
            self.queue_snapshot_push(frame_id, "tick_sync", frame_id % SNAPSHOT_INTERVAL_FRAMES == 0);
        }
    }

    fn get_serialized_state(&self) -> String {
        #[derive(Serialize)]
        struct CombatRoomState<'a> {
            room_id: &'a str,
            tick_count: u64,
            roster: &'a [String],
            snapshot: CombatSnapshot,
        }

        serde_json::to_string(&CombatRoomState {
            room_id: &self.room_id,
            tick_count: self.tick_count,
            roster: &self.roster,
            snapshot: self
                .combat
                .snapshot(self.combat.last_tick_frame(), self.catalog.as_ref()),
        })
        .unwrap_or_default()
    }

    fn take_pending_broadcasts(&mut self) -> Vec<RoomLogicBroadcast> {
        std::mem::take(&mut self.pending_broadcasts)
    }
}
