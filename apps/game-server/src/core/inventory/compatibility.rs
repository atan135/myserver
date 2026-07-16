use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{AssetBinding, AssetContainer, EquipmentSlots, Item, ItemContainer, PlayerData};
use crate::csv_code::itemtable::ItemTable;

/// Read-only findings for legacy JSONB snapshots. Consumers may log, meter or export these
/// findings, but must never reject a login or rewrite the snapshot as part of a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetCompatibilityIssueCode {
    ZeroQuantity,
    StackExceedsMax,
    DuplicateUid,
    InvalidBinding,
    UnknownItemConfig,
    InvalidItemConfig,
    ContainerCapacityMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetCompatibilityIssue {
    pub code: AssetCompatibilityIssueCode,
    pub container: AssetContainer,
    pub asset_uid: Option<u64>,
    pub detail: String,
}

pub fn scan_player_assets(
    player: &PlayerData,
    item_table: &ItemTable,
) -> Vec<AssetCompatibilityIssue> {
    let mut issues = Vec::new();
    let mut seen_uids = HashSet::new();
    scan_container(
        &player.inventory,
        AssetContainer::Inventory,
        item_table,
        &mut seen_uids,
        &mut issues,
    );
    scan_container(
        &player.warehouse,
        AssetContainer::Warehouse,
        item_table,
        &mut seen_uids,
        &mut issues,
    );
    scan_equipment(&player.equipment, item_table, &mut seen_uids, &mut issues);
    issues
}

/// Scan persisted container JSON without mutating it. Malformed JSON is surfaced as a finding;
/// callers can therefore retain the legacy load behavior and decide recovery separately.
pub fn scan_jsonb_containers(
    inventory_json: &serde_json::Value,
    warehouse_json: &serde_json::Value,
    equipment_json: &serde_json::Value,
    item_table: &ItemTable,
) -> Vec<AssetCompatibilityIssue> {
    let mut issues = Vec::new();
    let mut seen_uids = HashSet::new();
    scan_json_container(
        inventory_json,
        AssetContainer::Inventory,
        item_table,
        &mut seen_uids,
        &mut issues,
    );
    scan_json_container(
        warehouse_json,
        AssetContainer::Warehouse,
        item_table,
        &mut seen_uids,
        &mut issues,
    );
    match serde_json::from_value::<EquipmentSlots>(equipment_json.clone()) {
        Ok(equipment) => scan_equipment(&equipment, item_table, &mut seen_uids, &mut issues),
        Err(error) => issues.push(AssetCompatibilityIssue {
            code: AssetCompatibilityIssueCode::InvalidItemConfig,
            container: AssetContainer::Equipment,
            asset_uid: None,
            detail: format!("equipment JSONB cannot be read: {error}"),
        }),
    }
    issues
}

fn scan_json_container(
    json: &serde_json::Value,
    container: AssetContainer,
    item_table: &ItemTable,
    seen_uids: &mut HashSet<u64>,
    issues: &mut Vec<AssetCompatibilityIssue>,
) {
    if let (Some(declared_capacity), Some(slots)) = (
        json.get("capacity").and_then(serde_json::Value::as_u64),
        json.get("slots").and_then(serde_json::Value::as_array),
    ) && declared_capacity as usize != slots.len()
    {
        issues.push(AssetCompatibilityIssue {
            code: AssetCompatibilityIssueCode::ContainerCapacityMismatch,
            container,
            asset_uid: None,
            detail: format!(
                "declared capacity {declared_capacity} differs from {} slots",
                slots.len()
            ),
        });
    }
    match serde_json::from_value::<ItemContainer>(json.clone()) {
        Ok(value) => scan_container(&value, container, item_table, seen_uids, issues),
        Err(error) => issues.push(AssetCompatibilityIssue {
            code: AssetCompatibilityIssueCode::InvalidItemConfig,
            container,
            asset_uid: None,
            detail: format!("container JSONB cannot be read: {error}"),
        }),
    }
}

fn scan_container(
    container_data: &ItemContainer,
    container: AssetContainer,
    item_table: &ItemTable,
    seen_uids: &mut HashSet<u64>,
    issues: &mut Vec<AssetCompatibilityIssue>,
) {
    if container_data.capacity() != container_data.slot_count() {
        issues.push(AssetCompatibilityIssue {
            code: AssetCompatibilityIssueCode::ContainerCapacityMismatch,
            container,
            asset_uid: None,
            detail: "runtime capacity differs from slot count".to_string(),
        });
    }
    for item in container_data.non_empty_items() {
        scan_item(item, container, item_table, seen_uids, issues);
    }
}

fn scan_equipment(
    equipment: &EquipmentSlots,
    item_table: &ItemTable,
    seen_uids: &mut HashSet<u64>,
    issues: &mut Vec<AssetCompatibilityIssue>,
) {
    for (_, item) in equipment.iter() {
        scan_item(
            item,
            AssetContainer::Equipment,
            item_table,
            seen_uids,
            issues,
        );
    }
}

fn scan_item(
    item: &Item,
    container: AssetContainer,
    item_table: &ItemTable,
    seen_uids: &mut HashSet<u64>,
    issues: &mut Vec<AssetCompatibilityIssue>,
) {
    if item.count == 0 {
        issue(
            issues,
            AssetCompatibilityIssueCode::ZeroQuantity,
            container,
            item.uid,
            "item count is zero",
        );
    }
    if item.uid == 0 || !seen_uids.insert(item.uid) {
        issue(
            issues,
            AssetCompatibilityIssueCode::DuplicateUid,
            container,
            item.uid,
            "UID is zero or repeats in another asset slot",
        );
    }
    match item_table.get(item.item_id) {
        None => issue(
            issues,
            AssetCompatibilityIssueCode::UnknownItemConfig,
            container,
            item.uid,
            "item id is absent from ItemTable",
        ),
        Some(row) => {
            if item.count > row.maxstack.max(0) as u32 {
                issue(
                    issues,
                    AssetCompatibilityIssueCode::StackExceedsMax,
                    container,
                    item.uid,
                    "item count exceeds ItemTable.MaxStack",
                );
            }
            if super::item::validate_item_table_row(row, item_table).is_err() {
                issue(
                    issues,
                    AssetCompatibilityIssueCode::InvalidItemConfig,
                    container,
                    item.uid,
                    "item references an invalid ItemTable row",
                );
            }
        }
    }
    if matches!(
        AssetBinding::from_item(item),
        AssetBinding::LegacyBoundWithoutCharacter | AssetBinding::LegacyUnboundWithCharacter { .. }
    ) {
        issue(
            issues,
            AssetCompatibilityIssueCode::InvalidBinding,
            container,
            item.uid,
            "binded and bound_character_id are inconsistent",
        );
    }
}

fn issue(
    issues: &mut Vec<AssetCompatibilityIssue>,
    code: AssetCompatibilityIssueCode,
    container: AssetContainer,
    asset_uid: u64,
    detail: impl Into<String>,
) {
    issues.push(AssetCompatibilityIssue {
        code,
        container,
        asset_uid: Some(asset_uid),
        detail: detail.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csv_code::itemtable::ItemTableRow;
    use std::collections::HashMap;

    fn item_table() -> ItemTable {
        let string_pool = HashMap::from([
            (1, "Material".to_string()),
            (2, "None".to_string()),
            (3, "Never".to_string()),
        ]);
        ItemTable {
            string_pool,
            rows: vec![ItemTableRow {
                id: 1001,
                type_: 1,
                maxstack: 3,
                equipslot: 2,
                bindtype: 3,
                useeffect: 2,
                usetarget: 2,
                ..ItemTableRow::default()
            }],
            by_id: HashMap::from([(1001, 0)]),
        }
    }

    #[test]
    fn scan_reports_legacy_anomalies_without_mutating_player_data() {
        let table = item_table();
        let mut player = PlayerData::with_capacity("chr_01".to_string(), 3, 2);
        let mut zero_quantity = Item::new(7, 1001, 0, false);
        zero_quantity.growth_elements = super::super::item::ItemElementValues::new(1, 0, 0, 0);
        player.inventory.add_item(zero_quantity).unwrap();
        player
            .inventory
            .add_item(Item::new(8, 1001, 4, false))
            .unwrap();
        let mut duplicate = Item::new(7, 1001, 1, false);
        duplicate.bound_character_id = Some("chr_01".to_string());
        player.warehouse.add_item(duplicate).unwrap();

        let issues = scan_player_assets(&player, &table);

        assert!(
            issues
                .iter()
                .any(|issue| issue.code == AssetCompatibilityIssueCode::ZeroQuantity)
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == AssetCompatibilityIssueCode::StackExceedsMax)
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == AssetCompatibilityIssueCode::DuplicateUid)
        );
        assert!(
            issues
                .iter()
                .any(|issue| issue.code == AssetCompatibilityIssueCode::InvalidBinding)
        );
        assert_eq!(player.inventory.find_item(8).unwrap().count, 4);
    }
}
