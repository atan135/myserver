use serde::{Deserialize, Serialize};

use crate::core::room::PlayerInputRecord;
use crate::core::system::movement::state::Vec2;
use crate::pb::{MoveInputReq, MoveInputType};

pub const ACTION_MOVE_DIR: &str = "move_dir";
pub const ACTION_MOVE_STOP: &str = "move_stop";
pub const ACTION_FACE_TO: &str = "face_to";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveDirectionPayload {
    #[serde(rename = "dirX")]
    pub dir_x: f32,
    #[serde(rename = "dirY")]
    pub dir_y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceToPayload {
    #[serde(rename = "dirX")]
    pub dir_x: f32,
    #[serde(rename = "dirY")]
    pub dir_y: f32,
}

#[derive(Debug, Clone, Copy)]
pub enum MovementCommand {
    MoveDir(Vec2),
    MoveStop,
    FaceTo(Vec2),
}

#[derive(Debug)]
pub struct MovementInputError {
    pub error_code: &'static str,
}

pub fn player_input_from_move_req(
    request: &MoveInputReq,
) -> Result<(&'static str, String), MovementInputError> {
    let input_type = MoveInputType::try_from(request.input_type).unwrap_or(MoveInputType::Unknown);
    match input_type {
        MoveInputType::MoveDir => {
            let payload = MoveDirectionPayload {
                dir_x: request.dir_x,
                dir_y: request.dir_y,
            };
            validate_direction(payload.dir_x, payload.dir_y)?;
            let payload_json = serde_json::to_string(&payload).map_err(|_| MovementInputError {
                error_code: "INVALID_MOVE_PAYLOAD_JSON",
            })?;
            Ok((ACTION_MOVE_DIR, payload_json))
        }
        MoveInputType::MoveStop => Ok((ACTION_MOVE_STOP, String::new())),
        MoveInputType::FaceTo => {
            let payload = FaceToPayload {
                dir_x: request.dir_x,
                dir_y: request.dir_y,
            };
            validate_direction(payload.dir_x, payload.dir_y)?;
            let payload_json = serde_json::to_string(&payload).map_err(|_| MovementInputError {
                error_code: "INVALID_FACE_TO_PAYLOAD_JSON",
            })?;
            Ok((ACTION_FACE_TO, payload_json))
        }
        MoveInputType::Unknown => Err(MovementInputError {
            error_code: "MOVE_INPUT_TYPE_UNKNOWN",
        }),
    }
}

pub fn parse_player_input(record: &PlayerInputRecord) -> Result<Option<MovementCommand>, MovementInputError> {
    match record.action.as_str() {
        ACTION_MOVE_DIR => {
            let payload: MoveDirectionPayload =
                serde_json::from_str(&record.payload_json).map_err(|_| MovementInputError {
                    error_code: "INVALID_MOVE_DIR_PAYLOAD",
                })?;
            validate_direction(payload.dir_x, payload.dir_y)?;
            Ok(Some(MovementCommand::MoveDir(Vec2 {
                x: payload.dir_x,
                y: payload.dir_y,
            })))
        }
        ACTION_MOVE_STOP => Ok(Some(MovementCommand::MoveStop)),
        ACTION_FACE_TO => {
            let payload: FaceToPayload =
                serde_json::from_str(&record.payload_json).map_err(|_| MovementInputError {
                    error_code: "INVALID_FACE_TO_PAYLOAD",
                })?;
            validate_direction(payload.dir_x, payload.dir_y)?;
            Ok(Some(MovementCommand::FaceTo(Vec2 {
                x: payload.dir_x,
                y: payload.dir_y,
            })))
        }
        _ => Ok(None),
    }
}

fn validate_direction(dir_x: f32, dir_y: f32) -> Result<(), MovementInputError> {
    let len_sq = dir_x * dir_x + dir_y * dir_y;
    if len_sq <= f32::EPSILON {
        return Err(MovementInputError {
            error_code: "MOVE_DIRECTION_ZERO",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_input_req_maps_to_action_and_payload() {
        let request = MoveInputReq {
            frame_id: 3,
            input_type: MoveInputType::MoveDir as i32,
            dir_x: 3.0,
            dir_y: 4.0,
        };

        let (action, payload_json) = player_input_from_move_req(&request).unwrap();
        assert_eq!(action, ACTION_MOVE_DIR);
        assert!(payload_json.contains("\"dirX\":3.0"));
        assert!(payload_json.contains("\"dirY\":4.0"));
    }
}
