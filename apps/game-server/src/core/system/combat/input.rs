use serde::{Deserialize, Serialize};

use crate::core::room::PlayerInputRecord;

use super::Position;
use super::ecs::{CombatCommand, EntityId, RoomCombatEcs, SkillCastRequest};

pub const ACTION_COMBAT_CAST_SKILL: &str = "combat_cast_skill";
pub const ACTION_COMBAT_APPLY_BUFF: &str = "combat_apply_buff";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CastSkillInputPayload {
    #[serde(rename = "skillId")]
    pub skill_id: u16,
    #[serde(rename = "targetEntityId")]
    pub target_entity_id: Option<EntityId>,
    #[serde(rename = "targetCharacterId")]
    pub target_character_id: Option<String>,
    #[serde(rename = "targetX")]
    pub target_x: Option<f32>,
    #[serde(rename = "targetY")]
    pub target_y: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyBuffInputPayload {
    #[serde(rename = "targetEntityId")]
    pub target_entity_id: Option<EntityId>,
    #[serde(rename = "targetCharacterId")]
    pub target_character_id: Option<String>,
    #[serde(rename = "buffId")]
    pub buff_id: u16,
    #[serde(rename = "durationFrames")]
    pub duration_frames: Option<u16>,
}

#[derive(Debug, Clone, Copy)]
pub struct CombatInputError {
    pub error_code: &'static str,
}

pub fn parse_player_input(
    record: &PlayerInputRecord,
    combat: &RoomCombatEcs,
) -> Result<Option<CombatCommand>, CombatInputError> {
    match record.action.as_str() {
        ACTION_COMBAT_CAST_SKILL => {
            let payload: CastSkillInputPayload = serde_json::from_str(&record.payload_json)
                .map_err(|_| CombatInputError {
                    error_code: "INVALID_COMBAT_CAST_SKILL_PAYLOAD",
                })?;
            let source_entity =
                combat
                    .entity_id_by_character(&record.character_id)
                    .ok_or(CombatInputError {
                        error_code: "COMBAT_CHARACTER_ENTITY_NOT_FOUND",
                    })?;
            let target_entity = resolve_target_entity(
                payload.target_entity_id,
                payload.target_character_id.as_deref(),
                combat,
            )?;
            let target_point = match (payload.target_x, payload.target_y) {
                (Some(x), Some(y)) => Some(Position { x, y }),
                (None, None) => None,
                _ => {
                    return Err(CombatInputError {
                        error_code: "INVALID_COMBAT_TARGET_POINT",
                    });
                }
            };

            Ok(Some(CombatCommand::CastSkill(SkillCastRequest {
                frame_id: record.frame_id,
                source_entity,
                skill_id: payload.skill_id,
                target_entity,
                target_point,
            })))
        }
        ACTION_COMBAT_APPLY_BUFF => {
            let payload: ApplyBuffInputPayload = serde_json::from_str(&record.payload_json)
                .map_err(|_| CombatInputError {
                    error_code: "INVALID_COMBAT_APPLY_BUFF_PAYLOAD",
                })?;
            let source_entity =
                combat
                    .entity_id_by_character(&record.character_id)
                    .ok_or(CombatInputError {
                        error_code: "COMBAT_CHARACTER_ENTITY_NOT_FOUND",
                    })?;
            let target_entity = resolve_target_entity(
                payload.target_entity_id,
                payload.target_character_id.as_deref(),
                combat,
            )?
            .ok_or(CombatInputError {
                error_code: "COMBAT_TARGET_REQUIRED",
            })?;

            Ok(Some(CombatCommand::ApplyBuff {
                frame_id: record.frame_id,
                source_entity: Some(source_entity),
                target_entity,
                buff_id: payload.buff_id,
                duration_frames: payload.duration_frames,
            }))
        }
        _ => Ok(None),
    }
}

fn resolve_target_entity(
    target_entity_id: Option<EntityId>,
    target_character_id: Option<&str>,
    combat: &RoomCombatEcs,
) -> Result<Option<EntityId>, CombatInputError> {
    match (target_entity_id, target_character_id) {
        (Some(entity_id), None) => Ok(Some(entity_id)),
        (None, Some(character_id)) => {
            combat
                .entity_id_by_character(character_id)
                .map(Some)
                .ok_or(CombatInputError {
                    error_code: "COMBAT_TARGET_CHARACTER_NOT_FOUND",
                })
        }
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(CombatInputError {
            error_code: "COMBAT_TARGET_AMBIGUOUS",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::system::combat::{CombatEntityBlueprint, RoomCombatEcs};

    #[test]
    fn cast_skill_input_resolves_target_character_to_entity() {
        let mut combat = RoomCombatEcs::new();
        let _ = combat
            .spawn_entity(
                CombatEntityBlueprint::player("player-a", 1, Position { x: 0.0, y: 0.0 })
                    .with_skills(&[1]),
            )
            .unwrap();
        let target_entity = combat
            .spawn_entity(CombatEntityBlueprint::player(
                "player-b",
                2,
                Position { x: 10.0, y: 0.0 },
            ))
            .unwrap();

        let record = PlayerInputRecord {
            frame_id: 5,
            character_id: "player-a".to_string(),
            action: ACTION_COMBAT_CAST_SKILL.to_string(),
            payload_json: "{\"skillId\":1,\"targetCharacterId\":\"player-b\"}".to_string(),
            received_at: std::time::Instant::now(),
            is_synthetic: false,
        };

        let command = parse_player_input(&record, &combat).unwrap().unwrap();
        match command {
            CombatCommand::CastSkill(request) => {
                assert_eq!(request.target_entity, Some(target_entity));
            }
            _ => panic!("unexpected combat command"),
        }
    }
}
