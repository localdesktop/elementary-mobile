use crate::android::utils::application_context::get_application_context;

/// Persist the host window geometry and UI scale for the guest session.
///
/// The file lives in the proot-visible `/tmp`; elementary-mobile-session reads it to size the
/// rootful Xwayland window and pick the desktop's scaling factor.
pub fn write_guest_output_state(width: i32, height: i32, scale: i32) {
    if width <= 0 || height <= 0 || scale <= 0 {
        return;
    }

    let path = get_application_context()
        .rootfs_dir
        .join("tmp/localdesktop-output");
    let content =
        format!("LOCALDESKTOP_OUTPUT_MODE={width}x{height}\nLOCALDESKTOP_OUTPUT_SCALE={scale}\n");
    if let Err(error) = std::fs::write(&path, content) {
        log::warn!(
            "Failed to write guest output state to {}: {error}",
            path.display()
        );
    }
}

