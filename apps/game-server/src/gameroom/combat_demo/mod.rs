use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::info;

use crate::core::config_table::ConfigTableRuntime;
use crate::core::logic::{
    ROOM_TRANSFER_SCHEMA_VERSION, RoomLogic, RoomLogicBroadcast, RoomLogicTransfer,
    RoomLogicTransferState, RoomNpcTransferEntity, RoomNpcTransferPosition,
    RoomNpcTransferSkillState, RoomNpcTransferState, RoomRuntimeTimerTransferState,
    RoomSchedulerTransferEntry, RoomTimerTransferEntry,
};
use crate::core::room::PlayerInputRecord;
use crate::core::system::combat::{
    CombatCommandResult, CombatEntityBlueprint, CombatEntitySnapshot, CombatEvent, CombatEventKind,
    CombatSnapshot, EntityType, NoopCombatHooks, Position, RoomCombatEcs, parse_player_input,
};
use crate::pb::GameMessagePush;
use crate::protocol::{MessageType, encode_body};

const SNAPSHOT_INTERVAL_FRAMES: u32 = 5;
const DUMMY_TEAM_ID: u16 = 90;
const COMBAT_DEMO_TRANSFER_SCHEMA: &str = "combat-demo.logic.v1";

pub struct CombatDemoLogic {
    room_id: String,
    tick_count: u64,
    roster: Vec<String>,
    combat: RoomCombatEcs,
    next_snapshot_frame: u32,
    config_tables: ConfigTableRuntime,
    pending_broadcasts: Vec<RoomLogicBroadcast>,
}

impl CombatDemoLogic {
    pub fn new(config_tables: ConfigTableRuntime) -> Self {
        Self {
            room_id: String::new(),
            tick_count: 0,
            roster: Vec::new(),
            combat: RoomCombatEcs::new(),
            next_snapshot_frame: SNAPSHOT_INTERVAL_FRAMES,
            config_tables,
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
        self.pending_broadcasts
            .push(RoomLogicBroadcast::broadcast_to_room(
                MessageType::GameMessagePush,
                encode_body(&message),
            ));
    }

    fn queue_snapshot_push(&mut self, frame_id: u32, reason: &str, full_sync: bool) {
        #[derive(Serialize)]
        struct SnapshotEnvelope {
            frame_id: u32,
            reason: String,
            full_sync: bool,
            snapshot: CombatSnapshot,
        }

        let config = self.config_tables.current_snapshot();
        let snapshot = self
            .combat
            .snapshot(frame_id, config.combat_catalog.as_ref());
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

    fn timer_transfer_state(&self) -> RoomRuntimeTimerTransferState {
        let mut timer_state = RoomRuntimeTimerTransferState::new(
            "combat-demo",
            self.combat.last_tick_frame(),
            self.tick_count,
        );
        timer_state
            .metadata
            .insert("roomId".to_string(), self.room_id.clone());
        timer_state.timer_entries.push(RoomTimerTransferEntry {
            id: "combat-demo.snapshot-countdown".to_string(),
            timer_kind: "periodic_snapshot_countdown".to_string(),
            remaining_frames: self
                .next_snapshot_frame
                .saturating_sub(self.combat.last_tick_frame()),
            repeat_interval_frames: Some(SNAPSHOT_INTERVAL_FRAMES),
            payload_json: serde_json::json!({
                "snapshotIntervalFrames": SNAPSHOT_INTERVAL_FRAMES
            })
            .to_string(),
        });
        timer_state
            .scheduler_entries
            .push(RoomSchedulerTransferEntry {
                id: "combat-demo.snapshot-push".to_string(),
                scheduler_kind: "periodic_snapshot_push".to_string(),
                next_frame: self.next_snapshot_frame,
                interval_frames: Some(SNAPSHOT_INTERVAL_FRAMES),
                payload_json: serde_json::json!({
                    "reason": "tick_sync"
                })
                .to_string(),
            });
        timer_state
    }

    fn import_timer_transfer_state(
        &mut self,
        state: &RoomLogicTransferState,
    ) -> Result<(), &'static str> {
        let timer_state = state
            .timer_transfer_state()?
            .ok_or("ROOM_TRANSFER_INVALID_TIMER_STATE")?;
        if timer_state.runtime_summary.owner_kind != "combat-demo" {
            return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
        }
        if timer_state.runtime_summary.logical_frame != self.combat.last_tick_frame()
            || timer_state.runtime_summary.logical_tick != self.tick_count
        {
            return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
        }

        let snapshot_scheduler = timer_state
            .scheduler_entries
            .iter()
            .find(|entry| entry.id == "combat-demo.snapshot-push")
            .ok_or("ROOM_TRANSFER_INVALID_TIMER_STATE")?;
        if snapshot_scheduler.scheduler_kind != "periodic_snapshot_push"
            || snapshot_scheduler.interval_frames != Some(SNAPSHOT_INTERVAL_FRAMES)
            || snapshot_scheduler.next_frame <= self.combat.last_tick_frame()
            || snapshot_scheduler.next_frame != self.next_snapshot_frame
        {
            return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
        }

        let snapshot_timer = timer_state
            .timer_entries
            .iter()
            .find(|entry| entry.id == "combat-demo.snapshot-countdown")
            .ok_or("ROOM_TRANSFER_INVALID_TIMER_STATE")?;
        if snapshot_timer.timer_kind != "periodic_snapshot_countdown"
            || snapshot_timer.repeat_interval_frames != Some(SNAPSHOT_INTERVAL_FRAMES)
            || snapshot_timer.remaining_frames
                != snapshot_scheduler
                    .next_frame
                    .saturating_sub(self.combat.last_tick_frame())
        {
            return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
        }

        self.next_snapshot_frame = snapshot_scheduler.next_frame;
        Ok(())
    }

    fn export_npc_transfer_state_json(&self) -> Result<String, &'static str> {
        let mut npc_state = RoomNpcTransferState::new();
        npc_state.metadata.insert(
            "ownerKind".to_string(),
            serde_json::json!("combat-demo.training-dummies"),
        );
        npc_state.metadata.insert(
            "contractScope".to_string(),
            serde_json::json!("structured-runtime-skeleton"),
        );

        for entity in self.npc_snapshot_entities() {
            let Some(entity_kind) = npc_transfer_entity_kind(entity.entity_type) else {
                continue;
            };
            let mut npc_entity = RoomNpcTransferEntity::new(
                entity.entity_id,
                entity_kind,
                RoomNpcTransferPosition {
                    x: entity.x,
                    y: entity.y,
                },
                entity.hp,
                entity.max_hp,
                "training_dummy.idle",
            );
            npc_entity.skill_cooldowns = entity
                .skills
                .iter()
                .map(|skill| RoomNpcTransferSkillState {
                    skill_id: skill.skill_id,
                    cooldown_remaining: skill.cooldown_remaining,
                })
                .collect();
            npc_state.entities.push(npc_entity);
        }

        npc_state.to_json()
    }

    fn npc_snapshot_entities(&self) -> Vec<CombatEntitySnapshot> {
        let config = self.config_tables.current_snapshot();
        let snapshot = self.combat.snapshot(
            self.combat.last_tick_frame(),
            config.combat_catalog.as_ref(),
        );
        snapshot
            .entities
            .into_iter()
            .filter(|entity| npc_transfer_entity_kind(entity.entity_type).is_some())
            .collect()
    }

    fn validate_imported_npc_transfer_state(
        &self,
        state: &RoomLogicTransferState,
    ) -> Result<(), &'static str> {
        let npc_state = RoomNpcTransferState::from_json(&state.npc_state_json)?;
        let combat_entities = self.npc_snapshot_entities();
        if npc_state.entities.len() != combat_entities.len() {
            return Err("ROOM_TRANSFER_INVALID_NPC_STATE");
        }

        for npc_entity in &npc_state.entities {
            let combat_entity = combat_entities
                .iter()
                .find(|entity| entity.entity_id == npc_entity.entity_id)
                .ok_or("ROOM_TRANSFER_INVALID_NPC_STATE")?;
            let Some(entity_kind) = npc_transfer_entity_kind(combat_entity.entity_type) else {
                return Err("ROOM_TRANSFER_INVALID_NPC_STATE");
            };
            if npc_entity.entity_kind != entity_kind
                || npc_entity.position.x != combat_entity.x
                || npc_entity.position.y != combat_entity.y
                || npc_entity.hp != combat_entity.hp
                || npc_entity.max_hp != combat_entity.max_hp
            {
                return Err("ROOM_TRANSFER_INVALID_NPC_STATE");
            }
        }

        Ok(())
    }
}

fn npc_transfer_entity_kind(entity_type: EntityType) -> Option<&'static str> {
    match entity_type {
        EntityType::Npc => Some("npc"),
        EntityType::Monster => Some("monster"),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CombatDemoTransferLogicState {
    schema: String,
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    room_id: String,
    tick_count: u64,
    next_snapshot_frame: u32,
    roster: Vec<String>,
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
        self.next_snapshot_frame = SNAPSHOT_INTERVAL_FRAMES;
        self.queue_snapshot_push(0, "game_started", true);
    }

    fn on_game_ended(&mut self, _room_id: &str) {
        self.queue_snapshot_push(self.combat.last_tick_frame(), "game_ended", true);
        self.combat.clear();
        self.next_snapshot_frame = SNAPSHOT_INTERVAL_FRAMES;
    }

    fn on_tick(&mut self, frame_id: u32, fps: u16, inputs: &[PlayerInputRecord]) {
        self.tick_count = self.tick_count.saturating_add(1);
        let config = self.config_tables.current_snapshot();
        let catalog = config.combat_catalog.as_ref();

        let mut hooks = NoopCombatHooks;
        for input in inputs {
            match parse_player_input(input, &self.combat) {
                Ok(Some(command)) => {
                    match self.combat.execute_command(command, catalog, &mut hooks) {
                        CombatCommandResult::Accepted | CombatCommandResult::Ignored => {}
                        CombatCommandResult::Rejected { reason } => {
                            self.queue_input_reject_push(frame_id, &input.player_id, &reason);
                        }
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.queue_input_reject_push(frame_id, &input.player_id, error.error_code);
                }
            }
        }

        self.combat.tick(frame_id, fps, catalog, &mut hooks);
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

        let scheduled_snapshot = frame_id >= self.next_snapshot_frame;
        if scheduled_snapshot {
            while self.next_snapshot_frame <= frame_id {
                match self
                    .next_snapshot_frame
                    .checked_add(SNAPSHOT_INTERVAL_FRAMES)
                {
                    Some(next_frame) => self.next_snapshot_frame = next_frame,
                    None => {
                        self.next_snapshot_frame = u32::MAX;
                        break;
                    }
                }
            }
        }

        if scheduled_snapshot || force_snapshot {
            self.queue_snapshot_push(frame_id, "tick_sync", scheduled_snapshot);
        }
    }

    fn get_serialized_state(&self) -> String {
        #[derive(Serialize)]
        struct CombatRoomState<'a> {
            room_id: &'a str,
            tick_count: u64,
            next_snapshot_frame: u32,
            roster: &'a [String],
            snapshot: CombatSnapshot,
        }

        serde_json::to_string(&CombatRoomState {
            room_id: &self.room_id,
            tick_count: self.tick_count,
            next_snapshot_frame: self.next_snapshot_frame,
            roster: &self.roster,
            snapshot: self.combat.snapshot(
                self.combat.last_tick_frame(),
                self.config_tables
                    .current_snapshot()
                    .combat_catalog
                    .as_ref(),
            ),
        })
        .unwrap_or_default()
    }

    fn take_pending_broadcasts(&mut self) -> Vec<RoomLogicBroadcast> {
        std::mem::take(&mut self.pending_broadcasts)
    }
}

impl RoomLogicTransfer for CombatDemoLogic {
    fn export_transfer_state(&self) -> Result<RoomLogicTransferState, &'static str> {
        let logic_state = CombatDemoTransferLogicState {
            schema: COMBAT_DEMO_TRANSFER_SCHEMA.to_string(),
            schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
            room_id: self.room_id.clone(),
            tick_count: self.tick_count,
            next_snapshot_frame: self.next_snapshot_frame,
            roster: self.roster.clone(),
        };
        let timer_state_json = self.timer_transfer_state().to_json()?;

        Ok(RoomLogicTransferState {
            schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
            logic_state_json: serde_json::to_string(&logic_state)
                .map_err(|_| "ROOM_TRANSFER_INVALID_LOGIC_STATE")?,
            movement_state_json: String::new(),
            combat_state_json: self.combat.export_transfer_state_json()?,
            npc_state_json: self.export_npc_transfer_state_json()?,
            timer_state_json,
        })
    }

    fn import_transfer_state(
        &mut self,
        state: &RoomLogicTransferState,
    ) -> Result<(), &'static str> {
        if state.schema_version != ROOM_TRANSFER_SCHEMA_VERSION {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }

        let logic_state = serde_json::from_str::<serde_json::Value>(&state.logic_state_json)
            .map_err(|_| "ROOM_TRANSFER_INVALID_LOGIC_STATE")?;
        if logic_state
            .get("schema")
            .and_then(serde_json::Value::as_str)
            != Some(COMBAT_DEMO_TRANSFER_SCHEMA)
        {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        if logic_state
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            != Some(ROOM_TRANSFER_SCHEMA_VERSION as u64)
        {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }

        let logic_state = serde_json::from_value::<CombatDemoTransferLogicState>(logic_state)
            .map_err(|_| "ROOM_TRANSFER_INVALID_LOGIC_STATE")?;
        if !self.room_id.is_empty() && logic_state.room_id != self.room_id {
            return Err("ROOM_TRANSFER_INVALID_LOGIC_STATE");
        }
        if logic_state.room_id.trim().is_empty() {
            return Err("ROOM_TRANSFER_INVALID_LOGIC_STATE");
        }
        validate_transfer_roster(&logic_state.roster)?;
        let combat = RoomCombatEcs::import_transfer_state_json(&state.combat_state_json)?;

        self.room_id = logic_state.room_id;
        self.tick_count = logic_state.tick_count;
        self.next_snapshot_frame = logic_state.next_snapshot_frame;
        self.roster = logic_state.roster;
        self.combat = combat;
        self.validate_imported_npc_transfer_state(state)?;
        self.import_timer_transfer_state(state)?;
        self.pending_broadcasts.clear();

        Ok(())
    }
}

fn validate_transfer_roster(roster: &[String]) -> Result<(), &'static str> {
    let mut seen = HashSet::new();
    for player_id in roster {
        if player_id.trim().is_empty() || !seen.insert(player_id.as_str()) {
            return Err("ROOM_TRANSFER_INVALID_LOGIC_STATE");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    fn config_tables() -> ConfigTableRuntime {
        ConfigTableRuntime::load_with_scene_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("csv"),
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("scene"),
        )
        .expect("game-server csv fixture should load")
    }

    fn drain_broadcasts(logic: &mut CombatDemoLogic) -> Vec<serde_json::Value> {
        logic
            .take_pending_broadcasts()
            .into_iter()
            .filter_map(|broadcast| GameMessagePush::decode(broadcast.body.as_slice()).ok())
            .filter_map(|push| serde_json::from_str::<serde_json::Value>(&push.payload_json).ok())
            .collect()
    }

    #[test]
    fn transfer_state_roundtrip_restores_demo_scheduler() {
        let mut source = CombatDemoLogic::new(config_tables());
        source.on_room_created("room-combat-transfer");
        source.on_player_join("player-a");
        source.on_game_started("room-combat-transfer");
        drain_broadcasts(&mut source);

        for frame_id in 1..=SNAPSHOT_INTERVAL_FRAMES {
            source.on_tick(frame_id, 20, &[]);
        }
        let scheduled_before_export = drain_broadcasts(&mut source).into_iter().any(|payload| {
            payload["frame_id"] == SNAPSHOT_INTERVAL_FRAMES && payload["full_sync"] == true
        });
        assert!(scheduled_before_export);
        assert_eq!(source.next_snapshot_frame, SNAPSHOT_INTERVAL_FRAMES * 2);

        let exported = source.export_transfer_state().unwrap();
        let timer_state = exported.timer_transfer_state().unwrap().unwrap();
        let npc_state = RoomNpcTransferState::from_json(&exported.npc_state_json).unwrap();
        assert_eq!(npc_state.schema, "room-transfer.npc-state.v1");
        assert_eq!(npc_state.entities.len(), 2);
        assert!(npc_state.entities.iter().all(|entity| {
            entity.entity_kind == "monster" && entity.behavior_node == "training_dummy.idle"
        }));
        assert_eq!(npc_state.entities[0].entity_id, 2);
        assert_eq!(npc_state.entities[0].position.x, 120.0);
        assert_eq!(npc_state.entities[0].position.y, -16.0);
        assert_eq!(npc_state.entities[0].hp, 150);
        assert_eq!(npc_state.entities[0].max_hp, 150);
        assert_eq!(
            npc_state.entities[0]
                .skill_cooldowns
                .iter()
                .map(|skill| skill.skill_id)
                .collect::<Vec<_>>(),
            vec![1, 5]
        );
        assert!(npc_state.entities[0].blackboard.is_empty());
        assert!(npc_state.entities[0].threat_entries.is_empty());
        assert_eq!(timer_state.runtime_summary.owner_kind, "combat-demo");
        assert_eq!(
            timer_state.scheduler_entries[0].next_frame,
            SNAPSHOT_INTERVAL_FRAMES * 2
        );
        assert_eq!(
            timer_state.timer_entries[0].remaining_frames,
            SNAPSHOT_INTERVAL_FRAMES
        );

        let mut imported = CombatDemoLogic::new(config_tables());
        imported.on_room_created("room-combat-transfer");
        imported.import_transfer_state(&exported).unwrap();
        assert_eq!(imported.next_snapshot_frame, SNAPSHOT_INTERVAL_FRAMES * 2);

        imported.on_tick(SNAPSHOT_INTERVAL_FRAMES + 1, 20, &[]);
        assert!(
            drain_broadcasts(&mut imported)
                .into_iter()
                .all(|payload| payload["full_sync"] != true)
        );
        imported.on_tick(SNAPSHOT_INTERVAL_FRAMES * 2, 20, &[]);
        let scheduled_after_import = drain_broadcasts(&mut imported).into_iter().any(|payload| {
            payload["frame_id"] == SNAPSHOT_INTERVAL_FRAMES * 2 && payload["full_sync"] == true
        });
        assert!(scheduled_after_import);
        assert_eq!(imported.next_snapshot_frame, SNAPSHOT_INTERVAL_FRAMES * 3);
    }

    #[test]
    fn transfer_state_rejects_invalid_npc_state_contract() {
        let mut source = CombatDemoLogic::new(config_tables());
        source.on_room_created("room-combat-transfer");
        source.on_player_join("player-a");
        source.on_game_started("room-combat-transfer");
        source.on_tick(1, 20, &[]);

        let exported = source.export_transfer_state().unwrap();

        let mut unsupported_schema = exported.clone();
        let mut npc_json =
            serde_json::from_str::<serde_json::Value>(&unsupported_schema.npc_state_json).unwrap();
        npc_json["schema"] = serde_json::json!("room-transfer.npc-state.v2");
        unsupported_schema.npc_state_json = npc_json.to_string();
        let mut imported = CombatDemoLogic::new(config_tables());
        imported.on_room_created("room-combat-transfer");
        assert_eq!(
            imported.import_transfer_state(&unsupported_schema),
            Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA")
        );

        let mut duplicate_entity = exported.clone();
        let mut npc_json =
            serde_json::from_str::<serde_json::Value>(&duplicate_entity.npc_state_json).unwrap();
        let first_entity = npc_json["entities"][0].clone();
        npc_json["entities"]
            .as_array_mut()
            .unwrap()
            .push(first_entity);
        duplicate_entity.npc_state_json = npc_json.to_string();
        let mut imported = CombatDemoLogic::new(config_tables());
        imported.on_room_created("room-combat-transfer");
        assert_eq!(
            imported.import_transfer_state(&duplicate_entity),
            Err("ROOM_TRANSFER_INVALID_NPC_STATE")
        );

        let mut mismatched_entity = exported;
        let mut npc_json =
            serde_json::from_str::<serde_json::Value>(&mismatched_entity.npc_state_json).unwrap();
        npc_json["entities"][0]["hp"] = serde_json::json!(149);
        mismatched_entity.npc_state_json = npc_json.to_string();
        let mut imported = CombatDemoLogic::new(config_tables());
        imported.on_room_created("room-combat-transfer");
        assert_eq!(
            imported.import_transfer_state(&mismatched_entity),
            Err("ROOM_TRANSFER_INVALID_NPC_STATE")
        );
    }
}
