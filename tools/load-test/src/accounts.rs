use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{AccountPrepareConfig, PrivateConfig, SessionEffect};

pub const MANIFEST_SCHEMA_VERSION: u32 = 1;
/// The default `auth-http` character-name contract allows at most 16 ASCII
/// characters. Preparation uses this conservative bound because it cannot
/// inspect a live service configuration before a guarded write.
pub const AUTH_HTTP_CHARACTER_NAME_MAX_LENGTH: usize = 16;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccountSource {
    AuthHttpRegister,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CharacterReadiness {
    NotPrepared,
    Ready,
    VerificationFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccountPreparationState {
    Planned,
    Prepared,
    Verified,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AccountManifestEntry {
    pub logical_account_id: String,
    pub source: AccountSource,
    pub environment: String,
    pub batch: String,
    pub character_readiness: CharacterReadiness,
    pub last_verified_unix_ms: Option<u64>,
    pub preparation_state: AccountPreparationState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AccountManifest {
    pub schema_version: u32,
    pub environment: String,
    pub batch: String,
    pub source: AccountSource,
    pub created_unix_ms: u64,
    pub accounts: Vec<AccountManifestEntry>,
}

impl AccountManifest {
    pub fn planned(
        environment: &str,
        prepare: &AccountPrepareConfig,
        account_count: u32,
        now_unix_ms: u64,
    ) -> Self {
        let accounts = (1..=account_count)
            .map(|index| AccountManifestEntry {
                logical_account_id: logical_account_id(environment, &prepare.batch, index),
                source: AccountSource::AuthHttpRegister,
                environment: environment.to_string(),
                batch: prepare.batch.clone(),
                character_readiness: CharacterReadiness::NotPrepared,
                last_verified_unix_ms: None,
                preparation_state: AccountPreparationState::Planned,
            })
            .collect();
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            environment: environment.to_string(),
            batch: prepare.batch.clone(),
            source: AccountSource::AuthHttpRegister,
            created_unix_ms: now_unix_ms,
            accounts,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "account manifest schema_version must be {MANIFEST_SCHEMA_VERSION}"
            ));
        }
        if self.environment.trim().is_empty() || self.batch.trim().is_empty() {
            return Err("account manifest environment and batch are required".into());
        }
        if self.accounts.is_empty() {
            return Err("account manifest must contain at least one account".into());
        }
        let mut seen = BTreeMap::new();
        for account in &self.accounts {
            if account.environment != self.environment || account.batch != self.batch {
                return Err(
                    "account manifest entry does not match manifest environment or batch".into(),
                );
            }
            if account.logical_account_id.trim().is_empty()
                || seen.insert(&account.logical_account_id, ()).is_some()
            {
                return Err(
                    "account manifest logical account ids must be non-empty and unique".into(),
                );
            }
        }
        Ok(())
    }

    pub fn ready_accounts(&self) -> impl Iterator<Item = &AccountManifestEntry> {
        self.accounts.iter().filter(|entry| {
            entry.preparation_state == AccountPreparationState::Verified
                && entry.character_readiness == CharacterReadiness::Ready
        })
    }
}

pub fn logical_account_id(environment: &str, batch: &str, index: u32) -> String {
    format!("loadtest_{environment}_{batch}_{index:06}")
}

/// `auth-http` password account names accept lowercase letters, digits, and
/// underscores. Logical batch names may use hyphens for readability, so only
/// the transport-local projection changes them to underscores.
pub fn auth_login_name(logical_account_id: &str) -> Result<String, String> {
    let login_name = logical_account_id.replace('-', "_");
    if !(3..=32).contains(&login_name.len())
        || !login_name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(
            "logical account id cannot be represented as a supported auth-http login name".into(),
        );
    }
    Ok(login_name)
}

/// Projects the credential-free preparation identity into a valid, stable
/// `auth-http` character name. Short configured names remain readable; longer
/// prefix/batch combinations use a deterministic compact form so retries and
/// resume attempts address the same name without exceeding the server default.
pub fn auth_character_name(character_prefix: &str, batch: &str, index: u32) -> String {
    let candidate = format!("{character_prefix}_{batch}_{index}");
    if candidate.len() <= AUTH_HTTP_CHARACTER_NAME_MAX_LENGTH {
        return candidate;
    }

    let digest = Sha256::digest(format!("{character_prefix}:{batch}"));
    format!("lt{:02x}{:02x}_{index:08x}", digest[0], digest[1])
}

pub fn write_manifest(path: &Path, manifest: &AccountManifest) -> Result<(), String> {
    manifest.validate()?;
    let parent = path
        .parent()
        .ok_or("account manifest path must have a parent directory")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::write(
        path,
        serde_json::to_vec_pretty(manifest).expect("manifest serializes"),
    )
    .map_err(|error| error.to_string())
}

pub fn read_manifest(path: &Path) -> Result<AccountManifest, String> {
    let manifest: AccountManifest =
        serde_json::from_slice(&fs::read(path).map_err(|error| error.to_string())?)
            .map_err(|error| format!("account manifest is invalid: {error}"))?;
    manifest.validate()?;
    Ok(manifest)
}

pub trait SecretProvider {
    fn password_for(&self, logical_account_id: &str) -> Result<String, String>;
}

/// Resolves only the secret reference associated with a logical account. The
/// actual password never enters configuration, manifests, reports, or errors.
pub struct EnvironmentSecretProvider<'a> {
    private: &'a PrivateConfig,
}

impl<'a> EnvironmentSecretProvider<'a> {
    pub fn new(private: &'a PrivateConfig) -> Self {
        Self { private }
    }
}

impl SecretProvider for EnvironmentSecretProvider<'_> {
    fn password_for(&self, logical_account_id: &str) -> Result<String, String> {
        let reference = self
            .private
            .account_credentials
            .get(logical_account_id)
            .ok_or("secret reference is missing for a logical account")?;
        env::var(reference)
            .map_err(|_| "secret value is unavailable; configure its reference".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLease {
    pub logical_account_id: String,
    pub owner: String,
    pub expires_monotonic_ms: u64,
    pub shared: bool,
}

#[derive(Debug, Default)]
pub struct AccountLeasePool {
    leases: BTreeMap<String, BTreeMap<String, AccountLease>>,
}

impl AccountLeasePool {
    pub fn reclaim_expired(&mut self, now_monotonic_ms: u64) {
        self.leases.retain(|_, account_leases| {
            account_leases.retain(|_, lease| lease.expires_monotonic_ms > now_monotonic_ms);
            !account_leases.is_empty()
        });
    }

    pub fn acquire(
        &mut self,
        logical_account_id: &str,
        owner: &str,
        now_monotonic_ms: u64,
        ttl_ms: u64,
    ) -> Result<AccountLease, String> {
        self.reclaim_expired(now_monotonic_ms);
        if self.leases.contains_key(logical_account_id) {
            return Err("account lease is already held".into());
        }
        let lease = AccountLease {
            logical_account_id: logical_account_id.to_string(),
            owner: owner.to_string(),
            expires_monotonic_ms: now_monotonic_ms.saturating_add(ttl_ms.max(1)),
            shared: false,
        };
        self.leases
            .entry(logical_account_id.to_string())
            .or_default()
            .insert(owner.to_string(), lease.clone());
        Ok(lease)
    }

    fn acquire_shared(
        &mut self,
        logical_account_id: &str,
        owner: &str,
        now_monotonic_ms: u64,
        ttl_ms: u64,
        expected_effect: SessionEffect,
    ) -> Result<AccountLease, String> {
        self.reclaim_expired(now_monotonic_ms);
        let account_leases = self
            .leases
            .entry(logical_account_id.to_string())
            .or_default();
        if account_leases.values().any(|lease| !lease.shared) {
            return Err(
                "account is held exclusively and cannot enter same-account scenario".into(),
            );
        }
        if account_leases.contains_key(owner) {
            return Err("same-account lease owner is already registered".into());
        }
        let lease = AccountLease {
            logical_account_id: logical_account_id.to_string(),
            owner: owner.to_string(),
            expires_monotonic_ms: now_monotonic_ms.saturating_add(ttl_ms.max(1)),
            shared: true,
        };
        // The caller must supply an explicit expected session effect before it
        // can reach this path. Keep the value consumed here to make bypassing
        // that declaration impossible at the allocation boundary.
        let _ = expected_effect;
        account_leases.insert(owner.to_string(), lease.clone());
        Ok(lease)
    }

    pub fn release(&mut self, lease: &AccountLease) -> bool {
        let Some(account_leases) = self.leases.get_mut(&lease.logical_account_id) else {
            return false;
        };
        let released = account_leases
            .get(&lease.owner)
            .is_some_and(|existing| existing == lease)
            && account_leases.remove(&lease.owner).is_some();
        if account_leases.is_empty() {
            self.leases.remove(&lease.logical_account_id);
        }
        released
    }

    pub fn assign_players(
        &mut self,
        accounts: &[String],
        virtual_players: u32,
        owner_prefix: &str,
        now_monotonic_ms: u64,
        ttl_ms: u64,
        allow_same_account_concurrency: bool,
        expected_effect: Option<SessionEffect>,
    ) -> Result<Vec<AccountLease>, String> {
        if accounts.is_empty() {
            return Err("no verified accounts are available".into());
        }
        if allow_same_account_concurrency && expected_effect.is_none() {
            return Err("same-account assignment requires an explicit session effect".into());
        }
        if !allow_same_account_concurrency && virtual_players as usize > accounts.len() {
            return Err("account pool is smaller than requested virtual players; implicit reuse is forbidden".into());
        }

        let mut result = Vec::with_capacity(virtual_players as usize);
        for player_index in 0..virtual_players as usize {
            let account = &accounts[player_index % accounts.len()];
            let owner = format!("{owner_prefix}-{player_index}");
            let lease = match (allow_same_account_concurrency, expected_effect) {
                (true, Some(effect)) => {
                    self.acquire_shared(account, &owner, now_monotonic_ms, ttl_ms, effect)?
                }
                (false, None) => self.acquire(account, &owner, now_monotonic_ms, ttl_ms)?,
                _ => unreachable!("same-account scenario is validated before assignment"),
            };
            result.push(lease);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountPrepareConfig;

    #[test]
    fn manifest_schema_keeps_only_logical_identity_and_readiness() {
        let manifest = AccountManifest::planned("local", &AccountPrepareConfig::default(), 2, 1);
        assert_eq!(
            manifest.accounts[0].logical_account_id,
            "loadtest_local_default_000001"
        );
        let text = serde_json::to_string(&manifest).unwrap();
        assert!(!text.contains("password"));
        assert!(!text.contains("ticket"));
        assert!(manifest.validate().is_ok());
    }

    #[test]
    fn logical_batch_names_are_projected_to_supported_login_names() {
        assert_eq!(
            auth_login_name("loadtest_local_blue-green_000001").unwrap(),
            "loadtest_local_blue_green_000001"
        );
        assert!(auth_login_name("loadtest_local_this-batch-name-is-far-too-long_000001").is_err());
    }

    #[test]
    fn long_prepare_names_use_stable_auth_http_compatible_projection() {
        assert_eq!(
            auth_character_name("loadtest", "smoke01", 1),
            "lt140f_00000001"
        );
        let compact = auth_character_name("loadtest", "batch-name-that-is-too-long", 42);
        assert_eq!(
            compact,
            auth_character_name("loadtest", "batch-name-that-is-too-long", 42)
        );
        assert!(compact.len() <= AUTH_HTTP_CHARACTER_NAME_MAX_LENGTH);
        assert!(
            compact
                .bytes()
                .all(|byte| { byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' })
        );
    }

    #[test]
    fn lease_conflict_expiry_and_release_are_explicit() {
        let mut pool = AccountLeasePool::default();
        let first = pool.acquire("a", "worker-a", 1, 10).unwrap();
        assert!(pool.acquire("a", "worker-b", 1, 10).is_err());
        assert!(!pool.release(&AccountLease {
            logical_account_id: "a".into(),
            owner: "worker-b".into(),
            expires_monotonic_ms: 11,
            shared: false,
        }));
        pool.reclaim_expired(11);
        assert!(pool.acquire("a", "worker-b", 11, 10).is_ok());
        assert!(!pool.release(&first));
    }

    #[test]
    fn normal_scenarios_cannot_reuse_an_account_but_explicit_ones_must_declare_effect() {
        let mut pool = AccountLeasePool::default();
        assert!(
            pool.assign_players(&["a".into()], 2, "run", 0, 10, false, None)
                .is_err()
        );
        assert!(
            pool.assign_players(&["a".into()], 2, "run", 0, 10, true, None)
                .is_err()
        );
        let shared = pool
            .assign_players(
                &["a".into()],
                2,
                "same-account",
                0,
                10,
                true,
                Some(SessionEffect::SessionKick),
            )
            .unwrap();
        assert_eq!(shared.len(), 2);
        assert!(shared.iter().all(|lease| lease.shared));
    }
}
