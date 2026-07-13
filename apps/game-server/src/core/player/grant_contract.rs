use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantItemIntent {
    pub item_id: i32,
    pub count: u32,
    pub binded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantResultSummary {
    pub character_id: String,
    pub source: String,
    pub items: Vec<GrantItemIntent>,
}

#[derive(Serialize)]
struct GrantFingerprintPayload<'a> {
    mail_id: &'a str,
    character_id: &'a str,
    source: &'a str,
    items: &'a [GrantItemIntent],
}

pub fn normalize_grant_items(
    items: &[GrantItemIntent],
) -> Result<Vec<GrantItemIntent>, &'static str> {
    if items.is_empty() {
        return Err("EMPTY_ITEMS");
    }

    let mut merged = BTreeMap::<(i32, bool), u32>::new();
    for item in items {
        if item.item_id <= 0 {
            return Err("INVALID_ITEM_ID");
        }
        if item.count == 0 {
            return Err("INVALID_ITEM_COUNT");
        }
        let count = merged.entry((item.item_id, item.binded)).or_default();
        *count = count.checked_add(item.count).ok_or("ITEM_COUNT_OVERFLOW")?;
    }

    Ok(merged
        .into_iter()
        .map(|((item_id, binded), count)| GrantItemIntent {
            item_id,
            count,
            binded,
        })
        .collect())
}

pub fn compute_grant_fingerprint(
    mail_id: &str,
    character_id: &str,
    source: &str,
    items: &[GrantItemIntent],
) -> Result<String, serde_json::Error> {
    let canonical = serde_json::to_vec(&GrantFingerprintPayload {
        mail_id,
        character_id,
        source,
        items,
    })?;
    let digest = Sha256::digest(canonical);
    Ok(format!("sha256:{digest:x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_sorts_and_merges_by_item_and_binding() {
        let normalized = normalize_grant_items(&[
            GrantItemIntent {
                item_id: 1002,
                count: 3,
                binded: true,
            },
            GrantItemIntent {
                item_id: 1001,
                count: 2,
                binded: true,
            },
            GrantItemIntent {
                item_id: 1001,
                count: 4,
                binded: false,
            },
            GrantItemIntent {
                item_id: 1001,
                count: 5,
                binded: false,
            },
        ])
        .unwrap();

        assert_eq!(
            normalized,
            vec![
                GrantItemIntent {
                    item_id: 1001,
                    count: 9,
                    binded: false,
                },
                GrantItemIntent {
                    item_id: 1001,
                    count: 2,
                    binded: true,
                },
                GrantItemIntent {
                    item_id: 1002,
                    count: 3,
                    binded: true,
                },
            ]
        );
    }

    #[test]
    fn normalization_rejects_invalid_values_and_overflow() {
        assert_eq!(normalize_grant_items(&[]), Err("EMPTY_ITEMS"));
        assert_eq!(
            normalize_grant_items(&[GrantItemIntent {
                item_id: 0,
                count: 1,
                binded: false,
            }]),
            Err("INVALID_ITEM_ID")
        );
        assert_eq!(
            normalize_grant_items(&[
                GrantItemIntent {
                    item_id: 1,
                    count: u32::MAX,
                    binded: false,
                },
                GrantItemIntent {
                    item_id: 1,
                    count: 1,
                    binded: false,
                },
            ]),
            Err("ITEM_COUNT_OVERFLOW")
        );
    }

    #[test]
    fn fingerprint_uses_exact_canonical_json_contract() {
        let items = vec![GrantItemIntent {
            item_id: 1001,
            count: 2,
            binded: false,
        }];
        let fingerprint =
            compute_grant_fingerprint("mail_01ABC", "chr_01ABC", "mail-claim", &items).unwrap();

        assert_eq!(
            fingerprint,
            "sha256:4951d5c6cbf4612e0cd91e8a7acd106b570441a6a014e96d7c4d6c423bb94dce"
        );
    }

    #[test]
    fn fingerprint_is_independent_of_original_item_order() {
        let left = normalize_grant_items(&[
            GrantItemIntent {
                item_id: 2,
                count: 1,
                binded: false,
            },
            GrantItemIntent {
                item_id: 1,
                count: 2,
                binded: true,
            },
        ])
        .unwrap();
        let right = normalize_grant_items(&[
            GrantItemIntent {
                item_id: 1,
                count: 2,
                binded: true,
            },
            GrantItemIntent {
                item_id: 2,
                count: 1,
                binded: false,
            },
        ])
        .unwrap();

        assert_eq!(
            compute_grant_fingerprint("m", "c", "mail-claim", &left).unwrap(),
            compute_grant_fingerprint("m", "c", "mail-claim", &right).unwrap()
        );
    }

    #[test]
    fn shared_node_fixture_has_the_same_normalization_and_fingerprint() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../tests/fixtures/mail-cross-service-v1.json"
        ))
        .unwrap();
        let contract = &fixture["mail_attachment_grant_v1"];
        let input = &contract["input"];
        let items = input["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| GrantItemIntent {
                item_id: item["itemId"].as_i64().unwrap() as i32,
                count: item["count"].as_u64().unwrap() as u32,
                binded: item["binded"].as_bool().unwrap(),
            })
            .collect::<Vec<_>>();
        let expected_items = contract["expected_normalized_items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| GrantItemIntent {
                item_id: item["itemId"].as_i64().unwrap() as i32,
                count: item["count"].as_u64().unwrap() as u32,
                binded: item["binded"].as_bool().unwrap(),
            })
            .collect::<Vec<_>>();

        let normalized = normalize_grant_items(&items).unwrap();
        let fingerprint = compute_grant_fingerprint(
            input["mail_id"].as_str().unwrap(),
            input["character_id"].as_str().unwrap(),
            input["source"].as_str().unwrap(),
            &normalized,
        )
        .unwrap();

        assert_eq!(normalized, expected_items);
        assert_eq!(
            fingerprint,
            contract["expected_fingerprint"].as_str().unwrap()
        );
    }
}
