use tracing::info;

use crate::core::logic::{RoomLogic, RoomLogicTransfer};
use crate::core::room::PlayerInputRecord;

#[derive(Default)]
pub struct DisposableMatchLogic {
    pub tick_count: u64,
}

impl RoomLogic for DisposableMatchLogic {
    fn on_room_created(&mut self, _room_id: &str) {
        info!(
            room_id = _room_id,
            "[RoomLogic/disposable_match] match room created"
        );
    }

    fn on_character_join(&mut self, _character_id: &str) {
        info!(
            character_id = _character_id,
            "[RoomLogic/disposable_match] player joined"
        );
    }

    fn on_character_leave(&mut self, _character_id: &str) {
        info!(
            character_id = _character_id,
            "[RoomLogic/disposable_match] player left"
        );
    }

    fn on_character_offline(&mut self, _room_id: &str, _character_id: &str) {
        info!(
            room_id = _room_id,
            character_id = _character_id,
            "[RoomLogic/disposable_match] player offline"
        );
    }

    fn on_character_online(&mut self, _room_id: &str, _character_id: &str) {
        info!(
            room_id = _room_id,
            character_id = _character_id,
            "[RoomLogic/disposable_match] player online"
        );
    }

    fn on_game_started(&mut self, _room_id: &str) {
        info!(
            room_id = _room_id,
            "[RoomLogic/disposable_match] match started"
        );
    }

    fn on_game_ended(&mut self, _room_id: &str) {
        info!(
            room_id = _room_id,
            "[RoomLogic/disposable_match] match ended"
        );
    }

    fn on_tick(&mut self, _frame_id: u32, _fps: u16, _inputs: &[PlayerInputRecord]) {
        self.tick_count += 1;
    }

    fn should_destroy(&self) -> bool {
        true
    }
}

impl RoomLogicTransfer for DisposableMatchLogic {}
