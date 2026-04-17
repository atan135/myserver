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
    #[serde(flatten)]
    pub client_state: ClientMovementStatePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaceToPayload {
    #[serde(rename = "dirX")]
    pub dir_x: f32,
    #[serde(rename = "dirY")]
    pub dir_y: f32,
    #[serde(flatten)]
    pub client_state: ClientMovementStatePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MoveStopPayload {
    #[serde(flatten)]
    pub client_state: ClientMovementStatePayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClientMovementStatePayload {
    #[serde(rename = "hasClientState", default)]
    pub has_client_state: bool,
    #[serde(rename = "clientX", default)]
    pub client_x: f32,
    #[serde(rename = "clientY", default)]
    pub client_y: f32,
    #[serde(rename = "clientFrameId", default)]
    pub client_frame_id: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ClientMovementState {
    pub frame_id: u32,
    pub position: Vec2,
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

#[derive(Debug, Clone, Copy)]
pub struct ParsedMovementInput {
    pub command: Option<MovementCommand>,
    pub client_state: Option<ClientMovementState>,
}

pub fn player_input_from_move_req(
    request: &MoveInputReq,
) -> Result<(&'static str, String), MovementInputError> {
    let client_state = ClientMovementStatePayload {
        has_client_state: request.has_client_state,
        client_x: request.client_x,
        client_y: request.client_y,
        client_frame_id: request.client_frame_id,
    };
    let input_type = MoveInputType::try_from(request.input_type).unwrap_or(MoveInputType::Unknown);
    match input_type {
        MoveInputType::MoveDir => {
            let payload = MoveDirectionPayload {
                dir_x: request.dir_x,
                dir_y: request.dir_y,
                client_state,
            };
            validate_direction(payload.dir_x, payload.dir_y)?;
            let payload_json = serde_json::to_string(&payload).map_err(|_| MovementInputError {
                error_code: "INVALID_MOVE_PAYLOAD_JSON",
            })?;
            Ok((ACTION_MOVE_DIR, payload_json))
        }
        MoveInputType::MoveStop => {
            if !client_state.has_client_state {
                return Ok((ACTION_MOVE_STOP, String::new()));
            }
            let payload_json =
                serde_json::to_string(&MoveStopPayload { client_state }).map_err(|_| {
                    MovementInputError {
                        error_code: "INVALID_MOVE_STOP_PAYLOAD_JSON",
                    }
                })?;
            Ok((ACTION_MOVE_STOP, payload_json))
        }
        MoveInputType::FaceTo => {
            let payload = FaceToPayload {
                dir_x: request.dir_x,
                dir_y: request.dir_y,
                client_state,
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

pub fn parse_player_input(record: &PlayerInputRecord) -> Result<ParsedMovementInput, MovementInputError> {
    match record.action.as_str() {
        ACTION_MOVE_DIR => {
            let payload: MoveDirectionPayload =
                serde_json::from_str(&record.payload_json).map_err(|_| MovementInputError {
                    error_code: "INVALID_MOVE_DIR_PAYLOAD",
                })?;
            validate_direction(payload.dir_x, payload.dir_y)?;
            Ok(ParsedMovementInput {
                command: Some(MovementCommand::MoveDir(Vec2 {
                    x: payload.dir_x,
                    y: payload.dir_y,
                })),
                client_state: decode_client_state(record.frame_id, &payload.client_state),
            })
        }
        ACTION_MOVE_STOP => {
            let payload = if record.payload_json.is_empty() {
                MoveStopPayload::default()
            } else {
                serde_json::from_str(&record.payload_json).map_err(|_| MovementInputError {
                    error_code: "INVALID_MOVE_STOP_PAYLOAD",
                })?
            };
            Ok(ParsedMovementInput {
                command: Some(MovementCommand::MoveStop),
                client_state: decode_client_state(record.frame_id, &payload.client_state),
            })
        }
        ACTION_FACE_TO => {
            let payload: FaceToPayload =
                serde_json::from_str(&record.payload_json).map_err(|_| MovementInputError {
                    error_code: "INVALID_FACE_TO_PAYLOAD",
                })?;
            validate_direction(payload.dir_x, payload.dir_y)?;
            Ok(ParsedMovementInput {
                command: Some(MovementCommand::FaceTo(Vec2 {
                    x: payload.dir_x,
                    y: payload.dir_y,
                })),
                client_state: decode_client_state(record.frame_id, &payload.client_state),
            })
        }
        _ => Ok(ParsedMovementInput {
            command: None,
            client_state: None,
        }),
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

fn decode_client_state(
    default_frame_id: u32,
    payload: &ClientMovementStatePayload,
) -> Option<ClientMovementState> {
    if !payload.has_client_state {
        return None;
    }

    Some(ClientMovementState {
        frame_id: if payload.client_frame_id == 0 {
            default_frame_id
        } else {
            payload.client_frame_id
        },
        position: Vec2 {
            x: payload.client_x,
            y: payload.client_y,
        },
    })
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
            has_client_state: true,
            client_x: 10.0,
            client_y: 12.0,
            client_frame_id: 3,
        };

        let (action, payload_json) = player_input_from_move_req(&request).unwrap();
        assert_eq!(action, ACTION_MOVE_DIR);
        assert!(payload_json.contains("\"dirX\":3.0"));
        assert!(payload_json.contains("\"dirY\":4.0"));
        assert!(payload_json.contains("\"hasClientState\":true"));
        assert!(payload_json.contains("\"clientX\":10.0"));
    }
}
