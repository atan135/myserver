use tracing::{debug, warn};

const SET_ROUTE_WITH_OWNER_SCRIPT: &str = r#"
redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[3])
redis.call('SET', KEYS[2], ARGV[2], 'EX', ARGV[3])
return 1
"#;

const DELETE_IF_OWNER_SCRIPT: &str = r#"
if redis.call('GET', KEYS[2]) == ARGV[1] then
    redis.call('DEL', KEYS[1], KEYS[2])
    return 1
end
return 0
"#;

pub fn online_route_key(prefix: &str, player_id: &str) -> String {
    format!("{}chat:online:{}", prefix, player_id)
}

pub fn online_route_owner_key(prefix: &str, player_id: &str) -> String {
    format!("{}chat:online-owner:{}", prefix, player_id)
}

pub async fn set_online_route(
    redis_url: &str,
    key_prefix: &str,
    player_id: &str,
    instance_id: &str,
    connection_token: &str,
    ttl_secs: u64,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut redis = client.get_multiplexed_async_connection().await?;
    let key = online_route_key(key_prefix, player_id);
    let owner_key = online_route_owner_key(key_prefix, player_id);
    let _: i32 = redis::Script::new(SET_ROUTE_WITH_OWNER_SCRIPT)
        .key(&key)
        .key(&owner_key)
        .arg(instance_id)
        .arg(connection_token)
        .arg(ttl_secs)
        .invoke_async(&mut redis)
        .await?;
    debug!(instance_id = %instance_id, ttl_secs, "chat online route set");
    Ok(())
}

pub async fn clear_online_route(
    redis_url: &str,
    key_prefix: &str,
    player_id: &str,
    instance_id: &str,
    connection_token: &str,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut redis = client.get_multiplexed_async_connection().await?;
    let key = online_route_key(key_prefix, player_id);
    let owner_key = online_route_owner_key(key_prefix, player_id);
    let deleted: i32 = redis::Script::new(DELETE_IF_OWNER_SCRIPT)
        .key(&key)
        .key(&owner_key)
        .arg(connection_token)
        .invoke_async(&mut redis)
        .await?;

    if deleted > 0 {
        debug!(instance_id = %instance_id, "chat online route cleared");
    } else {
        warn!(
            instance_id = %instance_id,
            error_category = "online_route_not_owned",
            "skip clearing chat online route owned by another instance"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{online_route_key, online_route_owner_key};

    #[test]
    fn online_route_key_respects_prefix() {
        assert_eq!(online_route_key("", "p1"), "chat:online:p1");
        assert_eq!(online_route_key("dev:", "p1"), "dev:chat:online:p1");
        assert_eq!(
            online_route_owner_key("dev:", "p1"),
            "dev:chat:online-owner:p1"
        );
    }
}
