use tracing::info;

use crate::core::room::PlayerInputRecord;

pub trait RoomLogic: Send {
    fn on_room_created(&mut self, _room_id: &str) {}

    fn on_player_join(&mut self, _player_id: &str) {}

    fn on_player_leave(&mut self, _player_id: &str) {}

    /// Called when a player goes offline (disconnect). Game logic can use this
    /// to spawn an AI to take over the player's actions.
    fn on_player_offline(&mut self, _room_id: &str, _player_id: &str) {}

    /// Called when a player comes back online (reconnect).
    fn on_player_online(&mut self, _room_id: &str, _player_id: &str) {}

    fn on_game_started(&mut self, _room_id: &str) {}

    fn on_game_ended(&mut self, _room_id: &str) {}

    fn on_player_input(&mut self, _player_id: &str, _action: &str, _payload_json: &str) {}

    fn on_tick(&mut self, _frame_id: u32, _inputs: &[PlayerInputRecord]) {}

    fn should_destroy(&self) -> bool {
        false
    }
}

#[derive(Default)]
pub struct TestRoomLogic {
    pub tick_count: u64,
}

impl RoomLogic for TestRoomLogic {
    fn on_player_offline(&mut self, _room_id: &str, _player_id: &str) {
        info!(room_id = _room_id, player_id = _player_id, "[RoomLogic] player offline, AI takeover possible");
    }

    fn on_player_online(&mut self, _room_id: &str, _player_id: &str) {
        info!(room_id = _room_id, player_id = _player_id, "[RoomLogic] player online");
    }

    fn on_game_started(&mut self, _room_id: &str) {
        info!(room_id = _room_id, "[RoomLogic] game started");
    }

    fn on_game_ended(&mut self, _room_id: &str) {
        info!(room_id = _room_id, "[RoomLogic] game ended");
    }

    fn on_tick(&mut self, _frame_id: u32, _inputs: &[PlayerInputRecord]) {
        self.tick_count += 1;
    }

    fn should_destroy(&self) -> bool {
        false
    }
}

#[derive(Clone, Default)]
pub struct RoomLogicFactory;

impl RoomLogicFactory {
    pub fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
        Box::new(TestRoomLogic { tick_count: 0 })
    }
}
