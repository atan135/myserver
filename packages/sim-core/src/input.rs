//! Frame input model and deterministic ordering helpers.

use crate::ids::{EntityId, FrameId};
use crate::math::{Fp, QuantizedDir};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SimInputSource {
    Real,
    SynthesizedEmpty,
    SynthesizedRepeatLast,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MoveCommand {
    pub dir: QuantizedDir,
    pub speed_per_second: Option<Fp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FaceCommand {
    pub dir: QuantizedDir,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SimCommand {
    Move(MoveCommand),
    Stop,
    Face(FaceCommand),
    Noop,
}

impl SimCommand {
    pub fn is_movement_selection_command(&self) -> bool {
        matches!(self, Self::Move(_) | Self::Stop)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Deterministic command submitted for one entity on one simulation frame.
///
/// Inputs carry a source marker so callers can distinguish real player input
/// from synthesized lockstep input; ordering is resolved by frame, character,
/// sequence, and original slice index.
pub struct SimInput {
    pub frame: FrameId,
    pub character_id: String,
    pub entity_id: EntityId,
    pub seq: u32,
    pub source: SimInputSource,
    pub command: SimCommand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexedSimInput<'a> {
    pub original_index: usize,
    pub input: &'a SimInput,
}

pub fn stable_input_order_key(
    input: &SimInput,
    original_index: usize,
) -> (FrameId, &str, u32, usize) {
    (
        input.frame,
        input.character_id.as_str(),
        input.seq,
        original_index,
    )
}

pub fn ordered_inputs(inputs: &[SimInput]) -> Vec<IndexedSimInput<'_>> {
    let mut indexed = inputs
        .iter()
        .enumerate()
        .map(|(original_index, input)| IndexedSimInput {
            original_index,
            input,
        })
        .collect::<Vec<_>>();

    indexed.sort_by_key(|indexed| stable_input_order_key(indexed.input, indexed.original_index));
    indexed
}

pub fn select_latest_movement_inputs(inputs: &[SimInput]) -> Vec<IndexedSimInput<'_>> {
    let mut selected = BTreeMap::<(FrameId, &str), IndexedSimInput<'_>>::new();

    for (original_index, input) in inputs.iter().enumerate() {
        if !input.command.is_movement_selection_command() {
            continue;
        }

        let key = (input.frame, input.character_id.as_str());
        let candidate = IndexedSimInput {
            original_index,
            input,
        };
        let should_replace = match selected.get(&key) {
            Some(current) => movement_candidate_wins(candidate, *current),
            None => true,
        };

        if should_replace {
            selected.insert(key, candidate);
        }
    }

    selected.into_values().collect()
}

fn movement_candidate_wins(candidate: IndexedSimInput<'_>, current: IndexedSimInput<'_>) -> bool {
    candidate.input.seq > current.input.seq
        || (candidate.input.seq == current.input.seq
            && candidate.original_index > current.original_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(
        frame: u32,
        character_id: &str,
        entity_id: u32,
        seq: u32,
        command: SimCommand,
    ) -> SimInput {
        SimInput {
            frame: FrameId::new(frame),
            character_id: character_id.to_owned(),
            entity_id: EntityId::new(entity_id),
            seq,
            source: SimInputSource::Real,
            command,
        }
    }

    fn move_right() -> SimCommand {
        SimCommand::Move(MoveCommand {
            dir: QuantizedDir::RIGHT,
            speed_per_second: Some(Fp::from_i32(6)),
        })
    }

    #[test]
    fn ordered_inputs_sort_by_frame_character_seq_and_original_index() {
        let inputs = vec![
            input(1, "alice", 100, 2, move_right()),
            input(1, "alice", 100, 1, SimCommand::Noop),
            input(1, "bob", 200, 0, SimCommand::Stop),
            input(
                1,
                "alice",
                100,
                2,
                SimCommand::Face(FaceCommand {
                    dir: QuantizedDir::LEFT,
                }),
            ),
            input(0, "zoe", 300, 9, move_right()),
        ];

        let ordered = ordered_inputs(&inputs)
            .into_iter()
            .map(|indexed| indexed.original_index)
            .collect::<Vec<_>>();

        assert_eq!(ordered, vec![4, 1, 0, 3, 2]);
    }

    #[test]
    fn movement_selection_uses_highest_seq_then_highest_original_index() {
        let inputs = vec![
            input(10, "alice", 100, 1, move_right()),
            input(10, "alice", 100, 3, SimCommand::Stop),
            input(
                9,
                "alice",
                100,
                2,
                SimCommand::Move(MoveCommand {
                    dir: QuantizedDir::LEFT,
                    speed_per_second: None,
                }),
            ),
            input(
                10,
                "alice",
                100,
                3,
                SimCommand::Move(MoveCommand {
                    dir: QuantizedDir::UP,
                    speed_per_second: Some(Fp::from_i32(4)),
                }),
            ),
            input(
                10,
                "bob",
                200,
                1,
                SimCommand::Face(FaceCommand {
                    dir: QuantizedDir::DOWN,
                }),
            ),
            input(10, "bob", 200, 3, SimCommand::Noop),
            input(10, "bob", 200, 2, move_right()),
            input(10, "bob", 200, 2, SimCommand::Stop),
            input(
                9,
                "alice",
                100,
                2,
                SimCommand::Move(MoveCommand {
                    dir: QuantizedDir::RIGHT,
                    speed_per_second: None,
                }),
            ),
        ];

        let selected = select_latest_movement_inputs(&inputs);
        let selected_indexes = selected
            .iter()
            .map(|indexed| indexed.original_index)
            .collect::<Vec<_>>();

        assert_eq!(selected_indexes, vec![8, 3, 7]);
        assert!(
            matches!(selected[1].input.command, SimCommand::Move(command) if command.dir == QuantizedDir::UP)
        );
        assert_eq!(selected[2].input.command, SimCommand::Stop);
    }

    #[test]
    fn movement_selection_ignores_non_movement_commands() {
        let inputs = vec![
            input(1, "alice", 100, 9, SimCommand::Noop),
            input(
                1,
                "alice",
                100,
                10,
                SimCommand::Face(FaceCommand {
                    dir: QuantizedDir::RIGHT,
                }),
            ),
            input(1, "alice", 100, 1, SimCommand::Stop),
        ];

        let selected = select_latest_movement_inputs(&inputs);

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].original_index, 2);
        assert_eq!(selected[0].input.command, SimCommand::Stop);
    }

    #[test]
    fn invalid_quantized_direction_is_rejected_during_deserialization() {
        let move_command = r#"{"dir":{"x":1000,"y":1000},"speed_per_second":null}"#;
        assert!(serde_json::from_str::<MoveCommand>(move_command).is_err());

        let sim_input = r#"{
            "frame":1,
            "character_id":"alice",
            "entity_id":100,
            "seq":1,
            "source":"Real",
            "command":{"Move":{"dir":{"x":1000,"y":1000},"speed_per_second":null}}
        }"#;
        assert!(serde_json::from_str::<SimInput>(sim_input).is_err());
    }

    #[test]
    fn empty_input_slices_return_empty_results() {
        let inputs = Vec::<SimInput>::new();

        assert!(ordered_inputs(&inputs).is_empty());
        assert!(select_latest_movement_inputs(&inputs).is_empty());
    }
}
