use crate::core::room::PlayerInputRecord;
use crate::core::system::movement::input::parse_player_input;
use crate::core::system::movement::state::{ClientStateSample, RoomMovementState, Vec2};
use crate::core::system::scene::SceneQuery;
use crate::pb::{EntityTransform, MovementCorrectionReason};

#[derive(Debug, Clone)]
pub struct MovementRejectRecord {
    pub player_id: String,
    pub error_code: String,
    pub corrected: EntityTransform,
    pub reason_code: i32,
    pub client_state: Option<ClientStateSample>,
    pub server_x: f32,
    pub server_y: f32,
}

#[derive(Debug, Clone)]
pub struct MovementDriftRecord {
    pub player_id: String,
    pub client_state: ClientStateSample,
    pub authoritative: EntityTransform,
    pub drift_distance: f32,
}

#[derive(Debug, Clone, Default)]
pub struct SimulationTickResult {
    pub changed_entities: Vec<EntityTransform>,
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
    let mut players_with_client_state = std::collections::BTreeSet::new();

    for input in inputs {
        let Some(dense_index) = state.dense_index_by_player(&input.player_id) else {
            continue;
        };
        let Some(before) = state.entity_proto_at(dense_index) else {
            continue;
        };
        match parse_player_input(input) {
            Ok(parsed) => {
                if let Some(client_state) = parsed.client_state {
                    state.set_client_state_for_player(&input.player_id, client_state);
                    players_with_client_state.insert(input.player_id.clone());
                }
                if let Some(command) = parsed.command {
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
                    player_id: input.player_id.clone(),
                    error_code: error.error_code.to_string(),
                    corrected: corrected.clone(),
                    reason_code: MovementCorrectionReason::MovementRejected as i32,
                    client_state: state.client_state_for_player(&input.player_id),
                    server_x: corrected.x,
                    server_y: corrected.y,
                });
            }
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
        let clamped = scene_query.clamp_position(
            scene_id,
            position.x,
            position.y,
            desired_x,
            desired_y,
        );

        let _ = state.set_position_at(
            dense_index,
            Vec2 {
                x: clamped.x,
                y: clamped.y,
            },
        );
        if clamped.blocked {
            let _ = state.set_moving_at(dense_index, false);
            let Some(player_id) = state.entity_player_id(dense_index) else {
                continue;
            };
            let Some(corrected) = state.entity_proto_at(dense_index) else {
                continue;
            };
            rejects.push(MovementRejectRecord {
                player_id: player_id.to_string(),
                error_code: "MOVEMENT_BLOCKED".to_string(),
                corrected: corrected.clone(),
                reason_code: MovementCorrectionReason::CollisionBlocked as i32,
                client_state: state.client_state_for_player(player_id),
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

    let drifted_players = players_with_client_state
        .into_iter()
        .filter_map(|player_id| {
            if !state.should_force_correction_for_player(&player_id) {
                return None;
            }
            let client_state = state.client_state_for_player(&player_id)?;
            let authoritative = state.entity(&player_id)?.to_proto();
            let drift_distance = state.drift_distance_for_player(&player_id)?;
            Some(MovementDriftRecord {
                player_id,
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

    let _ = frame_id;
    SimulationTickResult {
        changed_entities,
        rejects,
        drifted_players,
    }
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
        state.spawn_player("player-a", &spawn, 4.0);

        let input = PlayerInputRecord {
            frame_id: 1,
            player_id: "player-a".to_string(),
            action: "move_dir".to_string(),
            payload_json: "{\"dirX\":1.0,\"dirY\":0.0}".to_string(),
            received_at: Instant::now(),
        };

        let result = tick_movement(&mut state, 1, 20, &[input], &BlockingScene);
        assert_eq!(result.rejects.len(), 1);
        assert!(!state.entity("player-a").unwrap().moving);
    }

    #[test]
    fn client_drift_is_detected_when_threshold_exceeded() {
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

        let mut state = RoomMovementState::new(1, 3);
        state.set_correction_config(3, 0.5, 0.0, false);
        let spawn = build_spawn();
        state.spawn_player("player-a", &spawn, 4.0);

        let input = PlayerInputRecord {
            frame_id: 1,
            player_id: "player-a".to_string(),
            action: "move_dir".to_string(),
            payload_json:
                "{\"dirX\":1.0,\"dirY\":0.0,\"hasClientState\":true,\"clientX\":99.0,\"clientY\":99.0,\"clientFrameId\":1}"
                    .to_string(),
            received_at: Instant::now(),
        };

        let result = tick_movement(&mut state, 1, 20, &[input], &OpenScene);
        assert_eq!(result.drifted_players.len(), 1);
        assert_eq!(result.drifted_players[0].player_id, "player-a");
    }
}
