use super::*;

impl RoomManager {
    pub(super) async fn get_room_entry(&self, room_id: &str) -> Option<SharedRoom> {
        self.rooms.read().await.get(room_id).cloned()
    }

    pub(super) async fn get_runtime_entry(&self, room_id: &str) -> Option<SharedRoomRuntime> {
        self.runtimes.read().await.get(room_id).cloned()
    }

    pub(super) async fn room_entries_snapshot(&self) -> Vec<(String, SharedRoom)> {
        let rooms = self.rooms.read().await;
        let mut entries = rooms
            .iter()
            .map(|(room_id, room)| (room_id.clone(), std::sync::Arc::clone(room)))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    pub(super) async fn publish_room_entry(
        &self,
        room_id: &str,
        room: Room,
    ) -> (SharedRoom, usize, bool) {
        let members = room_member_index_entries(&room);
        let (room_entry, room_count, inserted) = {
            let mut runtimes = self.runtimes.write().await;
            let mut rooms = self.rooms.write().await;
            let room_count = rooms.len();
            match rooms.entry(room_id.to_string()) {
                std::collections::hash_map::Entry::Occupied(entry) => {
                    (std::sync::Arc::clone(entry.get()), room_count, false)
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    runtimes
                        .entry(room_id.to_string())
                        .or_insert_with(|| std::sync::Arc::new(Mutex::new(RoomRuntime::default())));
                    let room_entry = std::sync::Arc::new(Mutex::new(room));
                    entry.insert(std::sync::Arc::clone(&room_entry));
                    (room_entry, room_count.saturating_add(1), true)
                }
            }
        };

        if inserted {
            replace_room_member_indexes(
                &self.character_rooms,
                &self.offline_characters,
                room_id,
                members,
            )
            .await;
        }

        (room_entry, room_count, inserted)
    }

    pub(super) async fn rebuild_room_indexes(&self, room_id: &str, room_entry: &SharedRoom) {
        sync_room_member_indexes_from_entry(
            &self.character_rooms,
            &self.offline_characters,
            room_id,
            room_entry,
        )
        .await;
    }

    pub(super) async fn remove_room_indexes(&self, room_id: &str) {
        remove_room_member_indexes(&self.character_rooms, &self.offline_characters, room_id).await;
    }

    pub(super) async fn remove_character_indexes_for_room(
        &self,
        character_id: &str,
        room_id: &str,
    ) {
        remove_character_index_for_room(
            &self.character_rooms,
            &self.offline_characters,
            character_id,
            room_id,
        )
        .await;
    }

    pub(super) async fn remove_offline_character_index(&self, character_id: &str, room_id: &str) {
        remove_offline_character_index_for_room(&self.offline_characters, character_id, room_id)
            .await;
    }

    pub(super) async fn set_character_index(
        &self,
        character_id: &str,
        room_id: &str,
        offline: bool,
    ) {
        set_character_room_index(
            &self.character_rooms,
            &self.offline_characters,
            character_id,
            room_id,
            offline,
        )
        .await;
    }

    pub(super) async fn ensure_runtime_entry(&self, room_id: &str) -> SharedRoomRuntime {
        let mut runtimes = self.runtimes.write().await;
        std::sync::Arc::clone(
            runtimes
                .entry(room_id.to_string())
                .or_insert_with(|| std::sync::Arc::new(Mutex::new(RoomRuntime::default()))),
        )
    }

    pub(super) fn spawn_cleanup_task(&self, cleanup_interval_secs: u64) {
        let rooms = std::sync::Arc::clone(&self.rooms);
        let runtimes = std::sync::Arc::clone(&self.runtimes);
        let character_rooms = std::sync::Arc::clone(&self.character_rooms);
        let offline_characters = std::sync::Arc::clone(&self.offline_characters);
        let policies = self.policies.clone();
        let match_client = std::sync::Arc::clone(&self.match_client);
        let cleanup_interval_secs = cleanup_interval_secs.max(1);

        tokio::spawn(async move {
            info!(
                cleanup_interval_secs = cleanup_interval_secs,
                "room cleanup task started"
            );

            let mut interval = tokio::time::interval(Duration::from_secs(cleanup_interval_secs));
            loop {
                interval.tick().await;

                let mut to_destroy = Vec::new();
                let mut matches_to_abort = Vec::new();
                let room_entries = {
                    let rooms_guard = rooms.read().await;
                    rooms_guard
                        .iter()
                        .map(|(room_id, room)| (room_id.clone(), std::sync::Arc::clone(room)))
                        .collect::<Vec<_>>()
                };

                {
                    for (room_id, room_entry) in room_entries {
                        let mut room = room_entry.lock().await;
                        if room.marked_for_destruction {
                            continue;
                        }
                        if room.transfer_state.status.rejects_room_mutation() {
                            continue;
                        }

                        let policy = policies.resolve(&room.policy_id);
                        let expired_characters =
                            room.collect_expired_offline_characters(policy.offline_ttl_secs);
                        if !expired_characters.is_empty() {
                            info!(
                                room_id = %room_id,
                                expired_characters = ?expired_characters,
                                ttl_secs = policy.offline_ttl_secs,
                                "removing expired offline characters from cleanup task"
                            );

                            for character_id in &expired_characters {
                                room.logic.on_character_leave(character_id);
                            }

                            room.remove_members(&expired_characters);
                            for character_id in &expired_characters {
                                remove_character_index_for_room(
                                    &character_rooms,
                                    &offline_characters,
                                    character_id,
                                    &room_id,
                                )
                                .await;
                            }

                            if !room.has_online_members() {
                                room.mark_empty();
                            } else {
                                room.clear_empty();
                            }
                        }

                        let should_cleanup_as_empty = match room.phase {
                            RoomPhase::InGame => room.members.is_empty(),
                            RoomPhase::Waiting => !room.has_online_members(),
                        };
                        if !policy.destroy_enabled
                            || !policy.destroy_when_empty
                            || !should_cleanup_as_empty
                        {
                            continue;
                        }

                        if !policy.retain_state_when_empty {
                            info!(
                                room_id = %room_id,
                                policy_id = %policy.policy_id,
                                "room marked for destruction (no retain)"
                            );
                            room.mark_for_destruction();
                            to_destroy.push(room_id.clone());
                            continue;
                        }

                        if let Some(empty_since) = room.empty_since {
                            let elapsed = empty_since.elapsed().as_secs();
                            if elapsed >= policy.empty_ttl_secs {
                                info!(
                                    room_id = %room_id,
                                    policy_id = %policy.policy_id,
                                    elapsed_secs = elapsed,
                                    "room TTL expired, marked for destruction"
                                );
                                room.mark_for_destruction();
                                to_destroy.push(room_id.clone());
                            }
                        }

                        if room.marked_for_destruction {
                            if let Some(match_id) = room.match_id.clone() {
                                matches_to_abort.push((match_id, room_id.clone()));
                            }
                        }
                    }
                }

                for room_id in to_destroy {
                    let runtime_entry = {
                        let runtimes = runtimes.read().await;
                        runtimes.get(&room_id).cloned()
                    };
                    if let Some(runtime_entry) = runtime_entry {
                        let runtime = runtime_entry.lock().await;
                        if let Some(handle) = &runtime.tick_handle {
                            handle.abort();
                        }
                    }
                    {
                        let mut runtimes = runtimes.write().await;
                        runtimes.remove(&room_id);
                    }
                    remove_room_member_indexes(&character_rooms, &offline_characters, &room_id)
                        .await;
                    let room_count = {
                        let mut rooms = rooms.write().await;
                        rooms.remove(&room_id);
                        rooms.len() as u64
                    };
                    METRICS.set_room_count(room_count);
                    info!(room_id = room_id, "room destroyed by cleanup task");
                }

                for (match_id, room_id) in matches_to_abort {
                    let mut guard = match_client.lock().await;
                    if let Some(ref mut client) = *guard {
                        if let Err(error) = client
                            .match_end(&match_id, &room_id, "offline_ttl_expired")
                            .await
                        {
                            tracing::error!(
                                match_id = %match_id,
                                room_id = %room_id,
                                error = %error,
                                "failed to notify MatchService after offline TTL expiration"
                            );
                        }
                    }
                }
            }
        });
    }

    pub async fn room_exists(&self, room_id: &str) -> bool {
        self.rooms.read().await.contains_key(room_id)
    }

    pub async fn find_room_by_offline_character(&self, character_id: &str) -> Option<String> {
        let room_id = self
            .offline_characters
            .read()
            .await
            .get(character_id)
            .cloned()?;
        let Some(room_entry) = self.get_room_entry(&room_id).await else {
            self.remove_character_indexes_for_room(character_id, &room_id)
                .await;
            return None;
        };

        let index_state = {
            let room = room_entry.lock().await;
            if room.marked_for_destruction
                || room.transfer_state.status == RoomTransferStatus::Retired
            {
                None
            } else {
                room.members.get(character_id).map(|member| member.offline)
            }
        };

        match index_state {
            Some(true) => Some(room_id),
            Some(false) => {
                self.remove_offline_character_index(character_id, &room_id)
                    .await;
                None
            }
            None => {
                self.remove_character_indexes_for_room(character_id, &room_id)
                    .await;
                None
            }
        }
    }
}
