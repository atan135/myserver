pub const DEFAULT_ROOM_POLICY_ID: &str = "default_match";

#[derive(Debug, Clone)]
pub struct RoomRuntimePolicy {
    pub policy_id: String,
    pub max_members: usize,
    pub min_start_players: usize,
    pub destroy_when_empty: bool,
    pub silent_room_fps: u16,
    pub idle_room_fps: u16,
    pub active_room_fps: u16,
    pub busy_room_fps: u16,
    pub busy_room_player_threshold: usize,
}

impl RoomRuntimePolicy {
    pub fn default_match() -> Self {
        Self {
            policy_id: DEFAULT_ROOM_POLICY_ID.to_string(),
            max_members: 10,
            min_start_players: 2,
            destroy_when_empty: true,
            silent_room_fps: 1,
            idle_room_fps: 2,
            active_room_fps: 10,
            busy_room_fps: 20,
            busy_room_player_threshold: 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoomPolicyRegistry {
    default_policy: RoomRuntimePolicy,
}

impl Default for RoomPolicyRegistry {
    fn default() -> Self {
        Self {
            default_policy: RoomRuntimePolicy::default_match(),
        }
    }
}

impl RoomPolicyRegistry {
    pub fn resolve(&self, policy_id: &str) -> RoomRuntimePolicy {
        if policy_id == self.default_policy.policy_id {
            return self.default_policy.clone();
        }

        self.default_policy.clone()
    }

    pub fn default_policy(&self) -> &RoomRuntimePolicy {
        &self.default_policy
    }
}
