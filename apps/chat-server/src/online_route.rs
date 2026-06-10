use redis::AsyncCommands;
use tracing::{debug, warn};

pub fn online_route_key(prefix: &str, player_id: &str) -> String {
    format!("{}chat:online:{}", prefix, player_id)
}

pub async fn set_online_route(
    redis_url: &str,
    key_prefix: &str,
    player_id: &str,
    instance_id: &str,
    ttl_secs: u64,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut redis = client.get_multiplexed_async_connection().await?;
    let key = online_route_key(key_prefix, player_id);
    let _: () = redis.set_ex(&key, instance_id, ttl_secs).await?;
    debug!(player_id = %player_id, instance_id = %instance_id, ttl_secs, "chat online route set");
    Ok(())
}

pub async fn clear_online_route(
    redis_url: &str,
    key_prefix: &str,
    player_id: &str,
    instance_id: &str,
) -> Result<(), redis::RedisError> {
    let client = redis::Client::open(redis_url)?;
    let mut redis = client.get_multiplexed_async_connection().await?;
    let key = online_route_key(key_prefix, player_id);
    let current: Option<String> = redis.get(&key).await?;

    if current.as_deref() == Some(instance_id) {
        let _: () = redis.del(&key).await?;
        debug!(player_id = %player_id, instance_id = %instance_id, "chat online route cleared");
    } else {
        warn!(
            player_id = %player_id,
            instance_id = %instance_id,
            current = ?current,
            "skip clearing chat online route owned by another instance"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::online_route_key;

    #[test]
    fn online_route_key_respects_prefix() {
        assert_eq!(online_route_key("", "p1"), "chat:online:p1");
        assert_eq!(online_route_key("dev:", "p1"), "dev:chat:online:p1");
    }
}
