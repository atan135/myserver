//! 角色状态管理模块

pub mod player_state;

pub use player_state::{
    CharacterMatchContext, CharacterMatchStatus, SharedCharacterState, new_character_state_store,
    new_character_state_store_with_runtime_store,
};
