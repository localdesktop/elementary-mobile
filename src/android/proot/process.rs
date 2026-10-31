use crate::android::utils::application_context::get_application_context;
use crate::core::cpuinfo::{kernel_enables_sve, GUEST_CPUINFO};
use std::ffi::CString;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use winit::platform::android::activity::AndroidApp;

pub type Log = Arc<dyn Fn(String) + Send + Sync>;

const SUPPORT_CHECK_BINARY: &str = "ld-linux-aarch64.so.1";

/// Runs a shell command inside the guest OS under PRoot.
///
/// - `command`: The shell command to execute (passed to `sh -c`).
/// - `user`: The user to run as. Defaults to `"root"` when `None`.
/// - `log`: Optional stdout line callback. When set, stdout is streamed line-by-line
///   to the callback. When `None`, stdout/stderr are captured.
pub struct GuestProcess {
    pub command: String,
    pub user: Option<String>,
    pub log: Option<Log>,
}

impl GuestProcess {
    fn ensure_support_probe_rootfs(android_app: &AndroidApp) -> Option<()> {
        let context = get_application_context();
        let probe_exec = context.data_dir.join(SUPPORT_CHECK_BINARY);

        let asset_name = CString::new(SUPPORT_CHECK_BINARY).ok()?;
        let mut asset = android_app.asset_manager().open(&asset_name)?;

        let mut bytes = Vec::with_capacity(asset.length());
        asset.read_to_end(&mut bytes).ok()?;
        fs::write(&probe_exec, bytes).ok()?;
        fs::set_permissions(&probe_exec, fs::Permissions::from_mode(0o755)).ok()?;

        Some(())
    }

    fn try_proot_probe(rootfs: &Path, guest_program: &str, args: &[&str]) -> bool {
        let context = get_application_context();
        let proot_loader = context.native_library_dir.join("libproot_loader.so");

        let mut process = Command::new(context.native_library_dir.join("libproot.so"));
        process
            .env("PROOT_LOADER", &proot_loader)
            .env("PROOT_TMP_DIR", &context.data_dir);

        process
            .arg("-r")
            .arg(rootfs)
            .arg("-w")
            .arg("/")
            .arg(guest_program)
            .args(args)
            .output()
            .map(|o| {
                log::info!(
                    "try_proot_probe rootfs={}, program={}, status={:?}, stdout: {}, stderr: {}",
                    rootfs.display(),
                    guest_program,
                    o.status.code(),
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                o.status.success()
            })
            .unwrap_or_else(|e| {
                log::info!(
                    "try_proot_probe rootfs={}, program={} error: {}",
                    rootfs.display(),
                    guest_program,
                    e
                );
                false
            })
    }

    pub fn is_supported(android_app: &AndroidApp) -> bool {
        let context = get_application_context();
        let supported = if Self::ensure_support_probe_rootfs(android_app).is_some() {
            Self::try_proot_probe(
                &context.data_dir,
                &format!("/{}", SUPPORT_CHECK_BINARY),
                &["--help"],
            )
        } else {
            log::info!("Support probe asset missing or could not be extracted");
            false
        };

        if !supported {
            log::error!("⚡️ Device Unsupported");
        }
        supported
    }

    pub fn run(self) -> Output {
        let context = get_application_context();
        let user = self.user.as_deref().unwrap_or("root");
        let rootfs = context.rootfs_dir.display().to_string();
        let home = if user == "root" {
            "/root".to_string()
        } else {
            format!("/home/{user}")
        };

        let mut process = Command::new(context.native_library_dir.join("libproot.so"));
        process
            .env(
                "PROOT_LOADER",
                context.native_library_dir.join("libproot_loader.so"),
            )
            .env("PROOT_TMP_DIR", &context.data_dir);

        process
            .arg("-r")
            .arg(&rootfs)
            .arg("-L")
            .arg("--link2symlink")
            .arg("--sysvipc")
            .arg("--kill-on-exit")
            .arg("--root-id")
            .arg("--bind=/dev")
            .arg("--bind=/proc")
            .arg("--bind=/sys")
            .arg(format!("--bind={rootfs}/tmp:/dev/shm"))
            // /dev/pts and /dev/ptmx are already covered by --bind=/dev above.
            // The explicit sub-binds added in commit 61d9079 were redundant and caused
            // proot to double-translate PTY ioctls (TIOCGPTN/TIOCSPTLCK/TIOCSWINSZ),
            // breaking terminal initialisation and keyboard arrow keys inside QTerminal.
            // We only need /dev/tty explicitly for processes that open it by path.
            .arg("--bind=/dev/tty:/dev/tty")
            // User data and settings live outside the OS so that an update replacing it keeps them.
            .arg(format!("--bind={}:/home", context.home_dir.display()))
            .arg(format!(
                "--bind={}:/etc/localdesktop",
                context.config_dir.display()
            ));

        if context.permission_all_files_access {
            process
                .arg("--bind=/sdcard:/android")
                .arg(format!("--bind=/sdcard:{home}/Android"));
        }

        process
            .arg("--bind=/dev/urandom:/dev/random")
            .arg("--bind=/proc/self/fd:/dev/fd")
            .arg("--bind=/proc/self/fd/0:/dev/stdin")
            .arg("--bind=/proc/self/fd/1:/dev/stdout")
            .arg("--bind=/proc/self/fd/2:/dev/stderr")
            .arg(format!("--bind={rootfs}/proc/.loadavg:/proc/loadavg"))
            .arg(format!("--bind={rootfs}/proc/.stat:/proc/stat"))
            .arg(format!("--bind={rootfs}/proc/.uptime:/proc/uptime"))
            .arg(format!("--bind={rootfs}/proc/.version:/proc/version"))
            .arg(format!("--bind={rootfs}/proc/.vmstat:/proc/vmstat"))
            .arg(format!("--bind={rootfs}/proc/.sysctl_entry_cap_last_cap:/proc/sys/kernel/cap_last_cap"))
            .arg(format!("--bind={rootfs}/proc/.sysctl_inotify_max_user_watches:/proc/sys/fs/inotify/max_user_watches"))
            .arg(format!("--bind={rootfs}/sys/.empty:/sys/fs/selinux"));

        if context.rootfs_dir.join(GUEST_CPUINFO).is_file() {
            process.arg(format!("--bind={rootfs}/{GUEST_CPUINFO}:/proc/cpuinfo"));
        }

        // env vars
        process.arg("/usr/bin/env").arg("-i");
        process.arg(format!("HOME={home}"));
        if let Some(timezone) = &context.timezone {
            process.arg(format!("TZ={timezone}"));
        }
        process
            .arg("LANG=C.UTF-8")
            .arg("TERM=xterm-256color")
            .arg("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/usr/local/games:/usr/games:/system/bin:/system/xbin")
            .arg("TMPDIR=/tmp")
            .arg(format!("USER={}", user))
            .arg(format!("LOGNAME={}", user));
        // See `core::cpuinfo`: llvmpipe's optimised shaders still crash Gala on these devices.
        if !kernel_enables_sve(&std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default()) {
            process.arg("GALLIVM_PERF=nopt");
        }

        // user shell
        if user == "root" {
            process.arg("sh");
        } else {
            process
                .arg("runuser")
                .arg("--pty")
                .arg("-u")
                .arg(user)
                .arg("--")
                .arg("sh");
        }

        process.arg("-c").arg(&self.command);

        if let Some(log) = self.log {
            let mut child = process
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()
                .expect("Failed to run command");

            let reader = BufReader::new(child.stdout.take().unwrap());
            for line in reader.lines() {
                let line = line.unwrap();
                log(line);
            }

            child
                .wait_with_output()
                .expect("Failed to wait for command")
        } else {
            process.output().expect("Failed to run command")
        }
    }
}
