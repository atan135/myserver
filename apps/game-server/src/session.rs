#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Connected,
    Authenticated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedSessionIdentity {
    pub account_player_id: String,
    pub character_id: String,
    pub world_id: Option<u64>,
}

pub struct Session {
    pub id: u64,
    pub state: SessionState,
    /// Legacy alias kept for existing room, registry, and protocol paths.
    /// This is the account player id, not the in-game character id.
    pub player_id: Option<String>,
    pub account_player_id: Option<String>,
    pub character_id: Option<String>,
    pub world_id: Option<u64>,
    pub room_id: Option<String>,
}

impl Session {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            state: SessionState::Connected,
            player_id: None,
            account_player_id: None,
            character_id: None,
            world_id: None,
            room_id: None,
        }
    }

    pub fn set_authenticated_identity(
        &mut self,
        account_player_id: String,
        character_id: String,
        world_id: Option<u64>,
    ) {
        self.state = SessionState::Authenticated;
        self.player_id = Some(account_player_id.clone());
        self.account_player_id = Some(account_player_id);
        self.character_id = Some(character_id);
        self.world_id = world_id;
    }

    pub fn authenticated_identity(&self) -> Option<AuthenticatedSessionIdentity> {
        if self.state != SessionState::Authenticated {
            return None;
        }

        let account_player_id = self
            .account_player_id
            .as_ref()
            .or(self.player_id.as_ref())?
            .clone();
        let character_id = self.character_id.as_ref()?.clone();

        Some(AuthenticatedSessionIdentity {
            account_player_id,
            character_id,
            world_id: self.world_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authenticated_identity_keeps_player_id_as_account_alias() {
        let mut session = Session::new(42);

        session.set_authenticated_identity(
            "plr_0000000000001".to_string(),
            "chr_0000000000001".to_string(),
            Some(7),
        );

        assert_eq!(session.state, SessionState::Authenticated);
        assert_eq!(session.player_id.as_deref(), Some("plr_0000000000001"));
        assert_eq!(
            session.account_player_id.as_deref(),
            Some("plr_0000000000001")
        );
        assert_eq!(session.character_id.as_deref(), Some("chr_0000000000001"));
        assert_eq!(session.world_id, Some(7));

        let identity = session.authenticated_identity().unwrap();
        assert_eq!(identity.account_player_id, "plr_0000000000001");
        assert_eq!(identity.character_id, "chr_0000000000001");
        assert_eq!(identity.world_id, Some(7));
    }

    #[test]
    fn authenticated_identity_requires_character_id() {
        let mut session = Session::new(42);
        session.state = SessionState::Authenticated;
        session.player_id = Some("plr_0000000000001".to_string());
        session.account_player_id = Some("plr_0000000000001".to_string());

        assert_eq!(session.authenticated_identity(), None);
    }
}
