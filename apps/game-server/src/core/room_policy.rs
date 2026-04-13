pub const DEFAULT_ROOM_POLICY_ID: &str = "default_match";

#[derive(Debug, Clone)]
pub struct RoomRuntimePolicy {
    pub policy_id: String,
    pub max_members: usize,
    pub min_start_players: usize,
    pub silent_room_fps: u16,
    pub idle_room_fps: u16,
    pub active_room_fps: u16,
    pub busy_room_fps: u16,
    pub busy_room_player_threshold: usize,
    pub destroy_enabled: bool,
    pub destroy_when_empty: bool,
    pub empty_ttl_secs: u64,
    pub retain_state_when_empty: bool,
    pub offline_ttl_secs: u64,
}

impl RoomRuntimePolicy {
    pub fn default_match() -> Self {
        Self {
            policy_id: DEFAULT_ROOM_POLICY_ID.to_string(),
            max_members: 10,
            min_start_players: 2,
            silent_room_fps: 1,
            idle_room_fps: 2,
            active_room_fps: 10,
            busy_room_fps: 20,
            busy_room_player_threshold: 4,
            destroy_enabled: true,
            destroy_when_empty: true,
            empty_ttl_secs: 60,
            retain_state_when_empty: false,
            offline_ttl_secs: 60,
        }
    }

    pub fn persistent_world() -> Self {
        Self {
            policy_id: "persistent_world".to_string(),
            max_members: 100,
            min_start_players: 1,
            silent_room_fps: 1,
            idle_room_fps: 2,
            active_room_fps: 10,
            busy_room_fps: 20,
            busy_room_player_threshold: 20,
            destroy_enabled: false,
            destroy_when_empty: false,
            empty_ttl_secs: 0,
            retain_state_when_empty: true,
            offline_ttl_secs: 300,
        }
    }

    pub fn disposable_match() -> Self {
        Self {
            policy_id: "disposable_match".to_string(),
            max_members: 10,
            min_start_players: 2,
            silent_room_fps: 1,
            idle_room_fps: 5,
            active_room_fps: 15,
            busy_room_fps: 30,
            busy_room_player_threshold: 2,
            destroy_enabled: true,
            destroy_when_empty: true,
            empty_ttl_secs: 60,
            retain_state_when_empty: false,
            offline_ttl_secs: 60,
        }
    }

    pub fn sandbox() -> Self {
        Self {
            policy_id: "sandbox".to_string(),
            max_members: 50,
            min_start_players: 1,
            silent_room_fps: 1,
            idle_room_fps: 5,
            active_room_fps: 20,
            busy_room_fps: 30,
            busy_room_player_threshold: 10,
            destroy_enabled: true,
            destroy_when_empty: false,
            empty_ttl_secs: 300,
            retain_state_when_empty: true,
            offline_ttl_secs: 120,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoomPolicyRegistry {
    default_policy: RoomRuntimePolicy,
    policies: std::collections::HashMap<String, RoomRuntimePolicy>,
}

impl Default for RoomPolicyRegistry {
    fn default() -> Self {
        let default_policy = RoomRuntimePolicy::default_match();
        let mut policies = std::collections::HashMap::new();
        policies.insert(default_policy.policy_id.clone(), default_policy.clone());
        policies.insert("persistent_world".to_string(), RoomRuntimePolicy::persistent_world());
        policies.insert("disposable_match".to_string(), RoomRuntimePolicy::disposable_match());
        policies.insert("sandbox".to_string(), RoomRuntimePolicy::sandbox());

        Self {
            default_policy,
            policies,
        }
    }
}

impl RoomPolicyRegistry {
    pub fn resolve(&self, policy_id: &str) -> RoomRuntimePolicy {
        self.policies
            .get(policy_id)
            .cloned()
            .unwrap_or_else(|| self.default_policy.clone())
    }

    pub fn default_policy(&self) -> &RoomRuntimePolicy {
        &self.default_policy
    }
}
