use mysql_async::{Opts, OptsBuilder, Pool, params, prelude::Queryable, Row};
use tracing::info;

use crate::config::Config;
use crate::core::inventory::player_data::PlayerData;
use crate::core::inventory::{
    AttrPanel, Buff, EquipmentSlots, ItemContainer, PlayerAttr, PlayerVisual,
};

/// MySQL Player Inventory Store
/// 负责玩家背包数据的持久化
#[derive(Clone)]
pub struct MySqlPlayerStore {
    pool: Option<Pool>,
}

impl MySqlPlayerStore {
    /// 创建一个禁用的 store（用于测试）
    pub fn new_disabled() -> Self {
        Self { pool: None }
    }

    /// 创建新的 MySqlPlayerStore
    pub async fn new(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        if !config.mysql_enabled {
            return Ok(Self { pool: None });
        }

        let opts = Opts::from_url(&config.mysql_url)?;
        let pool_opts = mysql_async::PoolOpts::default().with_constraints(
            mysql_async::PoolConstraints::new(1, config.mysql_pool_size.max(1)).unwrap(),
        );
        let builder = OptsBuilder::from_opts(opts).pool_opts(Some(pool_opts));
        let pool = Pool::new(builder);
        let mut conn = pool.get_conn().await?;

        // 创建表（如果不存在）
        conn.query_drop(
            r#"CREATE TABLE IF NOT EXISTS player_inventory (
                id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
                player_id VARCHAR(64) NOT NULL,
                level INT NOT NULL DEFAULT 1,
                hp BIGINT NOT NULL DEFAULT 0,
                inventory_data JSON NOT NULL,
                warehouse_data JSON NOT NULL,
                equipment_data JSON NOT NULL,
                attr_base_data JSON NOT NULL,
                visual_data JSON NOT NULL,
                buffs_data JSON NOT NULL,
                updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
                created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
                UNIQUE KEY uk_player_inventory_player_id (player_id),
                KEY idx_player_inventory_updated_at (updated_at)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci"#,
        )
        .await?;
        drop(conn);

        info!("MySqlPlayerStore initialized");
        Ok(Self { pool: Some(pool) })
    }

    /// 检查是否启用
    pub fn enabled(&self) -> bool {
        self.pool.is_some()
    }

    /// 关闭连接
    pub async fn close(&self) -> Result<(), mysql_async::Error> {
        if let Some(pool) = &self.pool {
            pool.clone().disconnect().await?;
        }
        Ok(())
    }

    /// 保存玩家数据到数据库
    pub async fn save(&self, player_id: &str, data: &PlayerData) -> Result<(), String> {
        let Some(pool) = &self.pool else {
            return Err("MySQL not enabled".to_string());
        };

        let inventory_json = serde_json::to_string(&data.inventory).map_err(|e| e.to_string())?;
        let warehouse_json = serde_json::to_string(&data.warehouse).map_err(|e| e.to_string())?;
        let equipment_json = serde_json::to_string(&data.equipment).map_err(|e| e.to_string())?;
        let attr_base_json = serde_json::to_string(&data.attr.base).map_err(|e| e.to_string())?;
        let visual_json = serde_json::to_string(&data.visual).map_err(|e| e.to_string())?;
        let buffs_json = serde_json::to_string(&data.buffs).map_err(|e| e.to_string())?;

        let mut conn = pool.get_conn().await.map_err(|e| e.to_string())?;

        // 使用 INSERT ... ON DUPLICATE KEY UPDATE 实现 upsert
        conn.exec_drop(
            r#"INSERT INTO player_inventory (
                player_id,
                level,
                hp,
                inventory_data,
                warehouse_data,
                equipment_data,
                attr_base_data,
                visual_data,
                buffs_data
            ) VALUES (
                :player_id,
                :level,
                :hp,
                :inventory_data,
                :warehouse_data,
                :equipment_data,
                :attr_base_data,
                :visual_data,
                :buffs_data
            ) ON DUPLICATE KEY UPDATE
                level = VALUES(level),
                hp = VALUES(hp),
                inventory_data = VALUES(inventory_data),
                warehouse_data = VALUES(warehouse_data),
                equipment_data = VALUES(equipment_data),
                attr_base_data = VALUES(attr_base_data),
                visual_data = VALUES(visual_data),
                buffs_data = VALUES(buffs_data),
                updated_at = CURRENT_TIMESTAMP(3)"#,
            params! {
                "player_id" => player_id,
                "level" => data.level,
                "hp" => data.get_hp(),
                "inventory_data" => &inventory_json,
                "warehouse_data" => &warehouse_json,
                "equipment_data" => &equipment_json,
                "attr_base_data" => &attr_base_json,
                "visual_data" => &visual_json,
                "buffs_data" => &buffs_json,
            },
        )
        .await
        .map_err(|e| e.to_string())?;

        info!(player_id = %player_id, "player inventory saved");
        Ok(())
    }

    /// 从数据库加载玩家数据
    pub async fn load(&self, player_id: &str) -> Result<Option<PlayerData>, String> {
        let Some(pool) = &self.pool else {
            return Err("MySQL not enabled".to_string());
        };

        let mut conn = pool.get_conn().await.map_err(|e| e.to_string())?;

        let row: Option<Row> = conn
            .exec_first(
                r#"SELECT
                    level,
                    hp,
                    inventory_data,
                    warehouse_data,
                    equipment_data,
                    attr_base_data,
                    visual_data,
                    buffs_data
                FROM player_inventory
                WHERE player_id = :player_id"#,
                params! {
                    "player_id" => player_id,
                },
            )
            .await
            .map_err(|e| e.to_string())?;

        match row {
            Some(row) => {
                let level: i32 = row.get("level").ok_or("missing level")?;
                let hp: i64 = row.get("hp").ok_or("missing hp")?;
                let inventory_data: String = row.get("inventory_data").ok_or("missing inventory_data")?;
                let warehouse_data: String = row.get("warehouse_data").ok_or("missing warehouse_data")?;
                let equipment_data: String = row.get("equipment_data").ok_or("missing equipment_data")?;
                let attr_base_data: String = row.get("attr_base_data").ok_or("missing attr_base_data")?;
                let visual_data: String = row.get("visual_data").ok_or("missing visual_data")?;
                let buffs_data: String = row.get("buffs_data").ok_or("missing buffs_data")?;

                let inventory: ItemContainer =
                    serde_json::from_str(&inventory_data).map_err(|e| e.to_string())?;
                let warehouse: ItemContainer =
                    serde_json::from_str(&warehouse_data).map_err(|e| e.to_string())?;
                let equipment: EquipmentSlots =
                    serde_json::from_str(&equipment_data).map_err(|e| e.to_string())?;
                let attr_base: AttrPanel =
                    serde_json::from_str(&attr_base_data).map_err(|e| e.to_string())?;
                let visual: PlayerVisual =
                    serde_json::from_str(&visual_data).map_err(|e| e.to_string())?;
                let buffs: Vec<Buff> =
                    serde_json::from_str(&buffs_data).map_err(|e| e.to_string())?;

                let mut attr = PlayerAttr::new();
                attr.set_base(attr_base);

                let mut player_data = PlayerData::with_capacity(
                    player_id.to_string(),
                    inventory.capacity(),
                    warehouse.capacity(),
                );
                player_data.inventory = inventory;
                player_data.warehouse = warehouse;
                player_data.equipment = equipment;
                player_data.level = level;
                player_data.attr = attr;
                player_data.visual = visual;
                player_data.buffs = buffs;
                player_data.set_hp(hp);

                // 清除脏标记（刚从数据库加载）
                player_data.clear_attr_dirty();
                player_data.clear_visual_dirty();
                player_data.clear_data_dirty();

                info!(player_id = %player_id, "player inventory loaded");
                Ok(Some(player_data))
            }
            None => Ok(None),
        }
    }

    /// 删除玩家数据
    pub async fn delete(&self, player_id: &str) -> Result<(), String> {
        let Some(pool) = &self.pool else {
            return Err("MySQL not enabled".to_string());
        };

        let mut conn = pool.get_conn().await.map_err(|e| e.to_string())?;

        conn.exec_drop(
            r#"DELETE FROM player_inventory WHERE player_id = :player_id"#,
            params! {
                "player_id" => player_id,
            },
        )
        .await
        .map_err(|e| e.to_string())?;

        info!(player_id = %player_id, "player inventory deleted");
        Ok(())
    }
}