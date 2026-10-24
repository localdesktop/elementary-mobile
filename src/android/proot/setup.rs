use super::process::GuestProcess;
use crate::{
    android::{
        accessibility::{send_user_event, AppUserEvent},
        app::build::PolarBearBackend,
        backend::{
            wayland::{Compositor, TouchMode, WaylandBackend},
            webview::{ErrorVariant, WebviewBackend},
        },
        utils::application_context::get_application_context,
        utils::ndk::{long_press_timeout_ms, run_in_jvm, scale_factor, touch_slop_px},
    },
    core::{
        cpuinfo::{guest_cpuinfo, GUEST_CPUINFO},
        rootfs::{self, RootfsManifest, ROOTFS_ARCHIVE, ROOTFS_MANIFEST},
    },
};
use jni::objects::{JObject, JString};
use pathdiff::diff_paths;
use smithay::utils::Clock;
use std::{
    ffi::CString,
    fs,
    io::{BufReader, ErrorKind, Read},
    os::unix::fs::{symlink, PermissionsExt},
    path::Path,
    process,
    sync::{
        mpsc::{self, Sender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};
use winit::platform::android::activity::AndroidApp;

#[derive(Debug)]
pub enum SetupMessage {
    Progress(String),
    Error(String),
}

pub struct SetupOptions {
    pub android_app: AndroidApp,
    pub mpsc_sender: Sender<SetupMessage>,
    /// The OS this APK carries.
    pub manifest: RootfsManifest,
}

/// Setup is a process that should be done **only once** when the user installed the app.
/// The setup process consists of several stages.
/// Each stage is a function that takes the `SetupOptions` and returns a `StageOutput`.
type SetupStage = Box<dyn Fn(&SetupOptions) -> StageOutput + Send>;

/// Each stage should indicate whether the associated task is done previously or not.
/// Thus, it should return a finished status if the task is done, so that the setup process can move on to the next stage.
/// Otherwise, it should return a `JoinHandle`, so that the setup process can wait for the task to finish, but not block the main thread so that the setup progress can be reported to the user.
///
/// For coding agents: READ THIS BEFORE ADDING WORK HERE.
/// - Heavy/long work belongs inside the spawned thread of a returned `Some(JoinHandle)`, so it runs once at install and surfaces as setup progress.
/// - Simple/light tasks or important settings that must be run every launch can be done inline on the `None` path.
type StageOutput = Option<JoinHandle<()>>;

/// Public resolvers used when Android does not tell us its own.
const FALLBACK_NAMESERVERS: &[&str] = &["1.1.1.1", "8.8.8.8"];

fn read_manifest(android_app: &AndroidApp) -> Option<RootfsManifest> {
    let name = CString::new(ROOTFS_MANIFEST).ok()?;
    let mut asset = android_app.asset_manager().open(&name)?;
    let mut json = Vec::new();
    asset.read_to_end(&mut json).ok()?;
    RootfsManifest::parse(&json)
        .map_err(|e| log::error!("Invalid {ROOTFS_MANIFEST}: {e}"))
        .ok()
}

/// Unpack the OS bundled in the APK: on first launch, and after an update that carries a new one.
fn setup_rootfs(options: &SetupOptions) -> StageOutput {
    let context = get_application_context();
    let rootfs_dir = context.rootfs_dir.clone();
    let manifest = options.manifest.clone();

    if rootfs::is_installed(&rootfs_dir, &manifest) {
        return None;
    }

    let android_app = options.android_app.clone();
    let mpsc_sender = options.mpsc_sender.clone();
    Some(thread::spawn(move || {
        let action = if rootfs_dir.exists() {
            "Updating"
        } else {
            "Unpacking"
        };
        let total = manifest.uncompressed_size.max(1);
        let total_mb = total as f64 / 1024.0 / 1024.0;
        mpsc_sender
            .send(SetupMessage::Progress(format!(
                "{action} elementary OS ({total_mb:.0} MB)..."
            )))
            .unwrap_or(());

        let name = CString::new(ROOTFS_ARCHIVE).expect("asset name has no NUL bytes");
        let asset = android_app
            .asset_manager()
            .open(&name)
            .unwrap_or_else(|| panic!("This APK does not contain {ROOTFS_ARCHIVE}"));

        let mut last_percent = None;
        let result = rootfs::install(
            BufReader::with_capacity(1 << 20, asset),
            &rootfs_dir,
            &manifest,
            |read| {
                let percent = (read * 100 / total).min(100);
                if last_percent != Some(percent) {
                    last_percent = Some(percent);
                    mpsc_sender
                        .send(SetupMessage::Progress(format!(
                            "{action} elementary OS... {percent}% ({:.0} MB / {total_mb:.0} MB)",
                            read as f64 / 1024.0 / 1024.0
                        )))
                        .unwrap_or(());
                }
            },
        );

        if let Err(error) = result {
            let hint = if error.kind() == ErrorKind::StorageFull {
                format!(
                    " elementary OS needs about {total_mb:.0} MB of free storage. Free up some space and open the app again."
                )
            } else {
                String::new()
            };
            let message = format!("Failed to unpack elementary OS: {error}.{hint}");
            mpsc_sender
                .send(SetupMessage::Error(message.clone()))
                .unwrap_or(());
            panic!("{message}");
        }
    }))
}

fn simulate_linux_sysdata_stage(options: &SetupOptions) -> StageOutput {
    let fs_root = get_application_context().rootfs_dir;
    let mpsc_sender = options.mpsc_sender.clone();

    if !fs_root.join("proc/.version").exists() {
        return Some(thread::spawn(move || {
            mpsc_sender
                .send(SetupMessage::Progress(
                    "Simulating Linux system data...".to_string(),
                ))
                .expect(&format!("Failed to send log message"));

            // Create necessary directories - don't fail if they already exist
            let _ = fs::create_dir_all(fs_root.join("proc"));
            let _ = fs::create_dir_all(fs_root.join("sys"));
            let _ = fs::create_dir_all(fs_root.join("sys/.empty"));

            // Set permissions - only try to set permissions if we're on Unix and have the capability
            #[cfg(unix)]
            {
                // Try to set permissions, but don't fail if we can't
                let _ =
                    fs::set_permissions(fs_root.join("proc"), fs::Permissions::from_mode(0o700));
                let _ = fs::set_permissions(fs_root.join("sys"), fs::Permissions::from_mode(0o700));
                let _ = fs::set_permissions(
                    fs_root.join("sys/.empty"),
                    fs::Permissions::from_mode(0o700),
                );
            }

            // Create fake proc files
            let proc_files = [
                    ("proc/.loadavg", "0.12 0.07 0.02 2/165 765\n"),
                    ("proc/.stat", "cpu  1957 0 2877 93280 262 342 254 87 0 0\ncpu0 31 0 226 12027 82 10 4 9 0 0\n"),
                    ("proc/.uptime", "124.08 932.80\n"),
                    ("proc/.version", "Linux version 6.2.1 (proot@termux) (gcc (GCC) 12.2.1 20230201, GNU ld (GNU Binutils) 2.40) #1 SMP PREEMPT_DYNAMIC Wed, 01 Mar 2023 00:00:00 +0000\n"),
                    ("proc/.vmstat", "nr_free_pages 1743136\nnr_zone_inactive_anon 179281\nnr_zone_active_anon 7183\n"),
                    ("proc/.sysctl_entry_cap_last_cap", "40\n"),
                    ("proc/.sysctl_inotify_max_user_watches", "4096\n"),
                ];

            for (path, content) in proc_files {
                let _ = fs::write(fs_root.join(path), content)
                    .expect(&format!("Permission denied while writing to {}", path));
            }
        }));
    }
    None
}

/// Create the directories bind-mounted into the guest, and the user's home on first launch.
fn setup_home(options: &SetupOptions) -> StageOutput {
    let context = get_application_context();
    fs::create_dir_all(&context.home_dir).expect("Failed to create the home directory");
    fs::create_dir_all(&context.config_dir).expect("Failed to create the config directory");

    let username = context.local_config.user.username;
    if username != options.manifest.username {
        log::warn!(
            "Configured user {username} is not the one created in the image ({}); make sure it exists",
            options.manifest.username
        );
    }
    if username != "root" {
        match rootfs::seed_home(&context.rootfs_dir, &context.home_dir, &username) {
            Ok(true) => log::info!("Created /home/{username}"),
            Ok(false) => {}
            Err(error) => log::error!("Failed to create /home/{username}: {error}"),
        }
    }
    None
}

fn setup_machine_id(_: &SetupOptions) -> StageOutput {
    let fs_root = get_application_context().rootfs_dir;
    let machine_id = fs_root.join("etc/machine-id");

    let existing = fs::read_to_string(&machine_id).unwrap_or_default();
    if !is_valid_machine_id(&existing) {
        if let Some(parent) = machine_id.parent() {
            fs::create_dir_all(parent).expect("Failed to create /etc for machine-id");
        }

        let _ = fs::set_permissions(&machine_id, fs::Permissions::from_mode(0o644));
        fs::write(&machine_id, format!("{}\n", generate_machine_id()))
            .expect("Failed to write machine-id");
        let _ = fs::set_permissions(&machine_id, fs::Permissions::from_mode(0o444));
        log::info!("Seeded guest /etc/machine-id");
    }

    let dbus_dir = fs_root.join("var/lib/dbus");
    fs::create_dir_all(&dbus_dir).expect("Failed to create /var/lib/dbus");
    let dbus_machine_id = dbus_dir.join("machine-id");
    match fs::symlink_metadata(&dbus_machine_id) {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {
            symlink("/etc/machine-id", &dbus_machine_id)
                .expect("Failed to symlink /var/lib/dbus/machine-id");
        }
        Err(err) => panic!("Failed to inspect /var/lib/dbus/machine-id: {}", err),
    }

    None
}

fn is_valid_machine_id(value: &str) -> bool {
    let value = value.trim();
    value.len() == 32
        && value.chars().all(|c| c.is_ascii_hexdigit())
        && value.chars().any(|c| c != '0')
}

fn generate_machine_id() -> String {
    if let Ok(uuid) = fs::read_to_string("/proc/sys/kernel/random/uuid") {
        let id = uuid.trim().replace('-', "").to_ascii_lowercase();
        if is_valid_machine_id(&id) {
            return id;
        }
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{:016x}{:016x}", nanos as u64, process::id() as u64)
}

/// Point the guest at Android's DNS servers and the current timezone. Refreshed every launch,
/// since both change as the phone moves between networks and places.
fn setup_network_and_time(options: &SetupOptions) -> StageOutput {
    let context = get_application_context();
    let etc = context.rootfs_dir.join("etc");

    let mut nameservers = android_dns_servers(&options.android_app);
    if nameservers.is_empty() {
        nameservers = FALLBACK_NAMESERVERS.iter().map(|s| s.to_string()).collect();
    }
    let resolv_conf: String = nameservers
        .iter()
        .map(|server| format!("nameserver {server}\n"))
        .collect();
    replace_file(&etc.join("resolv.conf"), &resolv_conf);
    replace_file(
        &etc.join("hosts"),
        "127.0.0.1 localhost\n::1 localhost ip6-localhost ip6-loopback\n",
    );

    if let Some(timezone) = &context.timezone {
        if context
            .rootfs_dir
            .join("usr/share/zoneinfo")
            .join(timezone)
            .is_file()
        {
            replace_file(&etc.join("timezone"), &format!("{timezone}\n"));
            let localtime = etc.join("localtime");
            let _ = fs::remove_file(&localtime);
            // Relative, so the link also resolves when read from outside proot.
            let _ = symlink(format!("../usr/share/zoneinfo/{timezone}"), &localtime);
        }
    }
    None
}

/// Give the guest a `/proc/cpuinfo` that keeps llvmpipe from using CPU features the kernel turned
/// off (see `core::cpuinfo`). Every launch, so it always describes this device.
fn setup_cpuinfo(_: &SetupOptions) -> StageOutput {
    let path = get_application_context().rootfs_dir.join(GUEST_CPUINFO);
    match fs::read_to_string("/proc/cpuinfo") {
        Ok(host) => replace_file(&path, &guest_cpuinfo(&host)),
        Err(error) => log::warn!("Failed to read /proc/cpuinfo: {error}"),
    }
    None
}

/// Write `contents` to `path`, replacing a symlink or read-only file left there by the image.
fn replace_file(path: &Path, contents: &str) {
    let _ = fs::remove_file(path);
    if let Err(error) = fs::write(path, contents) {
        log::warn!("Failed to write {}: {error}", path.display());
    }
}

/// The DNS servers of Android's active network, as IP address strings.
fn android_dns_servers(android_app: &AndroidApp) -> Vec<String> {
    fn query(env: &mut jni::JNIEnv, app: &AndroidApp) -> Option<Vec<String>> {
            let activity =
                unsafe { JObject::from_raw(app.activity_as_ptr() as *mut jni::sys::_jobject) };
            let service = env.new_string("connectivity").ok()?;
            let manager = env
                .call_method(
                    activity,
                    "getSystemService",
                    "(Ljava/lang/String;)Ljava/lang/Object;",
                    &[(&service).into()],
                )
                .and_then(|it| it.l())
                .ok()?;
            let network = env
                .call_method(&manager, "getActiveNetwork", "()Landroid/net/Network;", &[])
                .and_then(|it| it.l())
                .ok()
                .filter(|it| !it.is_null())?;
            let properties = env
                .call_method(
                    &manager,
                    "getLinkProperties",
                    "(Landroid/net/Network;)Landroid/net/LinkProperties;",
                    &[(&network).into()],
                )
                .and_then(|it| it.l())
                .ok()
                .filter(|it| !it.is_null())?;
            let servers = env
                .call_method(properties, "getDnsServers", "()Ljava/util/List;", &[])
                .and_then(|it| it.l())
                .ok()?;
            let count = env
                .call_method(&servers, "size", "()I", &[])
                .and_then(|it| it.i())
                .ok()?;

            let mut result = Vec::new();
            for i in 0..count {
                let address = env
                    .call_method(&servers, "get", "(I)Ljava/lang/Object;", &[i.into()])
                    .and_then(|it| it.l())
                    .ok()?;
                let host = env
                    .call_method(address, "getHostAddress", "()Ljava/lang/String;", &[])
                    .and_then(|it| it.l())
                    .ok()?;
                let host: String = env.get_string(&JString::from(host)).ok()?.into();
                // glibc does not understand scoped IPv6 addresses such as fe80::1%wlan0.
                if !host.contains('%') {
                    result.push(host);
                }
            }
            Some(result)
    }

    run_in_jvm(
        |env, app| {
            query(env, app).unwrap_or_else(|| {
                // A call that threw leaves the exception pending; later JNI calls would fail.
                let _ = env.exception_clear();
                Vec::new()
            })
        },
        android_app.clone(),
    )
}

fn fix_xkb_symlink(options: &SetupOptions) -> StageOutput {
    let fs_root = get_application_context().rootfs_dir;
    let xkb_path = fs_root.join("usr/share/X11/xkb");
    let mpsc_sender = options.mpsc_sender.clone();

    if let Ok(meta) = fs::symlink_metadata(&xkb_path) {
        if meta.file_type().is_symlink() {
            if let Ok(target) = fs::read_link(&xkb_path) {
                if target.is_absolute() {
                    log::info!(
                        "Absolute symlink target detected: {} -> {}. This is a problem because libxkbcommon is loaded in NDK, whose / is not the guest root!",
                        xkb_path.display(),
                        target.display()
                    );
                    let xkb_inside = Path::new("/usr/share/X11/xkb");
                    let rel_target = diff_paths(&target, xkb_inside.parent().unwrap())
                        .unwrap_or_else(|| target.to_path_buf());
                    log::info!(
                        "Fixing with new relative symlink: {} -> {}",
                        xkb_path.display(),
                        rel_target.display()
                    );
                    let _ = fs::remove_file(&xkb_path);
                    if let Err(e) = symlink(&rel_target, &xkb_path) {
                        mpsc_sender
                            .send(SetupMessage::Error(format!(
                                "Failed to create relative symlink for xkb: {}",
                                e
                            )))
                            .unwrap_or(());
                    }
                }
            }
        }
    }
    None
}

pub fn build_wayland_backend(android_app: AndroidApp) -> WaylandBackend {
    WaylandBackend {
        compositor: Compositor::build().expect("Failed to build compositor"),
        graphic_renderer: None,
        clock: Clock::new(),
        key_counter: 0,
        guest_scale_factor: scale_factor(&android_app),
        touch_points: std::collections::HashMap::new(),
        scroll_centroid: None,
        touch_mode: TouchMode::Undecided,
        touch_down_position: None,
        touch_down_time: None,
        touch_slop_px: touch_slop_px(&android_app),
        long_press_timeout_ms: long_press_timeout_ms(&android_app),
        pointer_pressed: false,
        touch_fingers: 0,
        cursor_position: (0.0, 0.0).into(),
        external_buttons: 0,
        android_app,
    }
}

pub fn setup(android_app: AndroidApp) -> PolarBearBackend {
    let (sender, receiver) = mpsc::channel();
    let progress = Arc::new(Mutex::new(0));

    if GuestProcess::is_supported(&android_app) {
        sender
            .send(SetupMessage::Progress(
                "✅ Your device is supported!".to_string(),
            ))
            .unwrap_or(());
    } else {
        log::info!("PRoot support check failed, showing Device Unsupported page");
        return PolarBearBackend::WebView(WebviewBackend {
            socket_port: 0,
            progress,
            error: ErrorVariant::Unsupported,
        });
    }

    let Some(manifest) = read_manifest(&android_app) else {
        log::error!(
            "{ROOTFS_MANIFEST} is missing from the APK; build it with scripts/build-rootfs.sh"
        );
        return PolarBearBackend::WebView(WebviewBackend {
            socket_port: 0,
            progress,
            error: ErrorVariant::MissingOs,
        });
    };

    if let Err(error) = rootfs::remove_partial(&get_application_context().rootfs_dir) {
        log::warn!("Failed to clean up an interrupted install: {error}");
    }

    let options = SetupOptions {
        android_app: android_app.clone(),
        mpsc_sender: sender.clone(),
        manifest,
    };

    let stages: Vec<SetupStage> = vec![
        Box::new(setup_rootfs),                 // Step 1. Unpack the OS bundled in the APK
        Box::new(simulate_linux_sysdata_stage), // Step 2. Simulate Linux system data
        Box::new(setup_home),                   // Step 3. Persistent /home and /etc/localdesktop
        Box::new(setup_machine_id),             // Step 4. Seed /etc/machine-id for D-Bus clients
        Box::new(setup_network_and_time),       // Step 5. DNS servers and timezone from Android
        Box::new(setup_cpuinfo),                // Step 6. /proc/cpuinfo that llvmpipe can trust
        Box::new(fix_xkb_symlink),              // Step 7. Fix xkb symlink
    ];

    let handle_stage_error = |e: Box<dyn std::any::Any + Send>, sender: &Sender<SetupMessage>| {
        let error_msg = if let Some(e) = e.downcast_ref::<String>() {
            format!("Stage execution failed: {}", e)
        } else if let Some(e) = e.downcast_ref::<&str>() {
            format!("Stage execution failed: {}", e)
        } else {
            "Stage execution failed: Unknown error".to_string()
        };
        sender
            .send(SetupMessage::Error(error_msg.clone()))
            .unwrap_or(());
    };

    let fully_installed = 'outer: loop {
        for (i, stage) in stages.iter().enumerate() {
            if let Some(handle) = stage(&options) {
                let progress_clone = progress.clone();
                let sender_clone = sender.clone();
                thread::spawn(move || {
                    let progress = progress_clone;
                    let progress_value = ((i) as u16 * 100 / stages.len() as u16) as u16;
                    *progress.lock().unwrap() = progress_value;

                    // Wait for the current stage to finish
                    if let Err(e) = handle.join() {
                        handle_stage_error(e, &sender_clone);
                        return;
                    }

                    // Process the remaining stages in the same loop
                    for (j, next_stage) in stages.iter().enumerate().skip(i + 1) {
                        let progress_value = ((j) as u16 * 100 / stages.len() as u16) as u16;
                        *progress.lock().unwrap() = progress_value;
                        if let Some(next_handle) = next_stage(&options) {
                            if let Err(e) = next_handle.join() {
                                handle_stage_error(e, &sender_clone);
                                return;
                            }

                            // Increment progress and send it
                            let next_progress_value =
                                ((j + 1) as u16 * 100 / stages.len() as u16) as u16;
                            *progress.lock().unwrap() = next_progress_value;
                        }
                    }

                    // Everything is in place: go straight to the desktop instead of asking the
                    // user to restart the app.
                    *progress.lock().unwrap() = 100;
                    sender_clone
                        .send(SetupMessage::Progress(
                            "Starting elementary OS...".to_string(),
                        ))
                        .unwrap_or(());
                    send_user_event(AppUserEvent::SetupFinished);
                });

                // Setup is still running in the background, but we need to return control
                // so that the main thread can continue to report progress to the user
                break 'outer false;
            }
        }

        // All stages were done previously, no need to wait for anything
        break 'outer true;
    };

    if fully_installed {
        PolarBearBackend::Wayland(build_wayland_backend(android_app))
    } else {
        PolarBearBackend::WebView(WebviewBackend::build(receiver, progress))
    }
}
