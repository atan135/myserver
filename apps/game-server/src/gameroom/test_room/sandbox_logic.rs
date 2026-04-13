use tracing::info;

use crate::core::room::PlayerInputRecord;
use crate::core::room_logic::RoomLogic;

#[derive(Default)]
pub struct SandboxLogic {
    pub tick_count: u64,
}

impl RoomLogic for SandboxLogic {
    fn on_room_created(&mut self, _room_id: &str) {
        info!(room_id = _room_id, "[RoomLogic/sandbox] sandbox room created");
    }

    fn on_player_join(&mut self, _player_id: &str) {
        info!(player_id = _player_id, "[RoomLogic/sandbox] player joined");
    }

    fn on_player_leave(&mut self, _player_id: &str) {
        info!(player_id = _player_id, "[RoomLogic/sandbox] player left");
    }

    fn on_player_offline(&mut self, _room_id: &str, _player_id: &str) {
        info!(room_id = _room_id, player_id = _player_id, "[RoomLogic/sandbox] player offline");
    }

    fn on_player_online(&mut self, _room_id: &str, _player_id: &str) {
        info!(room_id = _room_id, player_id = _player_id, "[RoomLogic/sandbox] player online");
    }

    fn on_game_started(&mut self, _room_id: &str) {
        info!(room_id = _room_id, "[RoomLogic/sandbox] sandbox started");
    }

    fn on_game_ended(&mut self, _room_id: &str) {
        info!(room_id = _room_id, "[RoomLogic/sandbox] sandbox ended");
    }

    fn on_tick(&mut self, _frame_id: u32, _inputs: &[PlayerInputRecord]) {
        self.tick_count += 1;
    }

    fn should_destroy(&self) -> bool {
        false
    }
}
