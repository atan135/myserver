use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use redis::AsyncCommands;
use tokio::sync::{Mutex, RwLock};

use crate::types::{ServiceEndpoint, ServiceInstance};

const DEFAULT_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(1);
const INSTANCE_DISCOVERY_STRATEGY: &str = "healthy_instances_sorted_v1";
const INSTANCE_PICK_STRATEGY: &str = "weighted_stable_instance_v1";
const ENDPOINT_PICK_STRATEGY: &str = "weighted_stable_endpoint_v1";
const ALL_ENDPOINTS_STRATEGY: &str = "all_healthy_endpoints_sorted_v1";

/// 服务注册中心客户端
pub struct RegistryClient {
    redis: redis::Client,
    instance_id: String,
    service_name: String,
    key_prefix: String,
    heartbeat_interval_secs: u64,
    heartbeat_ttl_secs: u64,
    discovery_cache_ttl: Duration,
    discovery_cache: Mutex<DiscoveryCache>,
}

#[derive(Clone, Debug)]
pub struct DiscoverySnapshot {
    pub service_name: String,
    pub instances: Vec<ServiceInstance>,
    pub updated_at: Option<Instant>,
    pub failed_at: Option<Instant>,
    pub error: Option<String>,
}

impl DiscoverySnapshot {
    pub fn ok(service_name: impl Into<String>, instances: Vec<ServiceInstance>) -> Self {
        Self {
            service_name: service_name.into(),
            instances,
            updated_at: Some(Instant::now()),
            failed_at: None,
            error: None,
        }
    }

    pub fn failure(
        service_name: impl Into<String>,
        instances: Vec<ServiceInstance>,
        updated_at: Option<Instant>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            service_name: service_name.into(),
            instances,
            updated_at,
            failed_at: Some(Instant::now()),
            error: Some(error.into()),
        }
    }

    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveryWatchConfig {
    pub interval: Duration,
    pub refresh_immediately: bool,
    pub retain_stale_on_error: bool,
}

impl DiscoveryWatchConfig {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            refresh_immediately: true,
            retain_stale_on_error: false,
        }
    }

    pub fn retain_stale_on_error(mut self, retain: bool) -> Self {
        self.retain_stale_on_error = retain;
        self
    }

    pub fn refresh_immediately(mut self, refresh_immediately: bool) -> Self {
        self.refresh_immediately = refresh_immediately;
        self
    }
}

impl Default for DiscoveryWatchConfig {
    fn default() -> Self {
        Self::new(Duration::from_secs(5))
    }
}

pub struct DiscoveryWatch {
    snapshot: Arc<RwLock<DiscoverySnapshot>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl DiscoveryWatch {
    pub async fn snapshot(&self) -> DiscoverySnapshot {
        self.snapshot.read().await.clone()
    }

    pub fn stop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        self.task = None;
    }

    pub async fn stop_and_wait(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for DiscoveryWatch {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl RegistryClient {
    /// 创建新的注册中心客户端
    pub async fn new(
        redis_url: &str,
        service_name: &str,
        instance_id: &str,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let redis = redis::Client::open(redis_url)?;
        // 测试连接
        let _conn = redis.get_multiplexed_async_connection().await?;

        Ok(Self {
            redis,
            instance_id: instance_id.to_string(),
            service_name: service_name.to_string(),
            key_prefix: default_key_prefix(),
            heartbeat_interval_secs: 10,
            heartbeat_ttl_secs: 30,
            discovery_cache_ttl: default_discovery_cache_ttl(),
            discovery_cache: Mutex::new(DiscoveryCache::default()),
        })
    }

    /// 设置注册中心 Redis key 前缀
    pub fn with_key_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.key_prefix = prefix.into();
        self.discovery_cache = Mutex::new(DiscoveryCache::default());
        self
    }

    /// 设置服务发现缓存 TTL。传入 0 可禁用缓存。
    pub fn with_discovery_cache_ttl(mut self, ttl: Duration) -> Self {
        self.discovery_cache_ttl = ttl;
        self.discovery_cache = Mutex::new(DiscoveryCache::default());
        self
    }

    /// 禁用服务发现缓存。
    pub fn without_discovery_cache(self) -> Self {
        self.with_discovery_cache_ttl(Duration::ZERO)
    }

    /// 设置心跳间隔（秒）
    pub fn with_heartbeat_interval(mut self, secs: u64) -> Self {
        self.heartbeat_interval_secs = secs;
        self
    }

    /// 设置心跳 TTL（秒）
    pub fn with_heartbeat_ttl(mut self, secs: u64) -> Self {
        self.heartbeat_ttl_secs = secs;
        self
    }

    /// 注册服务实例
    pub async fn register(
        &self,
        instance: &ServiceInstance,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = self.instance_key();
        let json = serde_json::to_string(&instance.clone().normalized())?;

        // 使用 HSET 存储 JSON 数据
        let _: () = redis::cmd("HSET")
            .arg(&key)
            .arg("data")
            .arg(&json)
            .query_async(&mut conn)
            .await?;

        // 创建心跳 Key
        let heartbeat_key = self.heartbeat_key();
        let _: () = conn
            .set_ex(&heartbeat_key, "1", self.heartbeat_ttl_secs)
            .await?;

        tracing::info!(
            service = %self.service_name,
            instance_id = %self.instance_id,
            "service registered"
        );

        self.clear_discovery_cache().await;

        Ok(())
    }

    /// 注销服务实例
    pub async fn deregister(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = self.instance_key();
        let heartbeat_key = self.heartbeat_key();

        conn.del::<_, ()>(&key).await?;
        conn.del::<_, ()>(&heartbeat_key).await?;

        tracing::info!(
            service = %self.service_name,
            instance_id = %self.instance_id,
            "service deregistered"
        );

        self.clear_discovery_cache().await;

        Ok(())
    }

    /// 发送心跳
    pub async fn heartbeat(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let heartbeat_key = self.heartbeat_key();

        let _: () = conn
            .set_ex(&heartbeat_key, "1", self.heartbeat_ttl_secs)
            .await?;

        Ok(())
    }

    /// 启动心跳任务
    pub fn start_heartbeat_task(&self) -> tokio::task::JoinHandle<()> {
        let heartbeat_ttl = self.heartbeat_ttl_secs;
        let heartbeat_interval = self.heartbeat_interval_secs;
        let redis = self.redis.clone();
        let instance_id = self.instance_id.clone();
        let service_name = self.service_name.clone();
        let key_prefix = self.key_prefix.clone();

        tokio::spawn(async move {
            let heartbeat_key = registry_heartbeat_key(&key_prefix, &service_name, &instance_id);
            let ttl = heartbeat_ttl;
            let interval = heartbeat_interval;

            // 立即发送一次心跳
            if let Ok(mut conn) = redis.get_multiplexed_async_connection().await {
                let _: Result<(), _> = conn.set_ex::<_, _, ()>(&heartbeat_key, "1", ttl).await;
            }

            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(interval));
            loop {
                ticker.tick().await;

                if let Ok(mut conn) = redis.get_multiplexed_async_connection().await {
                    let result: Result<(), _> =
                        conn.set_ex::<_, _, ()>(&heartbeat_key, "1", ttl).await;
                    if result.is_err() {
                        tracing::warn!("failed to send heartbeat");
                    }
                }
            }
        })
    }

    /// 发现服务实例（查询所有健康实例）
    pub async fn discover(
        &self,
        service_name: &str,
    ) -> Result<Vec<ServiceInstance>, Box<dyn std::error::Error + Send + Sync>> {
        self.discover_with_cache_expiry(service_name)
            .await
            .map(|(instances, _)| instances)
    }

    pub async fn refresh_discovery_snapshot(
        &self,
        service_name: &str,
    ) -> Result<DiscoverySnapshot, Box<dyn std::error::Error + Send + Sync>> {
        let instances = self.refresh_discovery_instances(service_name).await?;
        Ok(DiscoverySnapshot::ok(service_name, instances))
    }

    pub fn watch_discovery<F, Fut>(
        self,
        service_name: impl Into<String>,
        config: DiscoveryWatchConfig,
        on_refresh: F,
    ) -> DiscoveryWatch
    where
        F: Fn(DiscoverySnapshot) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        let service_name = service_name.into();
        let snapshot = Arc::new(RwLock::new(DiscoverySnapshot {
            service_name: service_name.clone(),
            instances: Vec::new(),
            updated_at: None,
            failed_at: None,
            error: None,
        }));
        let snapshot_for_task = Arc::clone(&snapshot);
        let on_refresh = Arc::new(on_refresh);

        let task = tokio::spawn(async move {
            if config.refresh_immediately {
                refresh_watch_once(
                    &self,
                    &service_name,
                    config.retain_stale_on_error,
                    &snapshot_for_task,
                    &on_refresh,
                )
                .await;
            }

            let interval = if config.interval.is_zero() {
                Duration::from_secs(1)
            } else {
                config.interval
            };
            let start = tokio::time::Instant::now() + interval;
            let mut ticker = tokio::time::interval_at(start, interval);
            loop {
                ticker.tick().await;
                refresh_watch_once(
                    &self,
                    &service_name,
                    config.retain_stale_on_error,
                    &snapshot_for_task,
                    &on_refresh,
                )
                .await;
            }
        });

        DiscoveryWatch {
            snapshot,
            task: Some(task),
        }
    }

    async fn discover_uncached(
        &self,
        service_name: &str,
    ) -> Result<Vec<ServiceInstance>, Box<dyn std::error::Error + Send + Sync>> {
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let pattern = registry_instance_scan_pattern(&self.key_prefix, service_name);

        // 使用 SCAN 而不是 KEYS（生产环境更安全）
        let mut cursor = 0_isize;
        let mut keys = Vec::new();

        loop {
            let (new_cursor, batch): (isize, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await?;

            keys.extend(batch);
            cursor = new_cursor;

            if cursor == 0 {
                break;
            }
        }

        let mut instances = Vec::new();

        for key in keys {
            let instance_id = key.split(':').last().unwrap_or("");
            let heartbeat_key = registry_heartbeat_key(&self.key_prefix, service_name, instance_id);

            // 检查心跳是否存在
            let exists: bool = conn.exists(&heartbeat_key).await?;
            if !exists {
                continue;
            }

            // 获取实例数据
            let data: Option<String> = conn.hget(&key, "data").await?;
            if let Some(json) = data {
                if let Ok(instance) = serde_json::from_str::<ServiceInstance>(&json) {
                    let instance = instance.normalized();
                    if !instance.healthy {
                        continue;
                    }
                    instances.push(instance);
                }
            }
        }

        instances.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(instances)
    }

    async fn refresh_discovery_instances(
        &self,
        service_name: &str,
    ) -> Result<Vec<ServiceInstance>, Box<dyn std::error::Error + Send + Sync>> {
        let instances = self.discover_uncached(service_name).await?;
        let expires_at = if self.discovery_cache_ttl.is_zero() {
            None
        } else {
            Some(Instant::now() + self.discovery_cache_ttl)
        };
        self.clear_cached_discovery_for_service(service_name).await;
        self.put_cached_discovery_until(
            DiscoveryCacheKey::instances(
                &self.key_prefix,
                service_name,
                INSTANCE_DISCOVERY_STRATEGY,
            ),
            DiscoveryCacheValue::Instances(instances.clone()),
            expires_at,
        )
        .await;
        Ok(instances)
    }

    async fn discover_with_cache_expiry(
        &self,
        service_name: &str,
    ) -> Result<(Vec<ServiceInstance>, Option<Instant>), Box<dyn std::error::Error + Send + Sync>>
    {
        if self.discovery_cache_ttl.is_zero() {
            return self
                .discover_uncached(service_name)
                .await
                .map(|instances| (instances, None));
        }

        let cache_key = DiscoveryCacheKey::instances(
            &self.key_prefix,
            service_name,
            INSTANCE_DISCOVERY_STRATEGY,
        );
        if let Some((DiscoveryCacheValue::Instances(instances), expires_at)) = self
            .discovery_cache
            .lock()
            .await
            .get_with_expiry(&cache_key, Instant::now())
        {
            return Ok((instances, Some(expires_at)));
        }

        let instances = self.discover_uncached(service_name).await?;
        let expires_at = Instant::now() + self.discovery_cache_ttl;
        self.put_cached_discovery_until(
            cache_key,
            DiscoveryCacheValue::Instances(instances.clone()),
            Some(expires_at),
        )
        .await;
        Ok((instances, Some(expires_at)))
    }

    /// 发现单个健康实例（用于 proxy 路由）
    pub async fn discover_one(
        &self,
        service_name: &str,
    ) -> Result<Option<ServiceInstance>, Box<dyn std::error::Error + Send + Sync>> {
        let cache_key =
            DiscoveryCacheKey::one_instance(&self.key_prefix, service_name, INSTANCE_PICK_STRATEGY);
        if let Some(DiscoveryCacheValue::Instance(instance)) =
            self.get_cached_discovery(&cache_key).await
        {
            return Ok(instance);
        }

        let (instances, expires_at) = match self.discover_with_cache_expiry(service_name).await {
            Ok(discovery) => discovery,
            Err(error) => {
                tracing::warn!(
                    service = %service_name,
                    endpoint = "",
                    instance_id = "",
                    source = "registry",
                    reason = "registry_error",
                    error = %error,
                    "service discovery failed"
                );
                return Err(error);
            }
        };

        if instances.is_empty() {
            self.put_cached_discovery_until(
                cache_key,
                DiscoveryCacheValue::Instance(None),
                expires_at,
            )
            .await;
            tracing::warn!(
                service = %service_name,
                endpoint = "",
                instance_id = "",
                source = "registry",
                reason = "no_healthy_instance",
                "service discovery returned no healthy instances"
            );
            return Ok(None);
        }

        let picked = pick_weighted_stable(&instances).cloned();
        tracing::info!(
            service = %service_name,
            endpoint = "",
            instance_id = picked.as_ref().map(|instance| instance.id.as_str()).unwrap_or(""),
            source = "registry",
            reason = "discovered",
            "service discovery selected instance"
        );
        self.put_cached_discovery_until(
            cache_key,
            DiscoveryCacheValue::Instance(picked.clone()),
            expires_at,
        )
        .await;
        Ok(picked)
    }

    /// 发现单个健康端点
    pub async fn discover_endpoint(
        &self,
        service_name: &str,
        endpoint_name: &str,
    ) -> Result<Option<ServiceEndpoint>, Box<dyn std::error::Error + Send + Sync>> {
        let cache_key = DiscoveryCacheKey::endpoint(
            &self.key_prefix,
            service_name,
            endpoint_name,
            ENDPOINT_PICK_STRATEGY,
        );
        if let Some(DiscoveryCacheValue::Endpoint(endpoint)) =
            self.get_cached_discovery(&cache_key).await
        {
            return Ok(endpoint);
        }

        let (instances, expires_at) = match self.discover_with_cache_expiry(service_name).await {
            Ok(discovery) => discovery,
            Err(error) => {
                tracing::warn!(
                    service = %service_name,
                    endpoint = %endpoint_name,
                    instance_id = "",
                    source = "registry",
                    reason = "registry_error",
                    error = %error,
                    "service endpoint discovery failed"
                );
                return Err(error);
            }
        };
        let selected = pick_endpoint_candidate_weighted_stable(&instances, endpoint_name);
        if let Some((instance, _)) = selected {
            tracing::info!(
                service = %service_name,
                endpoint = %endpoint_name,
                instance_id = %instance.id,
                source = "registry",
                reason = "discovered",
                "service endpoint discovery completed"
            );
        } else {
            tracing::warn!(
                service = %service_name,
                endpoint = %endpoint_name,
                instance_id = "",
                source = "registry",
                reason = "endpoint_missing",
                "service endpoint discovery completed"
            );
        }
        let endpoint = selected.map(|(_, endpoint)| endpoint.clone());
        self.put_cached_discovery_until(
            cache_key,
            DiscoveryCacheValue::Endpoint(endpoint.clone()),
            expires_at,
        )
        .await;
        Ok(endpoint)
    }

    /// 发现必需健康端点，不存在时返回错误
    pub async fn discover_required_endpoint(
        &self,
        service_name: &str,
        endpoint_name: &str,
    ) -> Result<ServiceEndpoint, Box<dyn std::error::Error + Send + Sync>> {
        self.discover_endpoint(service_name, endpoint_name)
            .await?
            .ok_or_else(|| {
                format!(
                    "service endpoint not found: service={}, endpoint={}",
                    service_name, endpoint_name
                )
                .into()
            })
    }

    /// 发现所有健康端点
    pub async fn discover_all_endpoints(
        &self,
        service_name: &str,
        endpoint_name: &str,
    ) -> Result<Vec<ServiceEndpoint>, Box<dyn std::error::Error + Send + Sync>> {
        let cache_key = DiscoveryCacheKey::all_endpoints(
            &self.key_prefix,
            service_name,
            endpoint_name,
            ALL_ENDPOINTS_STRATEGY,
        );
        if let Some(DiscoveryCacheValue::Endpoints(endpoints)) =
            self.get_cached_discovery(&cache_key).await
        {
            return Ok(endpoints);
        }

        let (instances, expires_at) = match self.discover_with_cache_expiry(service_name).await {
            Ok(discovery) => discovery,
            Err(error) => {
                tracing::warn!(
                    service = %service_name,
                    endpoint = %endpoint_name,
                    instance_id = "",
                    source = "registry",
                    reason = "registry_error",
                    error = %error,
                    "service endpoint list discovery failed"
                );
                return Err(error);
            }
        };
        let endpoints: Vec<_> = all_healthy_endpoints(&instances, endpoint_name)
            .into_iter()
            .cloned()
            .collect();
        if endpoints.is_empty() {
            tracing::warn!(
                service = %service_name,
                endpoint = %endpoint_name,
                instance_id = "",
                source = "registry",
                reason = "endpoint_missing",
                endpoint_count = endpoints.len(),
                "service endpoint list discovery completed"
            );
        } else {
            tracing::info!(
                service = %service_name,
                endpoint = %endpoint_name,
                instance_id = "",
                source = "registry",
                reason = "discovered",
                endpoint_count = endpoints.len(),
                "service endpoint list discovery completed"
            );
        }
        self.put_cached_discovery_until(
            cache_key,
            DiscoveryCacheValue::Endpoints(endpoints.clone()),
            expires_at,
        )
        .await;
        Ok(endpoints)
    }

    /// 获取当前实例的 Key
    fn instance_key(&self) -> String {
        registry_instance_key(&self.key_prefix, &self.service_name, &self.instance_id)
    }

    /// 获取心跳 Key
    fn heartbeat_key(&self) -> String {
        registry_heartbeat_key(&self.key_prefix, &self.service_name, &self.instance_id)
    }

    /// 获取服务名称
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    /// 获取实例 ID
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    async fn get_cached_discovery(&self, key: &DiscoveryCacheKey) -> Option<DiscoveryCacheValue> {
        if self.discovery_cache_ttl.is_zero() {
            return None;
        }

        self.discovery_cache.lock().await.get(key, Instant::now())
    }

    async fn put_cached_discovery_until(
        &self,
        key: DiscoveryCacheKey,
        value: DiscoveryCacheValue,
        expires_at: Option<Instant>,
    ) {
        if self.discovery_cache_ttl.is_zero() {
            return;
        }

        if let Some(expires_at) = expires_at {
            self.discovery_cache
                .lock()
                .await
                .put_until(key, value, expires_at);
        }
    }

    async fn clear_discovery_cache(&self) {
        self.discovery_cache.lock().await.clear();
    }

    async fn clear_cached_discovery_for_service(&self, service_name: &str) {
        self.discovery_cache
            .lock()
            .await
            .clear_service(&self.key_prefix, service_name);
    }
}

async fn refresh_watch_once<F, Fut>(
    client: &RegistryClient,
    service_name: &str,
    retain_stale_on_error: bool,
    snapshot: &Arc<RwLock<DiscoverySnapshot>>,
    on_refresh: &Arc<F>,
) where
    F: Fn(DiscoverySnapshot) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let next_snapshot = match client.refresh_discovery_snapshot(service_name).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            if !retain_stale_on_error {
                client.clear_cached_discovery_for_service(service_name).await;
            }
            let previous = snapshot.read().await.clone();
            let instances = if retain_stale_on_error {
                previous.instances
            } else {
                Vec::new()
            };
            DiscoverySnapshot::failure(service_name, instances, previous.updated_at, error.to_string())
        }
    };

    {
        let mut guard = snapshot.write().await;
        *guard = next_snapshot.clone();
    }
    on_refresh(next_snapshot).await;
}

fn default_key_prefix() -> String {
    std::env::var("REGISTRY_KEY_PREFIX")
        .or_else(|_| std::env::var("REDIS_KEY_PREFIX"))
        .unwrap_or_default()
}

fn default_discovery_cache_ttl() -> Duration {
    std::env::var("REGISTRY_DISCOVERY_CACHE_TTL_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_DISCOVERY_CACHE_TTL)
}

fn registry_instance_key(prefix: &str, service_name: &str, instance_id: &str) -> String {
    format!("{prefix}service:{service_name}:instances:{instance_id}")
}

fn registry_heartbeat_key(prefix: &str, service_name: &str, instance_id: &str) -> String {
    format!("{prefix}heartbeat:{service_name}:{instance_id}")
}

fn registry_instance_scan_pattern(prefix: &str, service_name: &str) -> String {
    format!("{prefix}service:{service_name}:instances:*")
}

fn pick_weighted_stable(instances: &[ServiceInstance]) -> Option<&ServiceInstance> {
    instances
        .iter()
        .filter(|instance| instance.healthy && instance.weight > 0)
        .max_by(|a, b| {
            weighted_score(a)
                .partial_cmp(&weighted_score(b))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.id.cmp(&a.id))
        })
}

#[cfg(test)]
fn pick_endpoint_weighted_stable<'a>(
    instances: &'a [ServiceInstance],
    endpoint_name: &str,
) -> Option<&'a ServiceEndpoint> {
    pick_endpoint_candidate_weighted_stable(instances, endpoint_name)
        .map(|(_, endpoint)| endpoint)
}

fn pick_endpoint_candidate_weighted_stable<'a>(
    instances: &'a [ServiceInstance],
    endpoint_name: &str,
) -> Option<(&'a ServiceInstance, &'a ServiceEndpoint)> {
    all_healthy_endpoint_candidates(instances, endpoint_name)
        .into_iter()
        .max_by(|(a_instance, _), (b_instance, _)| {
            weighted_score(a_instance)
                .partial_cmp(&weighted_score(b_instance))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b_instance.id.cmp(&a_instance.id))
        })
}

fn all_healthy_endpoints<'a>(
    instances: &'a [ServiceInstance],
    endpoint_name: &str,
) -> Vec<&'a ServiceEndpoint> {
    all_healthy_endpoint_candidates(instances, endpoint_name)
        .into_iter()
        .map(|(_, endpoint)| endpoint)
        .collect()
}

fn all_healthy_endpoint_candidates<'a>(
    instances: &'a [ServiceInstance],
    endpoint_name: &str,
) -> Vec<(&'a ServiceInstance, &'a ServiceEndpoint)> {
    let mut candidates: Vec<_> = instances
        .iter()
        .filter(|instance| instance.healthy && instance.weight > 0)
        .flat_map(|instance| {
            instance
                .endpoints
                .iter()
                .filter(move |endpoint| {
                    endpoint.name == endpoint_name && endpoint.healthy && endpoint.is_valid()
                })
                .map(move |endpoint| (instance, endpoint))
        })
        .collect();
    candidates.sort_by(|(a_instance, _), (b_instance, _)| a_instance.id.cmp(&b_instance.id));
    candidates
}

fn weighted_score(instance: &ServiceInstance) -> f64 {
    stable_hash(&instance.id) as f64 / u32::MAX as f64 * instance.weight as f64
}

fn stable_hash(value: &str) -> u32 {
    let mut hash = 2_166_136_261_u32;
    for byte in value.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

#[derive(Default)]
struct DiscoveryCache {
    entries: HashMap<DiscoveryCacheKey, DiscoveryCacheEntry>,
}

impl DiscoveryCache {
    fn get(&mut self, key: &DiscoveryCacheKey, now: Instant) -> Option<DiscoveryCacheValue> {
        self.get_with_expiry(key, now).map(|(value, _)| value)
    }

    fn get_with_expiry(
        &mut self,
        key: &DiscoveryCacheKey,
        now: Instant,
    ) -> Option<(DiscoveryCacheValue, Instant)> {
        let entry = self.entries.get(key)?;
        if entry.expires_at <= now {
            self.entries.remove(key);
            return None;
        }
        Some((entry.value.clone(), entry.expires_at))
    }

    #[cfg(test)]
    fn put(
        &mut self,
        key: DiscoveryCacheKey,
        value: DiscoveryCacheValue,
        now: Instant,
        ttl: Duration,
    ) {
        if ttl.is_zero() {
            return;
        }

        self.put_until(key, value, now + ttl);
    }

    fn put_until(
        &mut self,
        key: DiscoveryCacheKey,
        value: DiscoveryCacheValue,
        expires_at: Instant,
    ) {
        self.entries
            .insert(key, DiscoveryCacheEntry { expires_at, value });
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn clear_service(&mut self, prefix: &str, service_name: &str) {
        self.entries
            .retain(|key, _| key.prefix != prefix || key.service_name != service_name);
    }
}

struct DiscoveryCacheEntry {
    expires_at: Instant,
    value: DiscoveryCacheValue,
}

#[derive(Clone)]
enum DiscoveryCacheValue {
    Instances(Vec<ServiceInstance>),
    Instance(Option<ServiceInstance>),
    Endpoint(Option<ServiceEndpoint>),
    Endpoints(Vec<ServiceEndpoint>),
}

#[derive(Clone, Hash, Eq, PartialEq, Debug)]
struct DiscoveryCacheKey {
    prefix: String,
    service_name: String,
    endpoint_name: String,
    kind: &'static str,
    strategy: &'static str,
}

impl DiscoveryCacheKey {
    fn instances(prefix: &str, service_name: &str, strategy: &'static str) -> Self {
        Self::new(prefix, service_name, "", "instances", strategy)
    }

    fn one_instance(prefix: &str, service_name: &str, strategy: &'static str) -> Self {
        Self::new(prefix, service_name, "", "one_instance", strategy)
    }

    fn endpoint(
        prefix: &str,
        service_name: &str,
        endpoint_name: &str,
        strategy: &'static str,
    ) -> Self {
        Self::new(prefix, service_name, endpoint_name, "endpoint", strategy)
    }

    fn all_endpoints(
        prefix: &str,
        service_name: &str,
        endpoint_name: &str,
        strategy: &'static str,
    ) -> Self {
        Self::new(
            prefix,
            service_name,
            endpoint_name,
            "all_endpoints",
            strategy,
        )
    }

    fn new(
        prefix: &str,
        service_name: &str,
        endpoint_name: &str,
        kind: &'static str,
        strategy: &'static str,
    ) -> Self {
        Self {
            prefix: prefix.to_string(),
            service_name: service_name.to_string(),
            endpoint_name: endpoint_name.to_string(),
            kind,
            strategy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_instance_creation() {
        let instance = ServiceInstance::new(
            "test-001".to_string(),
            "game-server".to_string(),
            "127.0.0.1".to_string(),
            7000,
        )
        .with_admin_port(7001)
        .with_local_socket("test.sock".to_string());

        assert_eq!(instance.id, "test-001");
        assert_eq!(instance.port, 7000);
        assert_eq!(instance.admin_port, 7001);
        assert_eq!(instance.local_socket, "test.sock");
    }

    #[test]
    fn test_weighted_pick_ignores_unhealthy_instances() {
        let unhealthy = ServiceInstance::new(
            "unhealthy".to_string(),
            "game-server".to_string(),
            "127.0.0.1".to_string(),
            7000,
        )
        .with_weight(1000);
        let mut unhealthy = unhealthy;
        unhealthy.healthy = false;

        let healthy = ServiceInstance::new(
            "healthy".to_string(),
            "game-server".to_string(),
            "127.0.0.1".to_string(),
            7001,
        );

        let instances = vec![unhealthy, healthy.clone()];
        let picked = pick_weighted_stable(&instances).expect("healthy instance");
        assert_eq!(picked.id, healthy.id);
    }

    #[test]
    fn test_endpoint_pick_ignores_unhealthy_endpoints() {
        let mut instance = ServiceInstance::new(
            "game-001".to_string(),
            "game-server".to_string(),
            "127.0.0.1".to_string(),
            7000,
        );
        instance.endpoints[0].healthy = false;

        assert!(pick_endpoint_weighted_stable(&[instance], "client").is_none());
    }

    #[test]
    fn registry_keys_include_configured_prefix() {
        assert_eq!(
            registry_instance_key("test:", "game-server", "game-a"),
            "test:service:game-server:instances:game-a"
        );
        assert_eq!(
            registry_heartbeat_key("test:", "game-server", "game-a"),
            "test:heartbeat:game-server:game-a"
        );
        assert_eq!(
            registry_instance_scan_pattern("test:", "game-server"),
            "test:service:game-server:instances:*"
        );
    }

    #[test]
    fn discovery_cache_returns_value_until_ttl_expires() {
        let mut cache = DiscoveryCache::default();
        let key =
            DiscoveryCacheKey::endpoint("test:", "game-server", "admin", ENDPOINT_PICK_STRATEGY);
        let now = Instant::now();
        let endpoint = ServiceEndpoint::tcp("admin", "127.0.0.1", 7500, "admin");

        cache.put(
            key.clone(),
            DiscoveryCacheValue::Endpoint(Some(endpoint.clone())),
            now,
            Duration::from_millis(50),
        );

        match cache.get(&key, now + Duration::from_millis(49)) {
            Some(DiscoveryCacheValue::Endpoint(Some(cached))) => assert_eq!(cached, endpoint),
            _ => panic!("expected cached endpoint before ttl expiry"),
        }
        assert!(cache.get(&key, now + Duration::from_millis(50)).is_none());
    }

    #[test]
    fn discovery_cache_key_separates_services_endpoints_and_strategies() {
        let mut cache = DiscoveryCache::default();
        let now = Instant::now();
        let endpoint = ServiceEndpoint::tcp("admin", "127.0.0.1", 7500, "admin");
        let game_admin =
            DiscoveryCacheKey::endpoint("test:", "game-server", "admin", ENDPOINT_PICK_STRATEGY);

        cache.put(
            game_admin.clone(),
            DiscoveryCacheValue::Endpoint(Some(endpoint)),
            now,
            Duration::from_secs(1),
        );

        let chat_admin =
            DiscoveryCacheKey::endpoint("test:", "chat-server", "admin", ENDPOINT_PICK_STRATEGY);
        let game_client =
            DiscoveryCacheKey::endpoint("test:", "game-server", "client", ENDPOINT_PICK_STRATEGY);
        let game_admin_all_strategy = DiscoveryCacheKey::all_endpoints(
            "test:",
            "game-server",
            "admin",
            ALL_ENDPOINTS_STRATEGY,
        );

        assert!(cache.get(&chat_admin, now).is_none());
        assert!(cache.get(&game_client, now).is_none());
        assert!(cache.get(&game_admin_all_strategy, now).is_none());
        assert!(matches!(
            cache.get(&game_admin, now),
            Some(DiscoveryCacheValue::Endpoint(Some(_)))
        ));
    }

    #[test]
    fn discovery_cache_can_store_required_discovery_miss() {
        let mut cache = DiscoveryCache::default();
        let key = DiscoveryCacheKey::endpoint("", "game-server", "admin", ENDPOINT_PICK_STRATEGY);
        let now = Instant::now();

        cache.put(
            key.clone(),
            DiscoveryCacheValue::Endpoint(None),
            now,
            Duration::from_secs(1),
        );

        assert!(matches!(
            cache.get(&key, now),
            Some(DiscoveryCacheValue::Endpoint(None))
        ));
    }

    #[test]
    fn discovery_cache_clear_service_keeps_other_services_and_prefixes() {
        let mut cache = DiscoveryCache::default();
        let now = Instant::now();
        let endpoint = ServiceEndpoint::tcp("admin", "127.0.0.1", 7500, "admin");
        let game_admin =
            DiscoveryCacheKey::endpoint("test:", "game-server", "admin", ENDPOINT_PICK_STRATEGY);
        let proxy_admin =
            DiscoveryCacheKey::endpoint("test:", "game-proxy", "admin", ENDPOINT_PICK_STRATEGY);
        let default_game_admin =
            DiscoveryCacheKey::endpoint("", "game-server", "admin", ENDPOINT_PICK_STRATEGY);

        cache.put(
            game_admin.clone(),
            DiscoveryCacheValue::Endpoint(Some(endpoint.clone())),
            now,
            Duration::from_secs(1),
        );
        cache.put(
            proxy_admin.clone(),
            DiscoveryCacheValue::Endpoint(Some(endpoint.clone())),
            now,
            Duration::from_secs(1),
        );
        cache.put(
            default_game_admin.clone(),
            DiscoveryCacheValue::Endpoint(Some(endpoint)),
            now,
            Duration::from_secs(1),
        );

        cache.clear_service("test:", "game-server");

        assert!(cache.get(&game_admin, now).is_none());
        assert!(cache.get(&proxy_admin, now).is_some());
        assert!(cache.get(&default_game_admin, now).is_some());
    }

    #[test]
    fn discovery_watch_config_builders_are_explicit() {
        let config = DiscoveryWatchConfig::new(Duration::from_millis(50))
            .retain_stale_on_error(true)
            .refresh_immediately(false);

        assert_eq!(config.interval, Duration::from_millis(50));
        assert!(config.retain_stale_on_error);
        assert!(!config.refresh_immediately);
    }
}
