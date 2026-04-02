use std::io;

use interprocess::local_socket::{
    GenericFilePath, ListenerOptions, ToFsName, traits::tokio::Stream as _,
    tokio::{Listener, Stream},
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

pub fn listener_options(name: &str) -> io::Result<ListenerOptions<'_>> {
    Ok(ListenerOptions::new().name(to_name(name)?))
}

pub async fn connect(name: &str) -> io::Result<Stream> {
    Stream::connect(to_name(name)?).await
}

pub fn create_listener(name: &str) -> io::Result<Listener> {
    listener_options(name)?.create_tokio()
}
