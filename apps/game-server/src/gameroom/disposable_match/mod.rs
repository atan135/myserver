use tracing::info;

use crate::core::room::PlayerInputRecord;
use crate::core::logic::RoomLogic;

#[derive(Default)]
pub struct DisposableMatchLogic {
    pub tick_count: u64,
}

impl RoomLogic for DisposableMatchLogic {
    fn on_room_created(&mut self, _room_id: &str) {
        info!(room_id = _room_id, "[RoomLogic/disposable_match] match room created");
    }

    fn on_player_join(&mut self, _player_id: &str) {
        info!(player_id = _player_id, "[RoomLogic/disposable_match] player joined");
    }

    fn on_player_leave(&mut self, _player_id: &str) {
        info!(player_id = _player_id, "[RoomLogic/disposable_match] player left");
    }

    fn on_player_offline(&mut self, _room_id: &str, _player_id: &str) {
        info!(room_id = _room_id, player_id = _player_id, "[RoomLogic/disposable_match] player offline");
    }

    fn on_player_online(&mut self, _room_id: &str, _player_id: &str) {
        info!(room_id = _room_id, player_id = _player_id, "[RoomLogic/disposable_match] player online");
    }

    fn on_game_started(&mut self, _room_id: &str) {
        info!(room_id = _room_id, "[RoomLogic/disposable_match] match started");
    }

    fn on_game_ended(&mut self, _room_id: &str) {
        info!(room_id = _room_id, "[RoomLogic/disposable_match] match ended");
    }

    fn on_tick(&mut self, _frame_id: u32, _inputs: &[PlayerInputRecord]) {
        self.tick_count += 1;
    }

    fn should_destroy(&self) -> bool {
        true
    }
}
