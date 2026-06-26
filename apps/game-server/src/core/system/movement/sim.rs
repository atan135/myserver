use crate::core::room::PlayerInputRecord;
use crate::core::system::movement::input::parse_player_input;
use crate::core::system::movement::state::{ClientStateSample, RoomMovementState, Vec2};
use crate::core::system::scene::SceneQuery;
use crate::pb::{EntityTransform, MovementCorrectionReason};

#[derive(Debug, Clone)]
pub struct MovementRejectRecord {
    pub character_id: String,
    pub error_code: String,
    pub corrected: EntityTransform,
    pub reason_code: i32,
    pub client_state: Option<ClientStateSample>,
    pub server_x: f32,
    pub server_y: f32,
}

#[derive(Debug, Clone)]
pub struct MovementDriftRecord {
    pub character_id: String,
    pub client_state: ClientStateSample,
    pub authoritative: EntityTransform,
    pub drift_distance: f32,
}

#[derive(Debug, Clone, Default)]
pub struct SimulationTickResult {
    pub changed_entities: Vec<EntityTransform>,
    pub control_timeout_entities: Vec<EntityTransform>,
    pub rejects: Vec<MovementRejectRecord>,
    pub drifted_players: Vec<MovementDriftRecord>,
}

pub fn tick_movement<S: SceneQuery>(
    state: &mut RoomMovementState,
    frame_id: u32,
    fps: u16,
    inputs: &[PlayerInputRecord],
    scene_query: &S,
) -> SimulationTickResult {
    let mut changed_by_entity = std::collections::HashSet::new();
    let mut rejects = Vec::new();
    let mut characters_with_client_state = std::collections::BTreeSet::new();
    let mut characters_with_locomotion_control = std::collections::BTreeSet::new();

    for input in inputs {
        let Some(dense_index) = state.dense_index_by_character(&input.character_id) else {
            continue;
        };
        let Some(before) = state.entity_proto_at(dense_index) else {
            continue;
        };
        match parse_player_input(input) {
            Ok(parsed) => {
                if let Some(client_state) = parsed.client_state {
                    state.set_client_state_for_character(&input.character_id, client_state);
                    characters_with_client_state.insert(input.character_id.clone());
                }
                if let Some(command) = parsed.command {
                    if command.is_locomotion_control() && !input.is_synthetic {
                        state.reset_missing_movement_control_frames(&input.character_id);
                        characters_with_locomotion_control.insert(input.character_id.clone());
                    }
                    if state.apply_command_at(dense_index, input.frame_id, command) {
                        let Some(after) = state.entity_proto_at(dense_index) else {
                            continue;
                        };
                        if entity_changed(&before, &after) {
                            changed_by_entity.insert(after.entity_id);
                        }
                    }
                }
            }
            Err(error) => {
                let Some(corrected) = state.entity_proto_at(dense_index) else {
                    continue;
                };
                rejects.push(MovementRejectRecord {
                    character_id: input.character_id.clone(),
                    error_code: error.error_code.to_string(),
                    corrected: corrected.clone(),
                    reason_code: MovementCorrectionReason::MovementRejected as i32,
                    client_state: state.client_state_for_character(&input.character_id),
                    server_x: corrected.x,
                    server_y: corrected.y,
                });
            }
        }
    }

    let control_timeout_entities = stop_characters_without_locomotion_control(
        state,
        frame_id,
        &characters_with_locomotion_control,
        &mut changed_by_entity,
    );
    for timeout_entity in &control_timeout_entities {
        for reject in rejects
            .iter_mut()
            .filter(|reject| reject.character_id == timeout_entity.character_id)
        {
            reject.corrected = timeout_entity.clone();
            reject.server_x = timeout_entity.x;
            reject.server_y = timeout_entity.y;
        }
    }

    let delta_seconds = 1.0 / f32::from(fps.max(1));
    for dense_index in state.dense_indices() {
        let Some(is_moving) = state.is_moving_at(dense_index) else {
            continue;
        };
        if !is_moving {
            continue;
        }

        let (Some(before), Some(position), Some(direction), Some(speed), Some(scene_id)) = (
            state.entity_proto_at(dense_index),
            state.position_at(dense_index),
            state.direction_at(dense_index),
            state.speed_at(dense_index),
            state.entity_scene_id(dense_index),
        ) else {
            continue;
        };
        let desired_x = position.x + direction.x * speed * delta_seconds;
        let desired_y = position.y + direction.y * speed * delta_seconds;
        let clamped =
            scene_query.clamp_position(scene_id, position.x, position.y, desired_x, desired_y);

        let _ = state.set_position_at(
            dense_index,
            Vec2 {
                x: clamped.x,
                y: clamped.y,
            },
        );
        if clamped.blocked {
            let _ = state.set_moving_at(dense_index, false);
            let Some(character_id) = state.entity_character_id(dense_index) else {
                continue;
            };
            let Some(corrected) = state.entity_proto_at(dense_index) else {
                continue;
            };
            rejects.push(MovementRejectRecord {
                character_id: character_id.to_string(),
                error_code: "MOVEMENT_BLOCKED".to_string(),
                corrected: corrected.clone(),
                reason_code: MovementCorrectionReason::CollisionBlocked as i32,
                client_state: state.client_state_for_character(character_id),
                server_x: corrected.x,
                server_y: corrected.y,
            });
        }

        let Some(after) = state.entity_proto_at(dense_index) else {
            continue;
        };
        if entity_changed(&before, &after) {
            changed_by_entity.insert(after.entity_id);
        }
    }

    let drifted_players = characters_with_client_state
        .into_iter()
        .filter_map(|character_id| {
            if !state.should_force_correction_for_character(&character_id) {
                return None;
            }
            let client_state = state.client_state_for_character(&character_id)?;
            let authoritative = state.entity(&character_id)?.to_proto();
            let drift_distance = state.drift_distance_for_character(&character_id)?;
            Some(MovementDriftRecord {
                character_id,
                client_state,
                authoritative,
                drift_distance,
            })
        })
        .collect();

    let changed_entities = state
        .dense_indices()
        .filter_map(|dense_index| state.entity_proto_at(dense_index))
        .filter(|entity| changed_by_entity.contains(&entity.entity_id))
        .collect();

    SimulationTickResult {
        changed_entities,
        control_timeout_entities,
        rejects,
        drifted_players,
    }
}

fn stop_characters_without_locomotion_control(
    state: &mut RoomMovementState,
    frame_id: u32,
    characters_with_locomotion_control: &std::collections::BTreeSet<String>,
    changed_by_entity: &mut std::collections::HashSet<u64>,
) -> Vec<EntityTransform> {
    if state.movement_control_stop_frames == 0 {
        return Vec::new();
    }

    let character_ids = state
        .dense_indices()
        .filter_map(|dense_index| state.entity_character_id(dense_index).map(str::to_string))
        .collect::<Vec<_>>();

    let mut stopped_entities = Vec::new();
    for character_id in character_ids {
        let Some(dense_index) = state.dense_index_by_character(&character_id) else {
            continue;
        };
        let is_moving = state.is_moving_at(dense_index).unwrap_or(false);
        if !is_moving {
            state.reset_missing_movement_control_frames(&character_id);
            continue;
        }
        if characters_with_locomotion_control.contains(&character_id) {
            continue;
        }

        let missing_frames = state.increment_missing_movement_control_frames(&character_id);
        if missing_frames < state.movement_control_stop_frames {
            continue;
        }

        if let Some(stopped) = state.stop_character(&character_id, frame_id) {
            changed_by_entity.insert(stopped.entity_id);
            stopped_entities.push(stopped);
        }
    }

    stopped_entities
}

fn entity_changed(before: &EntityTransform, after: &EntityTransform) -> bool {
    before.x != after.x
        || before.y != after.y
        || before.dir_x != after.dir_x
        || before.dir_y != after.dir_y
        || before.moving != after.moving
        || before.last_input_frame != after.last_input_frame
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use crate::core::room::PlayerInputRecord;
    use crate::core::system::movement::state::RoomMovementState;
    use crate::core::system::scene::SceneQuery;
    use crate::core::system::scene::query::{
        ClampPositionResult, SceneDefinition, SceneSpawnPointDefinition,
    };

    use super::tick_movement;

    fn build_spawn() -> SceneSpawnPointDefinition {
        SceneSpawnPointDefinition {
            id: 1001,
            scene_id: 1,
            code: "spawn".to_string(),
            spawn_type: "player".to_string(),
            x: 1.0,
            y: 1.0,
            dir_x: 1.0,
            dir_y: 0.0,
            radius: 0.0,
            tags: Vec::new(),
        }
    }

    struct OpenScene;

    impl SceneQuery for OpenScene {
        fn scene(&self, _scene_id: i32) -> Option<&SceneDefinition> {
            None
        }

        fn spawn_point(&self, _spawn_id: i32) -> Option<&SceneSpawnPointDefinition> {
            None
        }

        fn is_walkable(&self, _scene_id: i32, _world_x: f32, _world_y: f32) -> bool {
            true
        }

        fn clamp_position(
            &self,
            _scene_id: i32,
            _from_x: f32,
            _from_y: f32,
            to_x: f32,
            to_y: f32,
        ) -> ClampPositionResult {
            ClampPositionResult {
                x: to_x,
                y: to_y,
                blocked: false,
            }
        }
    }

    fn movement_input(
        frame_id: u32,
        character_id: &str,
        action: &str,
        payload_json: &str,
        is_synthetic: bool,
    ) -> PlayerInputRecord {
        PlayerInputRecord {
            frame_id,
            character_id: character_id.to_string(),
            action: action.to_string(),
            payload_json: payload_json.to_string(),
            received_at: Instant::now(),
            is_synthetic,
        }
    }

    #[test]
    fn blocked_movement_stops_entity_and_emits_reject() {
        struct BlockingScene;

        impl SceneQuery for BlockingScene {
            fn scene(&self, _scene_id: i32) -> Option<&SceneDefinition> {
                None
            }

            fn spawn_point(&self, _spawn_id: i32) -> Option<&SceneSpawnPointDefinition> {
                None
            }

            fn is_walkable(&self, _scene_id: i32, _world_x: f32, _world_y: f32) -> bool {
                false
            }

            fn clamp_position(
                &self,
                _scene_id: i32,
                from_x: f32,
                from_y: f32,
                _to_x: f32,
                _to_y: f32,
            ) -> ClampPositionResult {
                ClampPositionResult {
                    x: from_x,
                    y: from_y,
                    blocked: true,
                }
            }
        }

        let mut state = RoomMovementState::new(1, 3);
        let spawn = build_spawn();
        state.spawn_character("player-a", &spawn, 4.0);

        let input = movement_input(
            1,
            "player-a",
            "move_dir",
            "{\"dirX\":1.0,\"dirY\":0.0}",
            false,
        );

        let result = tick_movement(&mut state, 1, 20, &[input], &BlockingScene);
        assert_eq!(result.rejects.len(), 1);
        assert!(!state.entity("player-a").unwrap().moving);
    }

    #[test]
    fn client_drift_is_detected_when_threshold_exceeded() {
        let mut state = RoomMovementState::new(1, 3);
        state.set_correction_config(3, 0.5, 0.0, false);
        let spawn = build_spawn();
        state.spawn_character("player-a", &spawn, 4.0);

        let input = movement_input(
            1,
            "player-a",
            "move_dir",
            "{\"dirX\":1.0,\"dirY\":0.0,\"hasClientState\":true,\"clientX\":99.0,\"clientY\":99.0,\"clientFrameId\":1}",
            false,
        );

        let result = tick_movement(&mut state, 1, 20, &[input], &OpenScene);
        assert_eq!(result.drifted_players.len(), 1);
        assert_eq!(result.drifted_players[0].character_id, "player-a");
    }

    #[test]
    fn missing_movement_control_stops_after_threshold() {
        let mut state = RoomMovementState::new(1, 3);
        state.set_movement_control_stop_frames(2);
        let spawn = build_spawn();
        state.spawn_character("player-a", &spawn, 4.0);

        let move_dir = movement_input(
            1,
            "player-a",
            "move_dir",
            "{\"dirX\":1.0,\"dirY\":0.0}",
            false,
        );
        let result = tick_movement(&mut state, 1, 20, &[move_dir], &OpenScene);
        assert!(result.control_timeout_entities.is_empty());
        assert!(state.entity("player-a").unwrap().moving);

        let result = tick_movement(&mut state, 2, 20, &[], &OpenScene);
        assert!(result.control_timeout_entities.is_empty());
        assert!(state.entity("player-a").unwrap().moving);

        let result = tick_movement(&mut state, 3, 20, &[], &OpenScene);
        assert_eq!(result.control_timeout_entities.len(), 1);
        let entity = state.entity("player-a").unwrap();
        assert!(!entity.moving);
        assert_eq!(entity.last_input_frame, 3);
    }

    #[test]
    fn face_to_does_not_keep_movement_control_alive() {
        let mut state = RoomMovementState::new(1, 3);
        state.set_movement_control_stop_frames(1);
        let spawn = build_spawn();
        state.spawn_character("player-a", &spawn, 4.0);

        let move_dir = movement_input(
            1,
            "player-a",
            "move_dir",
            "{\"dirX\":1.0,\"dirY\":0.0}",
            false,
        );
        tick_movement(&mut state, 1, 20, &[move_dir], &OpenScene);

        let face_to = movement_input(
            2,
            "player-a",
            "face_to",
            "{\"dirX\":0.0,\"dirY\":1.0}",
            false,
        );
        let result = tick_movement(&mut state, 2, 20, &[face_to], &OpenScene);
        assert_eq!(result.control_timeout_entities.len(), 1);
        let entity = state.entity("player-a").unwrap();
        assert!(!entity.moving);
        assert_eq!(entity.direction.x, 0.0);
        assert_eq!(entity.direction.y, 1.0);
    }

    #[test]
    fn synthetic_repeated_movement_does_not_keep_control_alive() {
        let mut state = RoomMovementState::new(1, 3);
        state.set_movement_control_stop_frames(1);
        let spawn = build_spawn();
        state.spawn_character("player-a", &spawn, 4.0);

        let move_dir = movement_input(
            1,
            "player-a",
            "move_dir",
            "{\"dirX\":1.0,\"dirY\":0.0}",
            false,
        );
        tick_movement(&mut state, 1, 20, &[move_dir], &OpenScene);

        let repeated_move_dir = movement_input(
            2,
            "player-a",
            "move_dir",
            "{\"dirX\":1.0,\"dirY\":0.0}",
            true,
        );
        let result = tick_movement(&mut state, 2, 20, &[repeated_move_dir], &OpenScene);
        assert_eq!(result.control_timeout_entities.len(), 1);
        assert!(!state.entity("player-a").unwrap().moving);
    }
}
