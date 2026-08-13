//! Offline player gameplay flows built from the current shared protobuf and
//! lockstep scenario boundaries.
//!
//! The module generates and validates packets but deliberately has no socket
//! or KCP connector. A live runner may consume these plans only after the
//! existing execution and protection gates admit the run.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use game_protocol::{MessageType, Packet};
use lockstep_client::online::{PlayerInputPlan, build_player_input_plan};
use lockstep_client::scenario::Scenario;
use prost::Message;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::metrics::{Metrics, MetricsSnapshot};
use crate::pb::{
    FrameBundlePush, GetInventoryReq, ItemEquipReq, ItemUseReq, MoveInputReq, MoveInputType,
    PlayerInputReq, RoomJoinReq, RoomLeaveReq, RoomReadyReq, RoomStartReq,
};
use crate::step::{ExpectedResponse, Idempotency, RetryPolicy, ScenarioStep};

pub const DEFAULT_STEP_TIMEOUT_MS: u64 = 2_000;
pub const DEFAULT_THINK_TIME_MS: u64 = 100;
pub const DEFAULT_MAX_MESSAGES_PER_CONNECTION_PER_SECOND: u32 = 20;
pub const HIGH_FREQUENCY_MAX_MESSAGES_PER_CONNECTION_PER_SECOND: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerProfile {
    Idle,
    Normal,
    HighFrequency,
}

/// A globally ordered packet for the narrowly scoped two-account
/// `default_match` smoke. The packet body is still generated from the shared
/// protobuf type; `player_index` only selects the already-authenticated
/// session that sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatedGameplayPacket {
    pub player_index: usize,
    pub packet: PlannedPacket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameplayStep {
    pub name: &'static str,
    pub request_type: MessageType,
    pub response_type: Option<MessageType>,
    pub timeout_ms: u64,
    pub think_time_ms: u64,
    pub expected: ExpectedResponse,
    pub idempotency: Idempotency,
    pub retry: RetryPolicy,
    pub max_messages_per_connection_per_second: u32,
}

impl GameplayStep {
    pub fn scenario_step(&self) -> ScenarioStep {
        ScenarioStep {
            name: self.name.to_owned(),
            timeout_ms: self.timeout_ms,
            think_time_ms: self.think_time_ms,
            expected: self.expected.clone(),
            idempotency: self.idempotency,
            retry: self.retry.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), GameplayError> {
        if self.response_type.is_none() && self.request_type != MessageType::RoomLeaveReq {
            return Err(GameplayError::MissingExpectedResponse(self.name));
        }
        if self.max_messages_per_connection_per_second == 0 {
            return Err(GameplayError::InvalidRate(self.name));
        }
        self.scenario_step()
            .validate()
            .map_err(|error| GameplayError::InvalidStep(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedPacket {
    pub step: GameplayStep,
    pub packet: Vec<u8>,
    pub sequence: u32,
}

impl PlannedPacket {
    pub fn packet_header(&self) -> Result<game_protocol::PacketHeader, GameplayError> {
        self.packet
            .get(..game_protocol::HEADER_LEN)
            .ok_or(GameplayError::TruncatedPacket)
            .and_then(|header| {
                game_protocol::parse_header(header.try_into().expect("fixed header length"))
                    .map_err(GameplayError::Protocol)
            })
    }

    pub fn body(&self) -> Result<&[u8], GameplayError> {
        self.packet
            .get(game_protocol::HEADER_LEN..)
            .ok_or(GameplayError::TruncatedPacket)
    }

    /// A profile packet never owns a live KCP sequence. The lifecycle assigns
    /// it, then this helper retains the shared framing and protobuf body.
    pub fn with_sequence(&self, sequence: u32) -> Result<Self, GameplayError> {
        Ok(Self {
            step: self.step.clone(),
            packet: game_protocol::encode_packet(self.step.request_type, sequence, self.body()?),
            sequence,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameplayProfilePlan {
    pub profile: PlayerProfile,
    pub max_messages_per_connection_per_second: u32,
    pub steps: Vec<GameplayStep>,
    pub lockstep_inputs: Vec<PlayerInputPlan>,
}

impl GameplayProfilePlan {
    pub fn from_lockstep_scenario_json(
        profile: PlayerProfile,
        scenario_json: &str,
    ) -> Result<Self, GameplayError> {
        let scenario = Scenario::from_json_str(scenario_json)
            .map_err(|error| GameplayError::Scenario(error.to_string()))?;
        let inputs = scenario
            .to_sim_inputs()
            .map_err(|error| GameplayError::Scenario(error.to_string()))?;
        let scenario_inputs = build_player_input_plan(&inputs)
            .map_err(|error| GameplayError::Scenario(error.to_string()))?;
        let max_rate = match profile {
            PlayerProfile::Idle | PlayerProfile::Normal => {
                DEFAULT_MAX_MESSAGES_PER_CONNECTION_PER_SECOND
            }
            PlayerProfile::HighFrequency => HIGH_FREQUENCY_MAX_MESSAGES_PER_CONNECTION_PER_SECOND,
        };
        let lockstep_inputs = if profile == PlayerProfile::Idle {
            Vec::new()
        } else {
            scenario_inputs
        };
        let inputs_per_second =
            lockstep_inputs
                .iter()
                .fold(BTreeMap::new(), |mut counts, input| {
                    let second = input.frame_id.saturating_sub(1) / u32::from(scenario.tick_rate);
                    *counts.entry(second).or_insert(0_u32) += 1;
                    counts
                });
        let highest_rate = inputs_per_second.values().copied().max().unwrap_or(0);
        if highest_rate > max_rate {
            return Err(GameplayError::ProfileRateExceeded {
                profile,
                inputs: highest_rate,
                max_rate,
            });
        }
        let steps = match profile {
            PlayerProfile::Idle => vec![room_join_step(max_rate), room_leave_step(max_rate)],
            PlayerProfile::Normal => vec![
                room_join_step(max_rate),
                player_input_step(max_rate),
                move_input_step(max_rate),
                room_leave_step(max_rate),
            ],
            PlayerProfile::HighFrequency => vec![
                room_join_step(max_rate),
                player_input_step(max_rate),
                move_input_step(max_rate),
                room_leave_step(max_rate),
            ],
        };
        for step in &steps {
            step.validate()?;
        }
        Ok(Self {
            profile,
            max_messages_per_connection_per_second: max_rate,
            steps,
            lockstep_inputs,
        })
    }

    pub fn packet_plan(
        &self,
        room_id: &str,
        policy_id: &str,
    ) -> Result<Vec<PlannedPacket>, GameplayError> {
        self.packet_plan_with_input_limit(room_id, policy_id, self.lockstep_inputs.len() as u32)
    }

    pub fn packet_plan_with_input_limit(
        &self,
        room_id: &str,
        policy_id: &str,
        max_frame_inputs: u32,
    ) -> Result<Vec<PlannedPacket>, GameplayError> {
        if room_id.trim().is_empty() || policy_id.trim().is_empty() {
            return Err(GameplayError::InvalidRoomPlan);
        }
        if max_frame_inputs == 0 || self.lockstep_inputs.is_empty() {
            return Err(GameplayError::MissingFrameInput);
        }
        let mut sequence = 1_u32;
        let mut packets = Vec::new();
        packets.push(plan_packet(
            room_join_step(self.max_messages_per_connection_per_second),
            sequence,
            &RoomJoinReq {
                room_id: room_id.to_owned(),
                policy_id: policy_id.to_owned(),
            },
        ));
        sequence += 1;
        packets.push(plan_packet(
            room_ready_step(self.max_messages_per_connection_per_second),
            sequence,
            &RoomReadyReq { ready: true },
        ));
        sequence += 1;
        packets.push(plan_packet(
            room_start_step(self.max_messages_per_connection_per_second),
            sequence,
            &RoomStartReq {},
        ));
        sequence += 1;
        for input in self.lockstep_inputs.iter().take(max_frame_inputs as usize) {
            packets.push(plan_packet(
                player_input_step(self.max_messages_per_connection_per_second),
                sequence,
                &PlayerInputReq {
                    frame_id: input.frame_id,
                    action: input.action.clone(),
                    payload_json: input.payload_json.clone(),
                    client_timestamp_ms: 1,
                },
            ));
            sequence += 1;
        }
        packets.push(plan_packet(
            room_leave_step(self.max_messages_per_connection_per_second),
            sequence,
            &RoomLeaveReq {},
        ));
        Ok(packets)
    }

    /// Builds the only supported multiplayer smoke order. Inputs are bounded
    /// per player by the caller; the controlled smoke consumes exactly one
    /// generated input for each participant.
    pub fn two_player_default_match_packet_plan(
        &self,
        room_id: &str,
        policy_id: &str,
        max_frame_inputs: u32,
    ) -> Result<Vec<CoordinatedGameplayPacket>, GameplayError> {
        if room_id.trim().is_empty() || policy_id != "default_match" {
            return Err(GameplayError::InvalidRoomPlan);
        }
        if max_frame_inputs == 0 || self.lockstep_inputs.is_empty() {
            return Err(GameplayError::MissingFrameInput);
        }
        let input = self
            .lockstep_inputs
            .first()
            .expect("non-empty lockstep input was checked");
        let rate = self.max_messages_per_connection_per_second;
        let mut sequence = 1;
        let mut planned = Vec::with_capacity(9);
        let mut push = |player_index: usize, step: GameplayStep, body: Vec<u8>| {
            planned.push(CoordinatedGameplayPacket {
                player_index,
                packet: PlannedPacket {
                    packet: game_protocol::encode_packet(step.request_type, sequence, &body),
                    step,
                    sequence,
                },
            });
            sequence += 1;
        };
        push(
            0,
            room_join_step(rate),
            game_protocol::encode_body(&RoomJoinReq {
                room_id: room_id.to_owned(),
                policy_id: policy_id.to_owned(),
            }),
        );
        push(
            1,
            room_join_step(rate),
            game_protocol::encode_body(&RoomJoinReq {
                room_id: room_id.to_owned(),
                policy_id: policy_id.to_owned(),
            }),
        );
        push(
            0,
            room_ready_step(rate),
            game_protocol::encode_body(&RoomReadyReq { ready: true }),
        );
        push(
            1,
            room_ready_step(rate),
            game_protocol::encode_body(&RoomReadyReq { ready: true }),
        );
        push(
            0,
            room_start_step(rate),
            game_protocol::encode_body(&RoomStartReq {}),
        );
        for player_index in 0..=1 {
            push(
                player_index,
                player_input_step(rate),
                game_protocol::encode_body(&PlayerInputReq {
                    frame_id: input.frame_id,
                    action: input.action.clone(),
                    payload_json: input.payload_json.clone(),
                    client_timestamp_ms: 1,
                }),
            );
        }
        push(
            0,
            room_leave_step(rate),
            game_protocol::encode_body(&RoomLeaveReq {}),
        );
        push(
            1,
            room_leave_step(rate),
            game_protocol::encode_body(&RoomLeaveReq {}),
        );
        Ok(planned)
    }
}

/// Enforces the declared legal per-connection send rate in the live runner.
/// It uses supplied monotonic milliseconds and does not sleep or open a socket.
#[derive(Debug, Default)]
pub struct ConnectionRateGate {
    admitted_at_ms: VecDeque<u64>,
}

impl ConnectionRateGate {
    pub fn admit(&mut self, step: &GameplayStep, now_ms: u64) -> Result<(), GameplayError> {
        step.validate()?;
        while self
            .admitted_at_ms
            .front()
            .is_some_and(|timestamp| now_ms.saturating_sub(*timestamp) >= 1_000)
        {
            self.admitted_at_ms.pop_front();
        }
        if self.admitted_at_ms.len() >= step.max_messages_per_connection_per_second as usize {
            return Err(GameplayError::ConnectionRateExceeded {
                max_rate: step.max_messages_per_connection_per_second,
            });
        }
        self.admitted_at_ms.push_back(now_ms);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum GameplayAction {
    Move {
        frame_id: u32,
        input_type: MoveInputType,
        dir_x: f32,
        dir_y: f32,
        client_timestamp_ms: i64,
    },
    FrameInput(PlayerInputPlan),
    Battle(PlayerInputPlan),
    InventoryEquip {
        item_uid: u64,
        equip_slot: String,
    },
    InventoryUse {
        item_uid: u64,
    },
    InventoryQuery,
}

impl GameplayAction {
    pub fn plan(
        &self,
        sequence: u32,
        max_messages_per_connection_per_second: u32,
    ) -> Result<PlannedPacket, GameplayError> {
        match self {
            Self::Move {
                frame_id,
                input_type,
                dir_x,
                dir_y,
                client_timestamp_ms,
            } => {
                if *input_type == MoveInputType::Unknown
                    || (*input_type == MoveInputType::MoveDir && *dir_x == 0.0 && *dir_y == 0.0)
                {
                    return Err(GameplayError::IllegalMoveInput);
                }
                Ok(plan_packet(
                    move_input_step(max_messages_per_connection_per_second),
                    sequence,
                    &MoveInputReq {
                        frame_id: *frame_id,
                        input_type: *input_type as i32,
                        dir_x: *dir_x,
                        dir_y: *dir_y,
                        has_client_state: false,
                        client_x: 0.0,
                        client_y: 0.0,
                        client_frame_id: 0,
                        client_timestamp_ms: *client_timestamp_ms,
                    },
                ))
            }
            Self::FrameInput(input) => Ok(plan_player_input(
                player_input_step(max_messages_per_connection_per_second),
                sequence,
                input,
            )),
            Self::Battle(input) => Ok(plan_player_input(
                battle_step(max_messages_per_connection_per_second),
                sequence,
                input,
            )),
            Self::InventoryEquip {
                item_uid,
                equip_slot,
            } => {
                if *item_uid == 0 || equip_slot.trim().is_empty() {
                    return Err(GameplayError::InvalidInventoryAction);
                }
                Ok(plan_packet(
                    inventory_equip_step(max_messages_per_connection_per_second),
                    sequence,
                    &ItemEquipReq {
                        item_uid: *item_uid,
                        equip_slot: equip_slot.clone(),
                    },
                ))
            }
            Self::InventoryUse { item_uid } => {
                if *item_uid == 0 {
                    return Err(GameplayError::InvalidInventoryAction);
                }
                Ok(plan_packet(
                    inventory_use_step(max_messages_per_connection_per_second),
                    sequence,
                    &ItemUseReq {
                        item_uid: *item_uid,
                    },
                ))
            }
            Self::InventoryQuery => Ok(plan_packet(
                inventory_query_step(max_messages_per_connection_per_second),
                sequence,
                &GetInventoryReq {},
            )),
        }
    }
}

pub fn room_join_step(max_rate: u32) -> GameplayStep {
    step(
        "room_join",
        MessageType::RoomJoinReq,
        Some(MessageType::RoomJoinRes),
        Idempotency::IdempotentWrite,
        max_rate,
    )
}

pub fn room_leave_step(max_rate: u32) -> GameplayStep {
    step(
        "room_leave",
        MessageType::RoomLeaveReq,
        Some(MessageType::RoomLeaveRes),
        Idempotency::IdempotentWrite,
        max_rate,
    )
}

pub fn room_ready_step(max_rate: u32) -> GameplayStep {
    step(
        "room_ready",
        MessageType::RoomReadyReq,
        Some(MessageType::RoomReadyRes),
        Idempotency::IdempotentWrite,
        max_rate,
    )
}

pub fn room_start_step(max_rate: u32) -> GameplayStep {
    step(
        "room_start",
        MessageType::RoomStartReq,
        Some(MessageType::RoomStartRes),
        Idempotency::IdempotentWrite,
        max_rate,
    )
}

pub fn room_reconnect_step(max_rate: u32) -> GameplayStep {
    step(
        "room_reconnect",
        MessageType::RoomReconnectReq,
        Some(MessageType::RoomReconnectRes),
        Idempotency::IdempotentWrite,
        max_rate,
    )
}

pub fn player_input_step(max_rate: u32) -> GameplayStep {
    step(
        "frame_input",
        MessageType::PlayerInputReq,
        Some(MessageType::PlayerInputRes),
        Idempotency::Write,
        max_rate,
    )
}

pub fn move_input_step(max_rate: u32) -> GameplayStep {
    step(
        "move_input",
        MessageType::MoveInputReq,
        Some(MessageType::MoveInputRes),
        Idempotency::Write,
        max_rate,
    )
}

pub fn battle_step(max_rate: u32) -> GameplayStep {
    step(
        "battle_skill",
        MessageType::PlayerInputReq,
        Some(MessageType::PlayerInputRes),
        Idempotency::Write,
        max_rate,
    )
}

pub fn inventory_equip_step(max_rate: u32) -> GameplayStep {
    step(
        "inventory_equip",
        MessageType::ItemEquipReq,
        Some(MessageType::ItemEquipRes),
        Idempotency::Write,
        max_rate,
    )
}

pub fn inventory_use_step(max_rate: u32) -> GameplayStep {
    step(
        "inventory_use",
        MessageType::ItemUseReq,
        Some(MessageType::ItemUseRes),
        Idempotency::Write,
        max_rate,
    )
}

pub fn inventory_query_step(max_rate: u32) -> GameplayStep {
    step(
        "inventory_query",
        MessageType::GetInventoryReq,
        Some(MessageType::GetInventoryRes),
        Idempotency::ReadOnly,
        max_rate,
    )
}

fn step(
    name: &'static str,
    request_type: MessageType,
    response_type: Option<MessageType>,
    idempotency: Idempotency,
    max_messages_per_connection_per_second: u32,
) -> GameplayStep {
    GameplayStep {
        name,
        request_type,
        response_type,
        timeout_ms: DEFAULT_STEP_TIMEOUT_MS,
        think_time_ms: DEFAULT_THINK_TIME_MS,
        expected: ExpectedResponse::Success,
        idempotency,
        retry: RetryPolicy::Never,
        max_messages_per_connection_per_second,
    }
}

fn plan_packet<M: Message>(step: GameplayStep, sequence: u32, message: &M) -> PlannedPacket {
    PlannedPacket {
        packet: game_protocol::encode_packet(
            step.request_type,
            sequence,
            &game_protocol::encode_body(message),
        ),
        step,
        sequence,
    }
}

fn plan_player_input(step: GameplayStep, sequence: u32, input: &PlayerInputPlan) -> PlannedPacket {
    plan_packet(
        step,
        sequence,
        &PlayerInputReq {
            frame_id: input.frame_id,
            action: input.action.clone(),
            payload_json: input.payload_json.clone(),
            client_timestamp_ms: 1,
        },
    )
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoomFlowMetrics {
    pub room_create_or_join: u64,
    pub room_leave: u64,
    pub room_reconnect: u64,
    pub frame_inputs_sent: u64,
    pub frame_inputs_received: u64,
    pub frame_bundles_received: u64,
    pub frame_out_of_order: u64,
    pub frame_timeouts: u64,
    pub frame_late_response: u64,
    pub frame_local_dropped: u64,
    pub gameplay_bytes_sent: u64,
    pub gameplay_bytes_received: u64,
}

#[derive(Debug, Default)]
pub struct RoomFlowTracker {
    room_id: Option<String>,
    join_started_ms: Option<u64>,
    first_frame_started_ms: Option<u64>,
    reconnect_started_ms: Option<u64>,
    leave_started_ms: Option<u64>,
    pending_responses: BTreeMap<(u16, u32), PendingResponse>,
    sent_frame_ids: BTreeSet<u32>,
    last_frame_id: Option<u32>,
    metrics: RoomFlowMetrics,
    telemetry: Metrics,
}

impl RoomFlowTracker {
    pub fn begin_join(&mut self, sequence: u32, started_ms: u64, bytes: usize) {
        self.join_started_ms = Some(started_ms);
        self.register(
            MessageType::RoomJoinRes,
            sequence,
            started_ms,
            DEFAULT_STEP_TIMEOUT_MS,
        );
        self.metrics.room_create_or_join += 1;
        self.record_sent(bytes);
    }

    pub fn begin_leave(&mut self, sequence: u32, started_ms: u64, bytes: usize) {
        self.leave_started_ms = Some(started_ms);
        self.register(
            MessageType::RoomLeaveRes,
            sequence,
            started_ms,
            DEFAULT_STEP_TIMEOUT_MS,
        );
        self.metrics.room_leave += 1;
        self.record_sent(bytes);
    }

    pub fn begin_reconnect(&mut self, sequence: u32, started_ms: u64, bytes: usize) {
        self.reconnect_started_ms = Some(started_ms);
        self.register(
            MessageType::RoomReconnectRes,
            sequence,
            started_ms,
            DEFAULT_STEP_TIMEOUT_MS,
        );
        self.metrics.room_reconnect += 1;
        self.record_sent(bytes);
    }

    pub fn begin_frame_input(
        &mut self,
        sequence: u32,
        frame_id: u32,
        started_ms: u64,
        bytes: usize,
    ) -> Result<(), GameplayError> {
        if !self.sent_frame_ids.insert(frame_id) {
            return Err(GameplayError::DuplicateFrameInput(frame_id));
        }
        self.register(
            MessageType::PlayerInputRes,
            sequence,
            started_ms,
            DEFAULT_STEP_TIMEOUT_MS,
        );
        self.metrics.frame_inputs_sent += 1;
        self.record_sent(bytes);
        Ok(())
    }

    pub fn begin_action(
        &mut self,
        packet: &PlannedPacket,
        started_ms: u64,
    ) -> Result<(), GameplayError> {
        packet.step.validate()?;
        let response_type = packet
            .step
            .response_type
            .ok_or(GameplayError::MissingExpectedResponse(packet.step.name))?;
        self.register(
            response_type,
            packet.sequence,
            started_ms,
            packet.step.timeout_ms,
        );
        self.record_sent(packet.packet.len());
        Ok(())
    }

    pub fn begin_planned_action(
        &mut self,
        packet: &PlannedPacket,
        started_ms: u64,
    ) -> Result<(), GameplayError> {
        match packet.step.request_type {
            MessageType::RoomJoinReq => {
                self.begin_join(packet.sequence, started_ms, packet.packet.len());
                Ok(())
            }
            MessageType::RoomLeaveReq => {
                self.begin_leave(packet.sequence, started_ms, packet.packet.len());
                Ok(())
            }
            MessageType::RoomReconnectReq => {
                self.begin_reconnect(packet.sequence, started_ms, packet.packet.len());
                Ok(())
            }
            MessageType::RoomReadyReq | MessageType::RoomStartReq => {
                self.begin_action(packet, started_ms)
            }
            MessageType::PlayerInputReq => {
                let input = PlayerInputReq::decode(packet.body()?)
                    .map_err(|_| GameplayError::InvalidBody)?;
                self.begin_frame_input(
                    packet.sequence,
                    input.frame_id,
                    started_ms,
                    packet.packet.len(),
                )
            }
            _ => self.begin_action(packet, started_ms),
        }
    }

    pub fn expire_at(&mut self, now_ms: u64) {
        let expired = self
            .pending_responses
            .iter()
            .filter_map(|(key, pending)| (pending.deadline_ms <= now_ms).then_some(*key))
            .collect::<Vec<_>>();
        for key in expired {
            self.pending_responses.remove(&key);
            self.metrics.frame_timeouts += 1;
            self.telemetry.increment("frame_timeouts", 1);
        }
    }

    pub fn ingest(&mut self, packet: Packet, now_ms: u64) -> Result<(), GameplayError> {
        let message_type = packet
            .message_type()
            .ok_or(GameplayError::UnknownMessageType(packet.header.msg_type))?;
        self.record_received(packet.body.len().saturating_add(game_protocol::HEADER_LEN));
        match message_type {
            MessageType::RoomJoinRes => {
                let response = decode_response::<crate::pb::RoomJoinRes>(&packet)?;
                self.complete_response(message_type, packet.header.seq, now_ms, "room_join_ms")?;
                if !response.ok {
                    self.join_started_ms = None;
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
                self.room_id = Some(response.room_id);
                self.first_frame_started_ms = self.join_started_ms.take();
            }
            MessageType::RoomLeaveRes => {
                let response = decode_response::<crate::pb::RoomLeaveRes>(&packet)?;
                self.complete_response(message_type, packet.header.seq, now_ms, "room_exit_ms")?;
                if !response.ok {
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
                self.room_id = None;
            }
            MessageType::RoomReconnectRes => {
                let response = decode_response::<crate::pb::RoomReconnectRes>(&packet)?;
                self.complete_response(
                    message_type,
                    packet.header.seq,
                    now_ms,
                    "room_recovery_ms",
                )?;
                if !response.ok {
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
                self.room_id = Some(response.room_id);
            }
            MessageType::RoomReadyRes => {
                let response = decode_response::<crate::pb::RoomReadyRes>(&packet)?;
                self.complete_response(
                    message_type,
                    packet.header.seq,
                    now_ms,
                    "gameplay_step_ms",
                )?;
                if !response.ok || !response.ready {
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
            }
            MessageType::RoomStartRes => {
                let response = decode_response::<crate::pb::RoomStartRes>(&packet)?;
                self.complete_response(
                    message_type,
                    packet.header.seq,
                    now_ms,
                    "gameplay_step_ms",
                )?;
                if !response.ok {
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
            }
            MessageType::PlayerInputRes => {
                let response = decode_response::<crate::pb::PlayerInputRes>(&packet)?;
                self.complete_response(
                    message_type,
                    packet.header.seq,
                    now_ms,
                    "gameplay_step_ms",
                )?;
                if !response.ok {
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
            }
            MessageType::MoveInputRes => {
                let response = decode_response::<crate::pb::MoveInputRes>(&packet)?;
                self.complete_response(
                    message_type,
                    packet.header.seq,
                    now_ms,
                    "gameplay_step_ms",
                )?;
                if !response.ok {
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
            }
            MessageType::ItemEquipRes => {
                let response = decode_response::<crate::pb::ItemEquipRes>(&packet)?;
                self.complete_response(
                    message_type,
                    packet.header.seq,
                    now_ms,
                    "gameplay_step_ms",
                )?;
                if !response.ok {
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
            }
            MessageType::ItemUseRes => {
                let response = decode_response::<crate::pb::ItemUseRes>(&packet)?;
                self.complete_response(
                    message_type,
                    packet.header.seq,
                    now_ms,
                    "gameplay_step_ms",
                )?;
                if !response.ok {
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
            }
            MessageType::GetInventoryRes => {
                let response = decode_response::<crate::pb::GetInventoryRes>(&packet)?;
                self.complete_response(
                    message_type,
                    packet.header.seq,
                    now_ms,
                    "gameplay_step_ms",
                )?;
                if !response.ok {
                    self.telemetry.increment("gameplay_business_errors", 1);
                    return Err(GameplayError::BusinessRejected(message_type));
                }
            }
            MessageType::FrameBundlePush => self.ingest_frame_bundle(packet, now_ms)?,
            _ => {
                self.metrics.frame_local_dropped += 1;
                self.telemetry.increment("frame_local_dropped", 1);
            }
        }
        Ok(())
    }

    pub fn metrics(&self) -> RoomFlowMetrics {
        self.metrics.clone()
    }

    /// Returns the newest in-room frame bundle accepted by this tracker.
    pub fn latest_frame_id(&self) -> Option<u32> {
        self.last_frame_id
    }

    pub fn telemetry(&self) -> MetricsSnapshot {
        self.telemetry.snapshot()
    }

    fn ingest_frame_bundle(&mut self, packet: Packet, now_ms: u64) -> Result<(), GameplayError> {
        let bundle = decode_response::<FrameBundlePush>(&packet)?;
        if self.room_id.as_deref() != Some(bundle.room_id.as_str()) {
            self.metrics.frame_local_dropped += 1;
            self.telemetry.increment("frame_local_dropped", 1);
            return Ok(());
        }
        if self
            .last_frame_id
            .is_some_and(|last| bundle.frame_id <= last)
        {
            self.metrics.frame_out_of_order += 1;
            self.telemetry.increment("frame_out_of_order", 1);
            return Ok(());
        }
        self.last_frame_id = Some(bundle.frame_id);
        self.metrics.frame_bundles_received += 1;
        self.metrics.frame_inputs_received += bundle.inputs.len() as u64;
        self.telemetry.increment("frame_bundles_received", 1);
        self.telemetry
            .increment("frame_inputs_received", bundle.inputs.len() as u64);
        if let Some(first_frame_started_ms) = self.first_frame_started_ms.take() {
            self.telemetry.observe_latency(
                "room_first_frame_ms",
                now_ms.saturating_sub(first_frame_started_ms),
            );
        }
        Ok(())
    }

    fn register(
        &mut self,
        response_type: MessageType,
        sequence: u32,
        started_ms: u64,
        timeout_ms: u64,
    ) {
        self.pending_responses.insert(
            (response_type as u16, sequence),
            PendingResponse {
                started_ms,
                deadline_ms: started_ms.saturating_add(timeout_ms),
            },
        );
    }

    fn complete_response(
        &mut self,
        response_type: MessageType,
        sequence: u32,
        now_ms: u64,
        latency_key: &str,
    ) -> Result<(), GameplayError> {
        let Some(pending) = self
            .pending_responses
            .remove(&(response_type as u16, sequence))
        else {
            self.metrics.frame_late_response += 1;
            self.telemetry.increment("frame_late_response", 1);
            return Err(GameplayError::LateResponse {
                message_type: response_type,
                sequence,
            });
        };
        self.telemetry
            .observe_latency(latency_key, now_ms.saturating_sub(pending.started_ms));
        Ok(())
    }

    fn record_sent(&mut self, bytes: usize) {
        self.metrics.gameplay_bytes_sent += bytes as u64;
        self.telemetry.increment("gameplay_messages_sent", 1);
        self.telemetry
            .increment("gameplay_bytes_sent", bytes as u64);
    }

    fn record_received(&mut self, bytes: usize) {
        self.metrics.gameplay_bytes_received += bytes as u64;
        self.telemetry
            .increment("gameplay_bytes_received", bytes as u64);
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingResponse {
    started_ms: u64,
    deadline_ms: u64,
}

fn decode_response<M>(packet: &Packet) -> Result<M, GameplayError>
where
    M: Message + Default,
{
    packet
        .decode_body("INVALID_GAMEPLAY_BODY")
        .map_err(|_| GameplayError::InvalidBody)
}

#[derive(Debug, Error)]
pub enum GameplayError {
    #[error("gameplay step {0} has no response expectation")]
    MissingExpectedResponse(&'static str),
    #[error("gameplay step {0} has a zero per-connection rate")]
    InvalidRate(&'static str),
    #[error("gameplay step is invalid: {0}")]
    InvalidStep(String),
    #[error("lockstep scenario is invalid: {0}")]
    Scenario(String),
    #[error("profile {profile:?} emits {inputs} inputs but permits only {max_rate} per second")]
    ProfileRateExceeded {
        profile: PlayerProfile,
        inputs: u32,
        max_rate: u32,
    },
    #[error("room id and policy id are required for a room packet plan")]
    InvalidRoomPlan,
    #[error("live gameplay requires at least one lockstep frame input")]
    MissingFrameInput,
    #[error("planned packet is shorter than the shared header")]
    TruncatedPacket,
    #[error("shared protocol header rejected: {0}")]
    Protocol(&'static str),
    #[error("duplicate frame input {0}")]
    DuplicateFrameInput(u32),
    #[error("connection exceeded declared legal rate of {max_rate} messages per second")]
    ConnectionRateExceeded { max_rate: u32 },
    #[error("move input is outside the current legal profile")]
    IllegalMoveInput,
    #[error("inventory action requires a nonzero item id and a nonempty slot when equipping")]
    InvalidInventoryAction,
    #[error("unknown player message type {0}")]
    UnknownMessageType(u16),
    #[error("invalid gameplay protobuf body")]
    InvalidBody,
    #[error("gameplay response room does not match the approved room")]
    RoomMismatch,
    #[error("gameplay server rejected {0:?}")]
    BusinessRejected(MessageType),
    #[error("late response {message_type:?} sequence {sequence}")]
    LateResponse {
        message_type: MessageType,
        sequence: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::{
        FrameInput, GetInventoryReq, ItemEquipReq, ItemUseReq, MoveInputReq, MoveInputType,
        RoomReconnectReq,
    };
    use game_protocol::PacketHeader;

    const MOVE_SCENARIO: &str = include_str!("../../lockstep-client/scenarios/move_stop.json");
    const MELEE_SCENARIO: &str =
        include_str!("../../lockstep-client/scenarios/lockstep_demo_melee.json");

    fn packet<M: Message>(message_type: MessageType, sequence: u32, message: &M) -> Packet {
        let body = game_protocol::encode_body(message);
        Packet::new(
            PacketHeader {
                msg_type: message_type as u16,
                seq: sequence,
                body_len: body.len() as u32,
            },
            body,
        )
    }

    #[test]
    fn profiles_reuse_lockstep_scenario_validation_and_online_payload_generation() {
        let idle =
            GameplayProfilePlan::from_lockstep_scenario_json(PlayerProfile::Idle, MOVE_SCENARIO)
                .unwrap();
        let normal =
            GameplayProfilePlan::from_lockstep_scenario_json(PlayerProfile::Normal, MOVE_SCENARIO)
                .unwrap();
        let high = GameplayProfilePlan::from_lockstep_scenario_json(
            PlayerProfile::HighFrequency,
            MOVE_SCENARIO,
        )
        .unwrap();
        let melee =
            GameplayProfilePlan::from_lockstep_scenario_json(PlayerProfile::Normal, MELEE_SCENARIO)
                .unwrap();

        assert!(idle.lockstep_inputs.is_empty());
        assert_eq!(normal.lockstep_inputs[0].action, "sim_input");
        assert!(normal.lockstep_inputs[0].payload_json.contains("move"));
        assert_eq!(high.max_messages_per_connection_per_second, 60);
        assert!(melee.lockstep_inputs[0].payload_json.contains("castSkill"));
        assert!(
            normal
                .steps
                .iter()
                .all(|step| step.timeout_ms > 0 && step.max_messages_per_connection_per_second > 0)
        );
    }

    #[test]
    fn packet_plan_uses_shared_message_types_and_protobuf_bodies() {
        let profile =
            GameplayProfilePlan::from_lockstep_scenario_json(PlayerProfile::Normal, MOVE_SCENARIO)
                .unwrap();
        let plan = profile.packet_plan("room-a", "lockstep_sim_demo").unwrap();
        assert_eq!(plan.len(), 6);
        assert_eq!(
            plan[0].packet_header().unwrap().msg_type,
            MessageType::RoomJoinReq as u16
        );
        assert_eq!(
            plan[1].packet_header().unwrap().msg_type,
            MessageType::RoomReadyReq as u16
        );
        assert_eq!(
            plan[2].packet_header().unwrap().msg_type,
            MessageType::RoomStartReq as u16
        );
        assert_eq!(
            plan[3].packet_header().unwrap().msg_type,
            MessageType::PlayerInputReq as u16
        );
        assert_eq!(
            plan[5].packet_header().unwrap().msg_type,
            MessageType::RoomLeaveReq as u16
        );
        assert!(RoomReadyReq::decode(plan[1].body().unwrap()).unwrap().ready);
        RoomStartReq::decode(plan[2].body().unwrap()).unwrap();
        let body = &plan[3].packet[game_protocol::HEADER_LEN..];
        let input = PlayerInputReq::decode(body).unwrap();
        assert_eq!(input.action, "sim_input");
        assert!(input.payload_json.contains("move"));
    }

    #[test]
    fn bounded_live_packet_plan_reframes_only_the_shared_sequence() {
        let profile =
            GameplayProfilePlan::from_lockstep_scenario_json(PlayerProfile::Normal, MOVE_SCENARIO)
                .unwrap();
        let packets = profile
            .packet_plan_with_input_limit("approved-room", "approved-policy", 1)
            .unwrap();
        assert_eq!(packets.len(), 5);
        let reframed = packets[3].with_sequence(99).unwrap();
        assert_eq!(reframed.sequence, 99);
        assert_eq!(reframed.packet_header().unwrap().seq, 99);
        assert_eq!(reframed.step.request_type, MessageType::PlayerInputReq);
        assert_eq!(
            PlayerInputReq::decode(reframed.body().unwrap())
                .unwrap()
                .action,
            "sim_input"
        );
        assert!(
            profile
                .packet_plan_with_input_limit("approved-room", "approved-policy", 0)
                .is_err()
        );
    }

    #[test]
    fn two_player_default_match_plan_has_the_exact_global_order() {
        let profile =
            GameplayProfilePlan::from_lockstep_scenario_json(PlayerProfile::Normal, MOVE_SCENARIO)
                .unwrap();
        let plan = profile
            .two_player_default_match_packet_plan("approved-room", "default_match", 1)
            .unwrap();
        assert_eq!(plan.len(), 9);
        assert_eq!(
            plan.iter()
                .map(|packet| (packet.player_index, packet.packet.step.request_type))
                .collect::<Vec<_>>(),
            vec![
                (0, MessageType::RoomJoinReq),
                (1, MessageType::RoomJoinReq),
                (0, MessageType::RoomReadyReq),
                (1, MessageType::RoomReadyReq),
                (0, MessageType::RoomStartReq),
                (0, MessageType::PlayerInputReq),
                (1, MessageType::PlayerInputReq),
                (0, MessageType::RoomLeaveReq),
                (1, MessageType::RoomLeaveReq),
            ]
        );
        assert!(
            RoomReadyReq::decode(plan[2].packet.body().unwrap())
                .unwrap()
                .ready
        );
        RoomStartReq::decode(plan[4].packet.body().unwrap()).unwrap();
    }

    #[test]
    fn all_gameplay_steps_are_bounded_and_non_retrying_for_writes() {
        for step in [
            room_join_step(20),
            room_leave_step(20),
            room_ready_step(20),
            room_start_step(20),
            room_reconnect_step(20),
            player_input_step(20),
            move_input_step(20),
            battle_step(20),
            inventory_equip_step(20),
            inventory_use_step(20),
            inventory_query_step(20),
        ] {
            step.validate().unwrap();
            if step.idempotency == Idempotency::Write {
                assert_eq!(step.retry, RetryPolicy::Never);
            }
        }
    }

    #[test]
    fn room_flow_tracks_e2e_frames_and_failure_categories_without_identity_metrics() {
        let mut flow = RoomFlowTracker::default();
        flow.begin_join(1, 10, 20);
        flow.ingest(
            packet(
                MessageType::RoomJoinRes,
                1,
                &crate::pb::RoomJoinRes {
                    ok: true,
                    room_id: "room-a".into(),
                    error_code: String::new(),
                },
            ),
            30,
        )
        .unwrap();
        flow.begin_frame_input(2, 1, 35, 20).unwrap();
        flow.ingest(
            packet(
                MessageType::PlayerInputRes,
                2,
                &crate::pb::PlayerInputRes {
                    ok: true,
                    room_id: "room-a".into(),
                    error_code: String::new(),
                },
            ),
            40,
        )
        .unwrap();
        flow.ingest(
            packet(
                MessageType::FrameBundlePush,
                0,
                &FrameBundlePush {
                    room_id: "room-a".into(),
                    frame_id: 1,
                    fps: 20,
                    inputs: vec![FrameInput {
                        character_id: "private-character".into(),
                        action: "sim_input".into(),
                        payload_json: "{}".into(),
                        frame_id: 1,
                    }],
                    is_silent_frame: false,
                    snapshot: None,
                },
            ),
            50,
        )
        .unwrap();
        flow.begin_reconnect(3, 55, 20);
        flow.ingest(
            packet(
                MessageType::RoomReconnectRes,
                3,
                &crate::pb::RoomReconnectRes {
                    ok: true,
                    room_id: "room-a".into(),
                    error_code: String::new(),
                    snapshot: None,
                    current_frame_id: 1,
                    recent_inputs: Vec::new(),
                    waiting_frame_id: 2,
                    waiting_inputs: Vec::new(),
                    input_delay_frames: 1,
                    movement_recovery: None,
                },
            ),
            60,
        )
        .unwrap();
        flow.begin_leave(4, 65, 20);
        flow.ingest(
            packet(
                MessageType::RoomLeaveRes,
                4,
                &crate::pb::RoomLeaveRes {
                    ok: true,
                    room_id: "room-a".into(),
                    error_code: String::new(),
                },
            ),
            75,
        )
        .unwrap();

        let metrics = flow.metrics();
        assert_eq!(metrics.room_create_or_join, 1);
        assert_eq!(metrics.room_reconnect, 1);
        assert_eq!(metrics.room_leave, 1);
        assert_eq!(metrics.frame_inputs_sent, 1);
        assert_eq!(metrics.frame_inputs_received, 1);
        assert_eq!(metrics.frame_bundles_received, 1);
        assert!(
            !serde_json::to_string(&flow.telemetry())
                .unwrap()
                .contains("private-character")
        );
        let histograms = flow.telemetry().histograms;
        assert!(histograms.contains_key("room_join_ms"));
        assert_eq!(
            histograms
                .get("room_first_frame_ms")
                .expect("first frame latency is recorded from join request")
                .percentile(0.5),
            40
        );
        assert!(histograms.contains_key("room_recovery_ms"));
        assert!(histograms.contains_key("room_exit_ms"));
    }

    #[test]
    fn multi_player_frame_bundles_measure_order_timeout_late_response_and_local_drop() {
        let mut flow = RoomFlowTracker::default();
        flow.begin_join(1, 1, 1);
        flow.ingest(
            packet(
                MessageType::RoomJoinRes,
                1,
                &crate::pb::RoomJoinRes {
                    ok: true,
                    room_id: "room-a".into(),
                    error_code: String::new(),
                },
            ),
            2,
        )
        .unwrap();
        flow.begin_frame_input(2, 3, 3, 1).unwrap();
        flow.expire_at(3 + DEFAULT_STEP_TIMEOUT_MS);
        assert!(matches!(
            flow.ingest(
                packet(
                    MessageType::PlayerInputRes,
                    2,
                    &crate::pb::PlayerInputRes {
                        ok: true,
                        room_id: "room-a".into(),
                        error_code: String::new(),
                    },
                ),
                5,
            ),
            Err(GameplayError::LateResponse {
                message_type: MessageType::PlayerInputRes,
                sequence: 2,
            })
        ));
        let bundle = |frame_id| {
            packet(
                MessageType::FrameBundlePush,
                0,
                &FrameBundlePush {
                    room_id: "room-a".into(),
                    frame_id,
                    fps: 20,
                    inputs: vec![
                        FrameInput {
                            character_id: "player-a".into(),
                            action: "sim_input".into(),
                            payload_json: "{}".into(),
                            frame_id,
                        },
                        FrameInput {
                            character_id: "player-b".into(),
                            action: "sim_input".into(),
                            payload_json: "{}".into(),
                            frame_id,
                        },
                    ],
                    is_silent_frame: false,
                    snapshot: None,
                },
            )
        };
        flow.ingest(bundle(4), 6).unwrap();
        flow.ingest(bundle(3), 7).unwrap();
        flow.ingest(
            packet(
                MessageType::FrameBundlePush,
                0,
                &FrameBundlePush {
                    room_id: "different-room".into(),
                    frame_id: 5,
                    fps: 20,
                    inputs: Vec::new(),
                    is_silent_frame: true,
                    snapshot: None,
                },
            ),
            8,
        )
        .unwrap();
        let metrics = flow.metrics();
        assert_eq!(metrics.frame_timeouts, 1);
        assert_eq!(metrics.frame_late_response, 1);
        assert_eq!(metrics.frame_inputs_received, 2);
        assert_eq!(metrics.frame_out_of_order, 1);
        assert_eq!(metrics.frame_local_dropped, 1);
    }

    #[test]
    fn action_timeouts_use_the_declared_step_deadline() {
        let mut flow = RoomFlowTracker::default();
        let mut planned = GameplayAction::InventoryQuery.plan(9, 20).unwrap();
        planned.step.timeout_ms = 15;

        flow.begin_action(&planned, 100).unwrap();
        flow.expire_at(114);
        assert_eq!(flow.metrics().frame_timeouts, 0);

        flow.expire_at(115);
        assert_eq!(flow.metrics().frame_timeouts, 1);
    }

    #[test]
    fn protobuf_business_steps_use_current_request_bodies() {
        let move_request = MoveInputReq {
            frame_id: 1,
            input_type: MoveInputType::MoveDir as i32,
            dir_x: 1.0,
            dir_y: 0.0,
            has_client_state: false,
            client_x: 0.0,
            client_y: 0.0,
            client_frame_id: 0,
            client_timestamp_ms: 1,
        };
        let equip = ItemEquipReq {
            item_uid: 1,
            equip_slot: "weapon".into(),
        };
        let use_item = ItemUseReq { item_uid: 1 };
        assert!(!game_protocol::encode_body(&move_request).is_empty());
        assert!(!game_protocol::encode_body(&equip).is_empty());
        assert!(!game_protocol::encode_body(&use_item).is_empty());
        assert_eq!(game_protocol::encode_body(&GetInventoryReq {}).len(), 0);
        let reconnect = RoomReconnectReq {
            last_character_push_sequence: 0,
        };
        assert_eq!(
            RoomReconnectReq::decode(game_protocol::encode_body(&reconnect).as_slice()).unwrap(),
            reconnect
        );
    }

    #[test]
    fn gameplay_actions_generate_current_protobuf_packets_and_reject_illegal_inputs() {
        let profile =
            GameplayProfilePlan::from_lockstep_scenario_json(PlayerProfile::Normal, MOVE_SCENARIO)
                .unwrap();
        let frame_input = profile.lockstep_inputs[0].clone();
        let cases = [
            (
                GameplayAction::Move {
                    frame_id: 1,
                    input_type: MoveInputType::MoveDir,
                    dir_x: 1.0,
                    dir_y: 0.0,
                    client_timestamp_ms: 1,
                },
                MessageType::MoveInputReq,
            ),
            (
                GameplayAction::FrameInput(frame_input.clone()),
                MessageType::PlayerInputReq,
            ),
            (
                GameplayAction::Battle(frame_input),
                MessageType::PlayerInputReq,
            ),
            (
                GameplayAction::InventoryEquip {
                    item_uid: 1,
                    equip_slot: "weapon".into(),
                },
                MessageType::ItemEquipReq,
            ),
            (
                GameplayAction::InventoryUse { item_uid: 1 },
                MessageType::ItemUseReq,
            ),
            (GameplayAction::InventoryQuery, MessageType::GetInventoryReq),
        ];
        for (action, expected_type) in cases {
            let packet = action.plan(9, 20).unwrap();
            assert_eq!(
                packet.packet_header().unwrap().msg_type,
                expected_type as u16
            );
            assert_eq!(packet.step.retry, RetryPolicy::Never);
        }
        assert!(matches!(
            GameplayAction::Move {
                frame_id: 1,
                input_type: MoveInputType::MoveDir,
                dir_x: 0.0,
                dir_y: 0.0,
                client_timestamp_ms: 1,
            }
            .plan(1, 20),
            Err(GameplayError::IllegalMoveInput)
        ));
        assert!(matches!(
            GameplayAction::InventoryEquip {
                item_uid: 0,
                equip_slot: String::new(),
            }
            .plan(1, 20),
            Err(GameplayError::InvalidInventoryAction)
        ));
    }

    #[test]
    fn connection_rate_gate_honors_each_step_limit_in_a_monotonic_window() {
        let mut gate = ConnectionRateGate::default();
        let step = move_input_step(2);
        gate.admit(&step, 0).unwrap();
        gate.admit(&step, 999).unwrap();
        assert!(matches!(
            gate.admit(&step, 999),
            Err(GameplayError::ConnectionRateExceeded { max_rate: 2 })
        ));
        gate.admit(&step, 1_000).unwrap();
    }

    #[test]
    fn tracker_correlates_movement_and_inventory_business_steps() {
        let mut flow = RoomFlowTracker::default();
        let actions = [
            GameplayAction::Move {
                frame_id: 1,
                input_type: MoveInputType::MoveDir,
                dir_x: 1.0,
                dir_y: 0.0,
                client_timestamp_ms: 1,
            },
            GameplayAction::InventoryEquip {
                item_uid: 1,
                equip_slot: "weapon".into(),
            },
            GameplayAction::InventoryUse { item_uid: 1 },
            GameplayAction::InventoryQuery,
        ];
        let response_types = [
            MessageType::MoveInputRes,
            MessageType::ItemEquipRes,
            MessageType::ItemUseRes,
            MessageType::GetInventoryRes,
        ];
        for (offset, (action, response_type)) in actions.into_iter().zip(response_types).enumerate()
        {
            let sequence = (offset + 1) as u32;
            let planned = action.plan(sequence, 20).unwrap();
            flow.begin_action(&planned, 10).unwrap();
            let packet = match response_type {
                MessageType::MoveInputRes => packet(
                    response_type,
                    sequence,
                    &crate::pb::MoveInputRes {
                        ok: true,
                        room_id: "room-a".into(),
                        error_code: String::new(),
                    },
                ),
                MessageType::ItemEquipRes => packet(
                    response_type,
                    sequence,
                    &crate::pb::ItemEquipRes {
                        ok: true,
                        error_code: String::new(),
                        unequipped_item: None,
                    },
                ),
                MessageType::ItemUseRes => packet(
                    response_type,
                    sequence,
                    &crate::pb::ItemUseRes {
                        ok: true,
                        error_code: String::new(),
                        hp_change: 1,
                        new_buff_ids: Vec::new(),
                    },
                ),
                MessageType::GetInventoryRes => packet(
                    response_type,
                    sequence,
                    &crate::pb::GetInventoryRes {
                        ok: true,
                        error_code: String::new(),
                        inventory_items: Vec::new(),
                        warehouse_items: Vec::new(),
                    },
                ),
                _ => unreachable!("test response set is exhaustive"),
            };
            flow.ingest(packet, 15).unwrap();
        }
        assert!(flow.telemetry().histograms.contains_key("gameplay_step_ms"));

        let rejected = GameplayAction::InventoryUse { item_uid: 1 }
            .plan(10, 20)
            .unwrap();
        flow.begin_action(&rejected, 20).unwrap();
        assert!(matches!(
            flow.ingest(
                packet(
                    MessageType::ItemUseRes,
                    10,
                    &crate::pb::ItemUseRes {
                        ok: false,
                        error_code: "NOT_USABLE".into(),
                        hp_change: 0,
                        new_buff_ids: Vec::new(),
                    },
                ),
                21,
            ),
            Err(GameplayError::BusinessRejected(MessageType::ItemUseRes))
        ));
        assert_eq!(
            flow.telemetry().counters.get("gameplay_business_errors"),
            Some(&1)
        );
    }
}
