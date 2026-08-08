use std::io;

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

pub fn remove_socket_path(name: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        let _ = name;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let path = normalize_name(name);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
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
}
