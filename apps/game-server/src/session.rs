#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Connected,
    Authenticated,
}

pub struct Session {
    pub id: u64,
    pub state: SessionState,
    pub player_id: Option<String>,
    pub room_id: Option<String>,
}

impl Session {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            state: SessionState::Connected,
            player_id: None,
            room_id: None,
        }
    }
}
