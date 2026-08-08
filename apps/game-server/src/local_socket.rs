use std::io;
use std::time::Duration;

use interprocess::local_socket::{
    GenericFilePath, ListenerOptions, ToFsName,
    tokio::{Listener, Stream},
    traits::tokio::Stream as _,
};

pub fn normalize_name(name: &str) -> String {
    #[cfg(windows)]
    {
        if name.starts_with("\\\\.\\pipe\\") {
            return name.to_string();
        }
        return format!("\\\\.\\pipe\\{}", name.replace('/', "_").replace('\\', "_"));
    }

    #[cfg(not(windows))]
    {
        if name.starts_with('/') {
            return name.to_string();
        }
        format!("/tmp/{}", name)
    }
}

pub fn to_name(name: &str) -> io::Result<interprocess::local_socket::Name<'_>> {
    normalize_name(name).to_fs_name::<GenericFilePath>()
}

pub fn create_listener(name: &str) -> io::Result<Listener> {
    ListenerOptions::new().name(to_name(name)?).create_tokio()
}

#[derive(Clone, Debug)]
pub struct OwnedSocketPath {
    pub name: String,
    #[cfg(not(windows))]
    device: u64,
    #[cfg(not(windows))]
    inode: u64,
}

pub fn capture_owned_socket(name: &str) -> io::Result<OwnedSocketPath> {
    #[cfg(windows)]
    {
        Ok(OwnedSocketPath {
            name: name.to_string(),
        })
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let path = normalize_name(name);
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("owned socket target is not a socket: {path}"),
            ));
        }
        Ok(OwnedSocketPath {
            name: name.to_string(),
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

pub fn remove_owned_socket_path(owned: &OwnedSocketPath) -> io::Result<()> {
    #[cfg(windows)]
    {
        let _ = owned;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let path = normalize_name(&owned.name);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_socket()
            || metadata.dev() != owned.device
            || metadata.ino() != owned.inode
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("refusing to remove replaced socket target {path}"),
            ));
        }
        std::fs::remove_file(path)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SocketReclaimConfig {
    pub timeout: Duration,
    pub spin_delay: Duration,
    pub probe_timeout: Duration,
}

impl SocketReclaimConfig {
    pub fn try_from_env() -> io::Result<Self> {
        Ok(Self {
            timeout: duration_from_env("GAME_SOCKET_RECLAIM_TIMEOUT_MS", 2_000, 1, 10_000)?,
            spin_delay: duration_from_env("GAME_SOCKET_RECLAIM_SPIN_MS", 25, 1, 1_000)?,
            probe_timeout: duration_from_env("GAME_SOCKET_PROBE_TIMEOUT_MS", 100, 1, 1_000)?,
        })
    }
}

fn duration_from_env(name: &str, default: u64, minimum: u64, maximum: u64) -> io::Result<Duration> {
    let value = match std::env::var(name) {
        Ok(value) => value.trim().parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{name} must be an integer in {minimum}..={maximum}"),
            )
        })?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidInput, error)),
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be in {minimum}..={maximum}"),
        ));
    }
    Ok(Duration::from_millis(value))
}

pub async fn create_owned_listener(
    name: &str,
    owned_names: &[String],
    lease_owned: bool,
    config: SocketReclaimConfig,
) -> io::Result<Listener> {
    validate_reclaim_authority(name, owned_names, lease_owned)?;
    let deadline = tokio::time::Instant::now() + config.timeout;
    loop {
        match create_listener(name) {
            Ok(listener) => return Ok(listener),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("timed out reclaiming owned socket {name}"),
                    ));
                }
                reclaim_stale_socket(name, config.probe_timeout).await?;
                tokio::time::sleep(config.spin_delay).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn validate_reclaim_authority(
    name: &str,
    owned_names: &[String],
    lease_owned: bool,
) -> io::Result<()> {
    if !lease_owned {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "worker lease is required before socket reclaim",
        ));
    }
    if !owned_names.iter().any(|owned| owned == name) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket path is not the current instance owned target",
        ));
    }
    Ok(())
}

pub fn prepare_owned_socket_root(owned_names: &[String], lease_owned: bool) -> io::Result<()> {
    if !lease_owned {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "worker lease is required before socket root preparation",
        ));
    }
    if owned_names.len() != 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "exactly two current-instance socket targets are required",
        ));
    }

    #[cfg(windows)]
    {
        Ok(())
    }
    #[cfg(not(windows))]
    {
        use std::path::Path;

        let first = Path::new(&owned_names[0]);
        let second = Path::new(&owned_names[1]);
        let root = first.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "socket target has no parent")
        })?;
        if !first.is_absolute()
            || !second.is_absolute()
            || second.parent() != Some(root)
            || first.file_name().is_none()
            || second.file_name().is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket targets must share one exact absolute parent",
            ));
        }

        let parent = root.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "socket root parent is missing",
            )
        })?;
        validate_real_directory(parent, "socket root parent")?;
        match std::fs::symlink_metadata(root) {
            Ok(_) => validate_real_directory(root, "socket root"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match std::fs::create_dir(root) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                validate_real_directory(root, "socket root")
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(not(windows))]
fn validate_real_directory(path: &std::path::Path, label: &str) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{label} must be a non-symlink directory: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(windows)]
async fn reclaim_stale_socket(_name: &str, _probe_timeout: Duration) -> io::Result<()> {
    // Named pipes disappear when their last listener handle closes. The bounded create loop waits
    // for that transition and never removes a filesystem object.
    Ok(())
}

#[cfg(not(windows))]
async fn reclaim_stale_socket(name: &str, probe_timeout: Duration) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let path = normalize_name(name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refusing to reclaim non-socket target {path}"),
        ));
    }

    match tokio::time::timeout(probe_timeout, Stream::connect(to_name(name)?)).await {
        Ok(Ok(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("owned socket {path} is active"),
            ));
        }
        Ok(Err(error)) if stale_socket_probe_error(&error) => {}
        Ok(Err(error)) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("refusing to reclaim socket {path} after inconclusive probe: {error}"),
            ));
        }
        Err(_) => {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("refusing to reclaim socket {path} after probe timeout"),
            ));
        }
    }

    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn stale_socket_probe_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}

pub async fn connect(name: &str) -> io::Result<Stream> {
    Stream::connect(to_name(name)?).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn create_listener_pair_with<T, Create>(
        local_name: &str,
        internal_name: &str,
        mut create: Create,
    ) -> io::Result<(T, T)>
    where
        Create: FnMut(&str) -> io::Result<T>,
    {
        let local_listener = create(local_name)?;
        let internal_listener = create(internal_name)?;
        Ok((local_listener, internal_listener))
    }

    fn unique_name(label: &str) -> String {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        format!(
            "myserver-startup-fixture-{}-{}-{label}.sock",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        )
    }

    #[test]
    fn listener_pair_stops_at_the_first_injected_socket_conflict() {
        let local_name = unique_name("proxy-local");
        let internal_name = unique_name("internal");
        let occupied = HashSet::from([local_name.clone(), internal_name.clone()]);
        let mut attempted = Vec::new();

        let error = create_listener_pair_with(&local_name, &internal_name, |name| {
            attempted.push(name.to_string());
            if occupied.contains(name) {
                Err::<(), _>(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "fixture listener already owns socket",
                ))
            } else {
                Ok(())
            }
        })
        .expect_err("occupied first socket must fail pair creation");

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert_eq!(attempted, vec![local_name]);
    }

    #[test]
    fn reclaim_requires_lease_and_exact_current_instance_target() {
        let owned = vec!["instance-a.sock".to_string()];
        assert_eq!(
            validate_reclaim_authority("instance-a.sock", &owned, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            validate_reclaim_authority("instance-b.sock", &owned, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        validate_reclaim_authority("instance-a.sock", &owned, true).unwrap();
    }

    #[test]
    fn sigkill_stale_probe_is_reclaimable_but_uncertain_probe_is_protected() {
        assert!(stale_socket_probe_error(&io::Error::from(
            io::ErrorKind::ConnectionRefused
        )));
        assert!(stale_socket_probe_error(&io::Error::from(
            io::ErrorKind::NotFound
        )));
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::TimedOut,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::Other,
        ] {
            assert!(!stale_socket_probe_error(&io::Error::from(kind)));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn two_occupied_socket_paths_reproduce_the_first_bootstrap_conflict() {
        struct SocketCleanup(Vec<String>);

        impl Drop for SocketCleanup {
            fn drop(&mut self) {
                for name in &self.0 {
                    let _ = std::fs::remove_file(normalize_name(name));
                }
            }
        }

        let local_name = unique_name("proxy-local");
        let internal_name = unique_name("internal");
        let _cleanup = SocketCleanup(vec![local_name.clone(), internal_name.clone()]);
        let occupied_local =
            create_listener(&local_name).expect("fixture local socket should bind");
        let occupied_internal =
            create_listener(&internal_name).expect("fixture internal socket should bind");

        let error =
            create_listener(&local_name).expect_err("occupied local socket must reject bootstrap");

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        drop(occupied_local);
        drop(occupied_internal);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sigkill_residual_socket_is_reclaimed_only_after_lease() {
        struct SocketCleanup(String);

        impl Drop for SocketCleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(normalize_name(&self.0));
            }
        }

        let name = unique_name("sigkill-stale");
        let _cleanup = SocketCleanup(name.clone());
        let listener = create_listener(&name).expect("fixture socket should bind");
        drop(listener);
        assert!(std::fs::symlink_metadata(normalize_name(&name)).is_ok());

        let replacement = create_owned_listener(
            &name,
            std::slice::from_ref(&name),
            true,
            SocketReclaimConfig {
                timeout: Duration::from_secs(1),
                spin_delay: Duration::from_millis(1),
                probe_timeout: Duration::from_millis(100),
            },
        )
        .await
        .expect("leased owner should reclaim its stale socket");
        drop(replacement);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn active_socket_is_never_reclaimed() {
        struct SocketCleanup(String);

        impl Drop for SocketCleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(normalize_name(&self.0));
            }
        }

        let name = unique_name("active");
        let _cleanup = SocketCleanup(name.clone());
        let active = create_listener(&name).expect("fixture socket should bind");
        let error = create_owned_listener(
            &name,
            std::slice::from_ref(&name),
            true,
            SocketReclaimConfig {
                timeout: Duration::from_secs(1),
                spin_delay: Duration::from_millis(1),
                probe_timeout: Duration::from_millis(100),
            },
        )
        .await
        .expect_err("active listener must retain ownership");

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        drop(active);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn non_socket_directory_and_symlink_targets_are_never_reclaimed() {
        use std::os::unix::fs::symlink;

        let config = SocketReclaimConfig {
            timeout: Duration::from_secs(1),
            spin_delay: Duration::from_millis(1),
            probe_timeout: Duration::from_millis(100),
        };
        let file_name = unique_name("ordinary-file");
        let directory_name = unique_name("directory");
        let symlink_name = unique_name("symlink");
        let file_path = normalize_name(&file_name);
        let directory_path = normalize_name(&directory_name);
        let symlink_path = normalize_name(&symlink_name);
        std::fs::write(&file_path, b"protected").unwrap();
        std::fs::create_dir(&directory_path).unwrap();
        symlink(&file_path, &symlink_path).unwrap();

        for name in [&file_name, &directory_name, &symlink_name] {
            let error = create_owned_listener(name, std::slice::from_ref(name), true, config)
                .await
                .expect_err("non-socket target must be protected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }

        std::fs::remove_file(&symlink_path).unwrap();
        std::fs::remove_dir(&directory_path).unwrap();
        std::fs::remove_file(&file_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn socket_root_preparation_creates_only_the_missing_final_directory() {
        let root = format!("/tmp/{}-root", unique_name("prepare"));
        let names = vec![
            format!("{root}/local.sock"),
            format!("{root}/internal.sock"),
        ];

        prepare_owned_socket_root(&names, true).unwrap();
        let metadata = std::fs::symlink_metadata(&root).unwrap();
        assert!(metadata.file_type().is_dir());
        assert!(!metadata.file_type().is_symlink());

        std::fs::remove_dir(&root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn socket_root_preparation_rejects_symlink_and_mismatched_targets() {
        use std::os::unix::fs::symlink;

        let real_root = format!("/tmp/{}-real", unique_name("prepare"));
        let link_root = format!("/tmp/{}-link", unique_name("prepare"));
        std::fs::create_dir(&real_root).unwrap();
        symlink(&real_root, &link_root).unwrap();
        let linked_names = vec![
            format!("{link_root}/local.sock"),
            format!("{link_root}/internal.sock"),
        ];
        assert_eq!(
            prepare_owned_socket_root(&linked_names, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            prepare_owned_socket_root(
                &[
                    "/tmp/a/local.sock".to_string(),
                    "/tmp/b/internal.sock".to_string()
                ],
                true,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied
        );

        std::fs::remove_file(&link_root).unwrap();
        std::fs::remove_dir(&real_root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_identity_never_removes_replacement_socket() {
        let name = unique_name("replacement-identity");
        let first = create_listener(&name).unwrap();
        let first_identity = capture_owned_socket(&name).unwrap();
        drop(first);
        std::fs::remove_file(normalize_name(&name)).unwrap();

        let replacement = create_listener(&name).unwrap();
        let replacement_identity = capture_owned_socket(&name).unwrap();
        assert_eq!(
            remove_owned_socket_path(&first_identity)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(std::fs::symlink_metadata(normalize_name(&name)).is_ok());

        drop(replacement);
        remove_owned_socket_path(&replacement_identity).unwrap();
    }
}
