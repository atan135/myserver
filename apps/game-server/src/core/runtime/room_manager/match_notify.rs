use super::*;

impl RoomManager {
    pub(super) async fn notify_room_created(
        &self,
        match_id: &str,
        room_id: &str,
        player_ids: &[String],
        mode: &str,
    ) {
        let mut guard = self.match_client.lock().await;
        if let Some(ref mut client) = *guard {
            match client
                .create_room_and_join(match_id, room_id, player_ids, mode)
                .await
            {
                Ok(()) => {
                    info!(
                        match_id = match_id,
                        room_id = room_id,
                        "Notified MatchService: room created"
                    );
                }
                Err(e) => {
                    tracing::error!(match_id = match_id, error = %e, "Failed to notify MatchService: room created");
                }
            }
        }
    }

    pub(super) async fn notify_player_joined(
        &self,
        match_id: &str,
        player_id: &str,
        room_id: &str,
    ) {
        let mut guard = self.match_client.lock().await;
        if let Some(ref mut client) = *guard {
            match client.player_joined(match_id, player_id, room_id).await {
                Ok(()) => {
                    info!(
                        match_id = match_id,
                        player_id = player_id,
                        room_id = room_id,
                        "Notified MatchService: player joined"
                    );
                }
                Err(e) => {
                    tracing::error!(match_id = match_id, player_id = player_id, error = %e, "Failed to notify MatchService: player joined");
                }
            }
        }
    }

    pub(super) async fn notify_player_left(
        &self,
        match_id: &str,
        player_id: &str,
        reason: &str,
    ) -> bool {
        let mut guard = self.match_client.lock().await;
        if let Some(ref mut client) = *guard {
            match client.player_left(match_id, player_id, reason).await {
                Ok(should_abort) => {
                    info!(
                        match_id = match_id,
                        player_id = player_id,
                        reason = reason,
                        should_abort = should_abort,
                        "Notified MatchService: player left"
                    );
                    return should_abort;
                }
                Err(e) => {
                    tracing::error!(match_id = match_id, player_id = player_id, error = %e, "Failed to notify MatchService: player left");
                }
            }
        }
        false
    }

    pub(super) async fn notify_match_end(&self, match_id: &str, room_id: &str, reason: &str) {
        let mut guard = self.match_client.lock().await;
        if let Some(ref mut client) = *guard {
            match client.match_end(match_id, room_id, reason).await {
                Ok(()) => {
                    info!(
                        match_id = match_id,
                        room_id = room_id,
                        reason = reason,
                        "Notified MatchService: match ended"
                    );
                }
                Err(e) => {
                    tracing::error!(match_id = match_id, room_id = room_id, error = %e, "Failed to notify MatchService: match ended");
                }
            }
        }
    }
}
