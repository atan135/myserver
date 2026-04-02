pub trait RoomLogic: Send {
    fn on_room_created(&mut self, _room_id: &str) {}

    fn on_player_join(&mut self, _player_id: &str) {}

    fn on_player_leave(&mut self, _player_id: &str) {}

    fn on_game_started(&mut self) {}

    fn on_game_ended(&mut self) {}

    fn on_player_input(&mut self, _player_id: &str, _action: &str, _payload_json: &str) {}
}

#[derive(Default)]
pub struct TestRoomLogic;

impl RoomLogic for TestRoomLogic {}

#[derive(Clone, Default)]
pub struct RoomLogicFactory;

impl RoomLogicFactory {
    pub fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
        Box::new(TestRoomLogic)
    }
}
