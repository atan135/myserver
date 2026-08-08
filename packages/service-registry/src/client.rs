use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, RwLock};

use crate::discovery_metrics::record_discovery_metric;
use crate::types::{ServiceEndpoint, ServiceInstance};

pub const REGISTRY_INSTANCE_TTL_SECONDS: u64 = 90;
pub const REGISTRY_HEARTBEAT_TTL_SECONDS: u64 = 30;
pub const REGISTRY_INSTANCE_INDEX_TTL_SECONDS: u64 = 300;
pub const REGISTRY_MAX_INSTANCES_PER_SERVICE: usize = 64;

const DEFAULT_DISCOVERY_CACHE_TTL: Duration = Duration::from_secs(1);
const INSTANCE_DISCOVERY_STRATEGY: &str = "healthy_instances_sorted_v1";
const INSTANCE_PICK_STRATEGY: &str = "weighted_stable_instance_v1";
const ENDPOINT_PICK_STRATEGY: &str = "weighted_stable_endpoint_v1";
const ALL_ENDPOINTS_STRATEGY: &str = "all_healthy_endpoints_sorted_v1";

#[derive(Debug)]
pub struct RegistryCapacityError {
    pub service_name: String,
}

impl std::fmt::Display for RegistryCapacityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "REGISTRY_CAPACITY_EXCEEDED: service={}",
            self.service_name
        )
    }
}

impl std::error::Error for RegistryCapacityError {}

/// 服务注册中心客户端
pub struct RegistryClient {
    redis: redis::Client,
    instance_id: String,
    service_name: String,
    key_prefix: String,
    heartbeat_interval_secs: u64,
    heartbeat_ttl_secs: u64,
    instance_ttl_secs: u64,
    instance_index_ttl_secs: u64,
    max_instances_per_service: usize,
    discovery_cache_ttl: Duration,
    discovery_cache: Mutex<DiscoveryCache>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    Succeeded,
    Failed,
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
            heartbeat_ttl_secs: REGISTRY_HEARTBEAT_TTL_SECONDS,
            instance_ttl_secs: REGISTRY_INSTANCE_TTL_SECONDS,
            instance_index_ttl_secs: REGISTRY_INSTANCE_INDEX_TTL_SECONDS,
            max_instances_per_service: REGISTRY_MAX_INSTANCES_PER_SERVICE,
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
        self.discovery_cache_ttl = ttl.min(Duration::from_secs(REGISTRY_HEARTBEAT_TTL_SECONDS));
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

    /// 注册服务实例
    pub async fn register(
        &self,
        instance: &ServiceInstance,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        validate_registry_identity(&self.service_name, &self.instance_id, instance)?;
        let mut conn = self.redis.get_multiplexed_async_connection().await?;
        let key = self.instance_key();
        let json = serde_json::to_string(&instance.clone().normalized())?;
        let heartbeat_key = self.heartbeat_key();
        let index_key = self.instance_index_key();
        let response: Vec<String> = redis::cmd("EVAL")
            .arg(REGISTRY_REGISTER_SCRIPT)
            .arg(3)
            .arg(&index_key)
            .arg(&key)
            .arg(&heartbeat_key)
            .arg(&self.instance_id)
            .arg(&json)
            .arg(unix_now_seconds())
            .arg(self.instance_ttl_secs)
            .arg(self.heartbeat_ttl_secs)
            .arg(self.instance_index_ttl_secs)
            .arg(self.max_instances_per_service)
            .arg(registry_instance_key_prefix(
                &self.key_prefix,
                &self.service_name,
            ))
            .arg(registry_heartbeat_key_prefix(
                &self.key_prefix,
                &self.service_name,
            ))
            .query_async(&mut conn)
            .await?;
        ensure_registry_script_success(&response, &self.service_name)?;

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
        let index_key = self.instance_index_key();
        let _: Vec<String> = redis::cmd("EVAL")
            .arg(REGISTRY_DEREGISTER_SCRIPT)
            .arg(3)
            .arg(&index_key)
            .arg(&key)
            .arg(&heartbeat_key)
            .arg(&self.instance_id)
            .query_async(&mut conn)
            .await?;

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
        heartbeat_registry_instance(
            &self.redis,
            &self.key_prefix,
            &self.service_name,
            &self.instance_id,
            self.instance_ttl_secs,
            self.heartbeat_ttl_secs,
            self.instance_index_ttl_secs,
            self.max_instances_per_service,
        )
        .await
    }

    /// 启动心跳任务
    pub fn start_heartbeat_task(&self) -> tokio::task::JoinHandle<()> {
        self.start_heartbeat_task_with_observer(|_| {})
    }

    /// 启动心跳任务，并以不含底层错误详情的结果通知调用方。
    pub fn start_heartbeat_task_with_observer<F>(&self, observer: F) -> tokio::task::JoinHandle<()>
    where
        F: Fn(HeartbeatOutcome) + Send + Sync + 'static,
    {
        let heartbeat_ttl = self.heartbeat_ttl_secs;
        let heartbeat_interval = self.heartbeat_interval_secs;
        let instance_ttl = self.instance_ttl_secs;
        let index_ttl = self.instance_index_ttl_secs;
        let max_instances = self.max_instances_per_service;
        let redis = self.redis.clone();
        let instance_id = self.instance_id.clone();
        let service_name = self.service_name.clone();
        let key_prefix = self.key_prefix.clone();

        tokio::spawn(async move {
            let ttl = heartbeat_ttl;
            let interval = heartbeat_interval;

            // 立即发送一次心跳
            let result = heartbeat_registry_instance(
                &redis,
                &key_prefix,
                &service_name,
                &instance_id,
                instance_ttl,
                ttl,
                index_ttl,
                max_instances,
            )
            .await;
            notify_heartbeat_observer(&observer, &result);
            if result.is_err() {
                tracing::warn!("failed to send heartbeat");
            }

            let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(interval));
            loop {
                ticker.tick().await;

                let result = heartbeat_registry_instance(
                    &redis,
                    &key_prefix,
                    &service_name,
                    &instance_id,
                    instance_ttl,
                    ttl,
                    index_ttl,
                    max_instances,
                )
                .await;
                notify_heartbeat_observer(&observer, &result);
                if result.is_err() {
                    tracing::warn!("failed to send heartbeat");
                }
            }
        })
    }

    /// 发现服务实例（查询所有健康实例）
    pub async fn discover(
        &self,
        service_name: &str,
    ) -> Result<Vec<ServiceInstance>, Box<dyn std::error::Error + Send + Sync>> {
        match self.discover_with_cache_expiry(service_name).await {
            Ok((instances, _)) => {
                record_discovery_metric(
                    service_name,
                    "",
                    "registry",
                    if instances.is_empty() {
                        "no_healthy_instance"
                    } else {
                        "discovered"
                    },
                );
                Ok(instances)
            }
            Err(error) => {
                record_discovery_metric(service_name, "", "registry", "registry_error");
                Err(error)
            }
        }
    }

    pub async fn refresh_discovery_snapshot(
        &self,
        service_name: &str,
    ) -> Result<DiscoverySnapshot, Box<dyn std::error::Error + Send + Sync>> {
        match self.refresh_discovery_instances(service_name).await {
            Ok(instances) => {
                record_discovery_metric(
                    service_name,
                    "",
                    "registry",
                    if instances.is_empty() {
                        "no_healthy_instance"
                    } else {
                        "discovered"
                    },
                );
                Ok(DiscoverySnapshot::ok(service_name, instances))
            }
            Err(error) => {
                record_discovery_metric(service_name, "", "registry", "registry_error");
                Err(error)
            }
        }
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
        validate_registry_identifier(service_name, "service name")?;
        let active_after = unix_now_seconds().saturating_sub(self.heartbeat_ttl_secs);
        let instance_ids: Vec<String> = redis::cmd("ZRANGEBYSCORE")
            .arg(registry_instance_index_key(&self.key_prefix, service_name))
            .arg(active_after)
            .arg("+inf")
            .arg("LIMIT")
            .arg(0)
            .arg(self.max_instances_per_service)
            .query_async(&mut conn)
            .await?;
        let instance_ids = instance_ids
            .into_iter()
            .filter(|instance_id| is_valid_registry_identifier(instance_id))
            .take(self.max_instances_per_service)
            .collect::<Vec<_>>();
        if instance_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut pipeline = redis::pipe();
        for instance_id in &instance_ids {
            pipeline
                .cmd("HGET")
                .arg(registry_instance_key(
                    &self.key_prefix,
                    service_name,
                    instance_id,
                ))
                .arg("data")
                .cmd("EXISTS")
                .arg(registry_heartbeat_key(
                    &self.key_prefix,
                    service_name,
                    instance_id,
                ));
        }
        let values: Vec<redis::Value> = pipeline.query_async(&mut conn).await?;
        let mut instances = Vec::new();

        for (offset, instance_id) in instance_ids.iter().enumerate() {
            let data: Option<String> = redis::from_redis_value(&values[offset * 2])?;
            let exists: bool = redis::from_redis_value(&values[offset * 2 + 1])?;
            if !exists || data.is_none() {
                continue;
            }
            if let Ok(instance) =
                serde_json::from_str::<ServiceInstance>(data.as_deref().unwrap_or_default())
            {
                let instance = instance.normalized();
                if instance.id == *instance_id && instance.name == service_name && instance.healthy
                {
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
                record_discovery_metric(service_name, "", "registry", "registry_error");
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
            record_discovery_metric(service_name, "", "registry", "no_healthy_instance");
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
        record_discovery_metric(service_name, "", "registry", "discovered");
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
                record_discovery_metric(service_name, endpoint_name, "registry", "registry_error");
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
            record_discovery_metric(service_name, endpoint_name, "registry", "discovered");
            tracing::info!(
                service = %service_name,
                endpoint = %endpoint_name,
                instance_id = %instance.id,
                source = "registry",
                reason = "discovered",
                "service endpoint discovery completed"
            );
        } else {
            if instances.is_empty() {
                record_discovery_metric(service_name, "", "registry", "no_healthy_instance");
            }
            record_discovery_metric(service_name, endpoint_name, "registry", "endpoint_missing");
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
                record_discovery_metric(service_name, endpoint_name, "registry", "registry_error");
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
            if instances.is_empty() {
                record_discovery_metric(service_name, "", "registry", "no_healthy_instance");
            }
            record_discovery_metric(service_name, endpoint_name, "registry", "endpoint_missing");
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
            record_discovery_metric(service_name, endpoint_name, "registry", "discovered");
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

    fn instance_index_key(&self) -> String {
        registry_instance_index_key(&self.key_prefix, &self.service_name)
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

fn heartbeat_outcome<T, E>(result: &Result<T, E>) -> HeartbeatOutcome {
    if result.is_ok() {
        HeartbeatOutcome::Succeeded
    } else {
        HeartbeatOutcome::Failed
    }
}

fn notify_heartbeat_observer<F, T, E>(observer: &F, result: &Result<T, E>)
where
    F: Fn(HeartbeatOutcome),
{
    observer(heartbeat_outcome(result));
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
                client
                    .clear_cached_discovery_for_service(service_name)
                    .await;
            }
            let previous = snapshot.read().await.clone();
            let instances = if retain_stale_on_error {
                previous.instances
            } else {
                Vec::new()
            };
            DiscoverySnapshot::failure(
                service_name,
                instances,
                previous.updated_at,
                error.to_string(),
            )
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
        .map(|milliseconds| {
            Duration::from_millis(milliseconds.min(REGISTRY_HEARTBEAT_TTL_SECONDS * 1000))
        })
        .unwrap_or(DEFAULT_DISCOVERY_CACHE_TTL)
}

fn registry_instance_key(prefix: &str, service_name: &str, instance_id: &str) -> String {
    format!("{prefix}service:{service_name}:instances:{instance_id}")
}

fn registry_heartbeat_key(prefix: &str, service_name: &str, instance_id: &str) -> String {
    format!("{prefix}heartbeat:{service_name}:{instance_id}")
}

fn registry_instance_index_key(prefix: &str, service_name: &str) -> String {
    format!("{prefix}service:{service_name}:instance-index")
}

fn registry_instance_key_prefix(prefix: &str, service_name: &str) -> String {
    format!("{prefix}service:{service_name}:instances:")
}

fn registry_heartbeat_key_prefix(prefix: &str, service_name: &str) -> String {
    format!("{prefix}heartbeat:{service_name}:")
}

async fn heartbeat_registry_instance(
    redis: &redis::Client,
    key_prefix: &str,
    service_name: &str,
    instance_id: &str,
    instance_ttl_secs: u64,
    heartbeat_ttl_secs: u64,
    instance_index_ttl_secs: u64,
    max_instances_per_service: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    validate_registry_identifier(service_name, "service name")?;
    validate_registry_identifier(instance_id, "instance id")?;
    let mut conn = redis.get_multiplexed_async_connection().await?;
    let response: Vec<String> = redis::cmd("EVAL")
        .arg(REGISTRY_HEARTBEAT_SCRIPT)
        .arg(3)
        .arg(registry_instance_index_key(key_prefix, service_name))
        .arg(registry_instance_key(key_prefix, service_name, instance_id))
        .arg(registry_heartbeat_key(
            key_prefix,
            service_name,
            instance_id,
        ))
        .arg(instance_id)
        .arg(unix_now_seconds())
        .arg(instance_ttl_secs)
        .arg(heartbeat_ttl_secs)
        .arg(instance_index_ttl_secs)
        .arg(max_instances_per_service)
        .arg(registry_instance_key_prefix(key_prefix, service_name))
        .arg(registry_heartbeat_key_prefix(key_prefix, service_name))
        .query_async(&mut conn)
        .await?;
    ensure_registry_script_success(&response, service_name)
}

fn validate_registry_identity(
    service_name: &str,
    instance_id: &str,
    instance: &ServiceInstance,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    validate_registry_identifier(service_name, "service name")?;
    validate_registry_identifier(instance_id, "instance id")?;
    if instance.name != service_name || instance.id != instance_id {
        return Err(registry_lifecycle_error(
            "REGISTRY_INSTANCE_IDENTITY_MISMATCH",
        ));
    }
    Ok(())
}

fn validate_registry_identifier(
    value: &str,
    label: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if is_valid_registry_identifier(value) {
        return Ok(());
    }
    Err(registry_lifecycle_error(&format!(
        "invalid registry {label}: must match ^[A-Za-z0-9][A-Za-z0-9._-]{{0,63}}$"
    )))
}

fn is_valid_registry_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() || value.len() > 64 {
        return false;
    }
    characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
}

fn unix_now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn ensure_registry_script_success(
    response: &[String],
    service_name: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match response.first().map(String::as_str) {
        Some("OK") => Ok(()),
        Some("REGISTRY_CAPACITY_EXCEEDED") => Err(Box::new(RegistryCapacityError {
            service_name: service_name.to_string(),
        })),
        Some(code) => Err(registry_lifecycle_error(code)),
        None => Err(registry_lifecycle_error("REGISTRY_LIFECYCLE_FAILED")),
    }
}

fn registry_lifecycle_error(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(std::io::Error::other(message.to_string()))
}

const REGISTRY_REGISTER_SCRIPT: &str = r#"
local cutoff = tonumber(ARGV[3]) - tonumber(ARGV[4])
local stale = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', '(' .. cutoff)
for _, stale_id in ipairs(stale) do
  redis.call('DEL', ARGV[8] .. stale_id)
  redis.call('DEL', ARGV[9] .. stale_id)
end
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', '(' .. cutoff)
if not redis.call('ZSCORE', KEYS[1], ARGV[1]) and redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[7]) then
  return { 'REGISTRY_CAPACITY_EXCEEDED' }
end
redis.call('HSET', KEYS[2], 'data', ARGV[2])
redis.call('EXPIRE', KEYS[2], ARGV[4])
redis.call('SET', KEYS[3], '1', 'EX', ARGV[5])
redis.call('ZADD', KEYS[1], ARGV[3], ARGV[1])
redis.call('EXPIRE', KEYS[1], ARGV[6])
return { 'OK', #stale }
"#;

const REGISTRY_HEARTBEAT_SCRIPT: &str = r#"
local cutoff = tonumber(ARGV[2]) - tonumber(ARGV[3])
local stale = redis.call('ZRANGEBYSCORE', KEYS[1], '-inf', '(' .. cutoff)
for _, stale_id in ipairs(stale) do
  redis.call('DEL', ARGV[7] .. stale_id)
  redis.call('DEL', ARGV[8] .. stale_id)
end
redis.call('ZREMRANGEBYSCORE', KEYS[1], '-inf', '(' .. cutoff)
if redis.call('EXISTS', KEYS[2]) == 0 then
  return { 'REGISTRY_INSTANCE_MISSING' }
end
if not redis.call('ZSCORE', KEYS[1], ARGV[1]) and redis.call('ZCARD', KEYS[1]) >= tonumber(ARGV[6]) then
  return { 'REGISTRY_CAPACITY_EXCEEDED' }
end
redis.call('EXPIRE', KEYS[2], ARGV[3])
redis.call('SET', KEYS[3], '1', 'EX', ARGV[4])
redis.call('ZADD', KEYS[1], ARGV[2], ARGV[1])
redis.call('EXPIRE', KEYS[1], ARGV[5])
return { 'OK', #stale }
"#;

const REGISTRY_DEREGISTER_SCRIPT: &str = r#"
redis.call('DEL', KEYS[2])
redis.call('DEL', KEYS[3])
redis.call('ZREM', KEYS[1], ARGV[1])
if redis.call('ZCARD', KEYS[1]) == 0 then
  redis.call('DEL', KEYS[1])
end
return { 'OK' }
"#;

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
    pick_endpoint_candidate_weighted_stable(instances, endpoint_name).map(|(_, endpoint)| endpoint)
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
    fn heartbeat_outcome_exposes_only_success_or_failure() {
        assert_eq!(
            heartbeat_outcome(&Ok::<_, &str>(())),
            HeartbeatOutcome::Succeeded
        );
        assert_eq!(
            heartbeat_outcome(&Err::<(), _>("redis://user:secret@internal")),
            HeartbeatOutcome::Failed
        );

        let observed = std::sync::Mutex::new(Vec::new());
        let observer = |outcome| observed.lock().unwrap().push(outcome);
        notify_heartbeat_observer(&observer, &Ok::<_, &str>(()));
        notify_heartbeat_observer(&observer, &Err::<(), _>("redis://user:secret@internal"));
        assert_eq!(
            *observed.lock().unwrap(),
            vec![HeartbeatOutcome::Succeeded, HeartbeatOutcome::Failed]
        );
    }

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
            registry_instance_index_key("test:", "game-server"),
            "test:service:game-server:instance-index"
        );
    }

    #[test]
    fn registry_index_contract_is_bounded_and_contains_no_scan_fallback() {
        assert_eq!(REGISTRY_INSTANCE_TTL_SECONDS, 90);
        assert_eq!(REGISTRY_HEARTBEAT_TTL_SECONDS, 30);
        assert_eq!(REGISTRY_INSTANCE_INDEX_TTL_SECONDS, 300);
        assert_eq!(REGISTRY_MAX_INSTANCES_PER_SERVICE, 64);
        assert!(REGISTRY_REGISTER_SCRIPT.contains("ZREMRANGEBYSCORE"));
        assert!(REGISTRY_HEARTBEAT_SCRIPT.contains("REGISTRY_CAPACITY_EXCEEDED"));
        assert!(!REGISTRY_REGISTER_SCRIPT.contains("SCAN"));
        assert!(!REGISTRY_HEARTBEAT_SCRIPT.contains("SCAN"));
        assert!(is_valid_registry_identifier("game-server.v2_1"));
        assert!(!is_valid_registry_identifier("game:server"));
        assert!(!is_valid_registry_identifier(" game-server"));
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
