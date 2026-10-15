use crate::android::utils::application_context::get_application_context;
use crate::core::config;
use smithay::reexports::wayland_server::ListeningSocket;
use std::error::Error;

pub fn bind_socket() -> Result<ListeningSocket, Box<dyn Error>> {
    let socket_path = get_application_context()
        .rootfs_dir
        .join("tmp")
        .join(config::WAYLAND_SOCKET_NAME);
    let listener = ListeningSocket::bind_absolute(socket_path)?;
    Ok(listener)
}
