use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

const MAX_PUBLIC_REWARD_ITEMS: usize = 16;

pub(crate) type HistoryStoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityHistoryCursor {
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityRewardSummary {
    pub(crate) reward_type: String,
    pub(crate) asset_id: String,
    pub(crate) quantity: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityClaimHistoryRecord {
    pub(crate) id: i64,
    pub(crate) character_id: String,
    pub(crate) activity_id: String,
    pub(crate) version: i32,
    pub(crate) activity_type: String,
    pub(crate) action_type: String,
    pub(crate) stage_id: Option<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) completed_at: Option<DateTime<Utc>>,
    pub(crate) status: String,
    pub(crate) rewards: Vec<ActivityRewardSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityClaimHistoryPage {
    pub(crate) records: Vec<ActivityClaimHistoryRecord>,
    pub(crate) has_more: bool,
    pub(crate) next_cursor: Option<ActivityHistoryCursor>,
}

pub(crate) trait ActivityClaimHistoryStore: Send + Sync {
    fn list<'a>(
        &'a self,
        character_id: &'a str,
        cursor: Option<ActivityHistoryCursor>,
        limit: u32,
    ) -> HistoryStoreFuture<'a, Result<ActivityClaimHistoryPage, String>>;
}

#[derive(Clone, Default)]
pub(crate) struct InMemoryActivityClaimHistoryStore {
    records: Arc<RwLock<Vec<ActivityClaimHistoryRecord>>>,
}

impl InMemoryActivityClaimHistoryStore {
    #[cfg(test)]
    pub(crate) fn push(&self, record: ActivityClaimHistoryRecord) {
        self.records
            .write()
            .expect("history store lock")
            .push(record);
    }
}

impl ActivityClaimHistoryStore for InMemoryActivityClaimHistoryStore {
    fn list<'a>(
        &'a self,
        character_id: &'a str,
        cursor: Option<ActivityHistoryCursor>,
        limit: u32,
    ) -> HistoryStoreFuture<'a, Result<ActivityClaimHistoryPage, String>> {
        let records = self.records.read().expect("history store lock").clone();
        let character_id = character_id.to_string();
        Box::pin(async move {
            let mut records = records
                .into_iter()
                .filter(|record| record.character_id == character_id)
                .filter(|record| {
                    cursor.as_ref().is_none_or(|cursor| {
                        record.created_at < cursor.created_at
                            || (record.created_at == cursor.created_at && record.id < cursor.id)
                    })
                })
                .collect::<Vec<_>>();
            records.sort_by(|left, right| {
                right
                    .created_at
                    .cmp(&left.created_at)
                    .then_with(|| right.id.cmp(&left.id))
            });
            let has_more = records.len() > limit as usize;
            if has_more {
                records.truncate(limit as usize);
            }
            let next_cursor =
                has_more
                    .then(|| records.last())
                    .flatten()
                    .map(|record| ActivityHistoryCursor {
                        created_at: record.created_at,
                        id: record.id,
                    });
            Ok(ActivityClaimHistoryPage {
                records,
                has_more,
                next_cursor,
            })
        })
    }
}

#[derive(Clone)]
pub(crate) struct PgActivityClaimHistoryStore {
    pool: PgPool,
}

#[derive(sqlx::FromRow)]
struct PgActivityClaimHistoryRow {
    id: i64,
    character_id: String,
    activity_id: String,
    version_no: i32,
    activity_type: String,
    action_type: String,
    stage_id: Option<String>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    status: String,
    reward_snapshot_json: serde_json::Value,
}

impl PgActivityClaimHistoryStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn map_row(row: PgActivityClaimHistoryRow) -> ActivityClaimHistoryRecord {
        let rewards = public_reward_summary(&row.reward_snapshot_json, &row.status);
        ActivityClaimHistoryRecord {
            id: row.id,
            character_id: row.character_id,
            activity_id: row.activity_id,
            version: row.version_no,
            activity_type: row.activity_type,
            action_type: row.action_type,
            stage_id: row.stage_id,
            created_at: row.created_at,
            completed_at: row.completed_at,
            status: row.status,
            rewards,
        }
    }
}

impl ActivityClaimHistoryStore for PgActivityClaimHistoryStore {
    fn list<'a>(
        &'a self,
        character_id: &'a str,
        cursor: Option<ActivityHistoryCursor>,
        limit: u32,
    ) -> HistoryStoreFuture<'a, Result<ActivityClaimHistoryPage, String>> {
        Box::pin(async move {
            let fetch_limit = i64::from(limit) + 1;
            let rows = if let Some(cursor) = cursor {
                sqlx::query_as::<_, PgActivityClaimHistoryRow>(
                    r#"SELECT c.id, c.character_id, c.activity_id, c.version_no, c.activity_type,
                        c.action_type, c.stage_id, c.created_at, c.completed_at,
                        c.status, c.reward_snapshot_json
                    FROM activity_claim_record c
                    WHERE c.character_id = $1
                      AND (c.created_at, c.id) < ($2, $3)
                    ORDER BY c.created_at DESC, c.id DESC
                    LIMIT $4"#,
                )
                .bind(character_id)
                .bind(cursor.created_at)
                .bind(cursor.id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await
            } else {
                sqlx::query_as::<_, PgActivityClaimHistoryRow>(
                    r#"SELECT c.id, c.character_id, c.activity_id, c.version_no, c.activity_type,
                        c.action_type, c.stage_id, c.created_at, c.completed_at,
                        c.status, c.reward_snapshot_json
                    FROM activity_claim_record c
                    WHERE c.character_id = $1
                    ORDER BY c.created_at DESC, c.id DESC
                    LIMIT $2"#,
                )
                .bind(character_id)
                .bind(fetch_limit)
                .fetch_all(&self.pool)
                .await
            }
            .map_err(|error| error.to_string())?;

            let mut records = rows.into_iter().map(Self::map_row).collect::<Vec<_>>();
            let has_more = records.len() > limit as usize;
            if has_more {
                records.truncate(limit as usize);
            }
            let next_cursor =
                has_more
                    .then(|| records.last())
                    .flatten()
                    .map(|record| ActivityHistoryCursor {
                        created_at: record.created_at,
                        id: record.id,
                    });
            Ok(ActivityClaimHistoryPage {
                records,
                has_more,
                next_cursor,
            })
        })
    }
}

fn public_reward_summary(snapshot: &serde_json::Value, status: &str) -> Vec<ActivityRewardSummary> {
    if status != "granted" {
        return Vec::new();
    }
    snapshot
        .as_array()
        .into_iter()
        .flat_map(|items| items.iter())
        .take(MAX_PUBLIC_REWARD_ITEMS)
        .filter_map(|item| {
            let item_id = item
                .get("item_id")
                .and_then(|value| value.as_i64())
                .filter(|value| *value > 0)?;
            let quantity = item
                .get("count")
                .and_then(|value| value.as_u64())
                .filter(|value| *value > 0)?;
            Some(ActivityRewardSummary {
                reward_type: "item".to_string(),
                asset_id: item_id.to_string(),
                quantity,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn record(character_id: &str, id: i64, seconds: i64) -> ActivityClaimHistoryRecord {
        ActivityClaimHistoryRecord {
            id,
            character_id: character_id.to_string(),
            activity_id: format!("activity-{id}"),
            version: 1,
            activity_type: "login_reward".to_string(),
            action_type: "claim".to_string(),
            stage_id: Some(format!("stage-{id}")),
            created_at: Utc.timestamp_opt(seconds, 0).single().unwrap(),
            completed_at: Some(Utc.timestamp_opt(seconds, 0).single().unwrap()),
            status: "granted".to_string(),
            rewards: Vec::new(),
        }
    }

    #[test]
    fn public_summary_only_exposes_granted_item_ids_and_counts() {
        let snapshot = serde_json::json!([
            {"item_id": 1001, "count": 2, "binding": {"character_id": "chr-secret"}},
            {"item_id": 1002, "count": 0},
            {"item_id": -1, "count": 3}
        ]);
        let rewards = public_reward_summary(&snapshot, "granted");
        assert_eq!(rewards.len(), 1);
        assert_eq!(rewards[0].asset_id, "1001");
        assert_eq!(rewards[0].quantity, 2);
    }

    #[test]
    fn non_granted_history_does_not_expose_reward_snapshot() {
        let snapshot = serde_json::json!([{"item_id": 1001, "count": 2}]);
        assert!(public_reward_summary(&snapshot, "manual_review").is_empty());
    }

    #[tokio::test]
    async fn in_memory_history_is_character_bound_and_keyset_paginated() {
        let store = InMemoryActivityClaimHistoryStore::default();
        store.push(record("character-a", 1, 100));
        store.push(record("character-a", 2, 100));
        store.push(record("character-a", 3, 99));
        store.push(record("character-b", 4, 101));

        let first = store.list("character-a", None, 2).await.unwrap();
        assert_eq!(
            first.records.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert!(first.has_more);
        let second = store
            .list("character-a", first.next_cursor, 2)
            .await
            .unwrap();
        assert_eq!(
            second.records.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![3]
        );
        assert!(!second.has_more);
        assert!(
            store
                .list("character-b", None, 2)
                .await
                .unwrap()
                .records
                .iter()
                .all(|row| row.character_id == "character-b")
        );
    }

    #[tokio::test]
    async fn distinct_claim_facts_are_not_collapsed_in_history() {
        let store = InMemoryActivityClaimHistoryStore::default();
        let mut first = record("character-a", 10, 100);
        first.activity_id = "same-activity".to_string();
        first.stage_id = Some("same-stage".to_string());
        let mut duplicate = first.clone();
        duplicate.id = 11;
        duplicate.created_at = Utc.timestamp_opt(99, 0).single().unwrap();
        store.push(first);
        store.push(duplicate);

        let page = store.list("character-a", None, 20).await.unwrap();
        assert_eq!(page.records.len(), 2);
        assert_eq!(page.records[0].activity_id, "same-activity");
        assert_eq!(page.records[1].stage_id.as_deref(), Some("same-stage"));
    }
}
