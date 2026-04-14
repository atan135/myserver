use crate::core::room::PlayerInputRecord;

pub trait RoomLogic: Send {
    fn on_room_created(&mut self, _room_id: &str) {}

    fn on_player_join(&mut self, _player_id: &str) {}

    fn on_player_leave(&mut self, _player_id: &str) {}

    // Disconnection hook for AI takeover or offline state handling.
    fn on_player_offline(&mut self, _room_id: &str, _player_id: &str) {}

    fn on_player_online(&mut self, _room_id: &str, _player_id: &str) {}

    fn on_game_started(&mut self, _room_id: &str) {}

    fn on_game_ended(&mut self, _room_id: &str) {}

    fn on_player_input(&mut self, _player_id: &str, _action: &str, _payload_json: &str) {}

    fn on_tick(&mut self, _frame_id: u32, _inputs: &[PlayerInputRecord]) {}

    fn should_destroy(&self) -> bool {
        false
    }

    fn get_serialized_state(&self) -> String {
        String::new()
    }

    fn restore_from_serialized_state(&mut self, _state: &str) {}
}
