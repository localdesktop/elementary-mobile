use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where things live, relative to the app's files directory (`Context.getFilesDir()`).
///
/// The OS is kept apart from everything that must survive an APK update, because an update that
/// carries a new OS replaces `ROOTFS_DIR` wholesale (see `core::rootfs`).
pub const ROOTFS_DIR: &str = "rootfs";
/// Bind-mounted over the guest's `/home`.
pub const HOME_DIR: &str = "home";
/// Bind-mounted over the guest's `/etc/localdesktop`, so `CONFIG_FILE` survives OS updates.
pub const CONFIG_DIR: &str = "config";

/// The desktop user created in the bundled image (`ARG USERNAME` in rootfs/Dockerfile).
pub const DEFAULT_USERNAME: &str = "elementary";

pub const WAYLAND_SOCKET_NAME: &str = "wayland-0";

pub const MAX_PANEL_LOG_ENTRIES: usize = 100;

/// Crash reporting is off unless a DSN is supplied when building (`SENTRY_DSN=... x build ...`).
/// This fork must not report into upstream Local Desktop's Sentry project.
pub const SENTRY_DSN: Option<&str> = option_env!("SENTRY_DSN");

/// PipeWire runtime path as seen from inside the proot guest.
pub const PIPEWIRE_GUEST_RUNTIME_DIR: &str = "/tmp";

/// PipeWire-Pulse socket as seen from inside the proot guest.
pub const PULSE_GUEST_SERVER: &str = "unix:/tmp/pulse/native";

/// The smallest desktop, in logical pixels, that the panel, the dock and elementary's windows fit.
const MIN_DESKTOP_SHORT_SIDE: i32 = 540;
const MIN_DESKTOP_LONG_SIDE: i32 = 900;
/// How much denser than Android's own UI the desktop may get to reach that size.
const MAX_DENSITY_RATIO: f64 = 2.0;

/// Whole-number UI scale for the desktop from Android's density scale (dpi / 160) and the window
/// size in physical pixels.
///
/// Rounds down unless the density is close to the next step: a 420 dpi phone (2.625) gets 2x, a
/// 480 dpi one 3x. Rounding to nearest gives 3x at 420 dpi, which leaves a desktop barely 360
/// logical pixels wide in portrait. Then it steps down while the desktop would be smaller than
/// `MIN_DESKTOP_SHORT_SIDE` x `MIN_DESKTOP_LONG_SIDE`, as long as text stays at least half the
/// size of Android's: dense screens that are small in pixels, like a foldable's cover screen (1116
/// pixels wide at 520 dpi, so 372 at 3x), get 2x.
pub fn guest_ui_scale(scale_factor: f64, width: i32, height: i32) -> i32 {
    let (short_side, long_side) = (width.min(height), width.max(height));
    let mut scale = ((scale_factor + 0.25).floor() as i32).max(1);
    while scale > 1
        && (short_side / scale < MIN_DESKTOP_SHORT_SIDE || long_side / scale < MIN_DESKTOP_LONG_SIDE)
        && scale_factor / f64::from(scale - 1) <= MAX_DENSITY_RATIO
    {
        scale -= 1;
    }
    scale
}


/// Make sure the config keys are all lowercase, and config values are single-line. Use \n for multi-line config values if needed
/// If a key exists multiple time, the first entry is applied
/// If a `try_` config exsists multiple time, the last entry is applied
/// But in general, it is **invalid** to have duplicated config keys inside a TOML file
/// The file name inside `CONFIG_DIR`, edited from the guest as /etc/localdesktop/localdesktop.toml.
pub const CONFIG_FILE: &str = "localdesktop.toml";

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct LocalConfig {
    #[serde(default)]
    pub user: UserConfig,

    /// What happens if we don't assign this `#[serde(default)]` attribute?
    /// The answer: If the user omits the `[command]` group, the WHOLE config fails to parse
    /// => The default `[user]` group is applied (with the default username) even if the `[user]` settings are completely valid.
    /// => So make sure that every config group has a `#[serde(default)]` attribute to avoid invalid sections breaking unrelated parts of the config.
    #[serde(default)]
    pub command: CommandConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserConfig {
    pub username: String,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            username: DEFAULT_USERNAME.to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CommandConfig {
    /// Runs inside proot as `[user] username` once the compositor is up, and should keep running
    /// for as long as the desktop does.
    #[serde(default = "default_launch")]
    pub launch: String,
}

/// Starts elementary OS; see rootfs/overlay/usr/local/bin/elementary-mobile-session.
fn default_launch() -> String {
    format!("export PIPEWIRE_RUNTIME_DIR={PIPEWIRE_GUEST_RUNTIME_DIR} PULSE_SERVER={PULSE_GUEST_SERVER} WAYLAND_DISPLAY=/tmp/{WAYLAND_SOCKET_NAME}; exec /usr/local/bin/elementary-mobile-session 2>&1")
}

impl Default for CommandConfig {
    fn default() -> Self {
        Self {
            launch: default_launch(),
        }
    }
}

/// This function does 2 major tasks:
/// - Read config from `CONFIG_FILE`, and override configs with their `try_*` versions, and return the configs line by line
/// - Write back to the config file, with `try_*` configs commented out
///
/// **Important**: As each call to this function will comment out the `try_*` config, it is **non-idempotent**.
fn process_config_file(full_config_path: String) -> Vec<String> {
    let mut write_back_lines: Vec<String> = vec![];
    let mut effective_config: Vec<String> = vec![];

    if let Ok(content) = fs::read_to_string(&full_config_path) {
        for line in content.lines() {
            let trimmed = line.trim();

            if let Some((key, value)) = trimmed.split_once('=') {
                let key = key.trim();
                let value = value.trim();

                if key.starts_with("try_") {
                    // Comment out the `try_*` configs
                    write_back_lines.push(format!("# {}", trimmed));

                    // Prefer the `try_*` configs
                    let actual_key = key.trim_start_matches("try_");
                    if let Some(line_index) = effective_config
                        .iter()
                        .position(|line| line.starts_with(&format!("{}=", actual_key)))
                    {
                        // Config exists, overriding
                        effective_config[line_index] = format!("{}={}", actual_key, value);
                    } else {
                        // Config does not exist, appending
                        effective_config.push(format!("{}={}", actual_key, value));
                        // Make sure there are no spaces around = so that the check existing key logic works
                    }
                } else {
                    // Keep the config as is
                    write_back_lines.push(trimmed.to_string());

                    if effective_config
                        .iter()
                        .any(|line| line.starts_with(&format!("{}=", key)))
                    {
                        // If already overridden by try_ version, skip inserting
                    } else {
                        // Config does not exist, appending
                        effective_config.push(format!("{}={}", key, value)); // Make sure there are no spaces around = so that the check existing key logic works
                    }
                }
            } else {
                // Keep the line as is
                write_back_lines.push(trimmed.to_string());
                effective_config.push(trimmed.to_string());
            }
        }

        // Rewrite config with try_* lines commented out
        let _ = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&full_config_path)
            .and_then(|mut file| {
                for line in &write_back_lines {
                    writeln!(file, "{}", line)?;
                }
                Ok(())
            });
    }

    // Convert effective config back to lines
    effective_config
}

pub fn parse_config(full_config_path: String) -> LocalConfig {
    let lines = process_config_file(full_config_path);
    let content = lines.join("\n");
    if let Ok(config) = toml::from_str::<LocalConfig>(&content) {
        return config;
    }
    // Config malformed, use the default config and the user can modify it again
    let default_config = LocalConfig::default();
    default_config
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn with_config_file(content: &str, f: impl Fn(String)) -> () {
        let dir = tempdir().unwrap();
        let base_dir = dir.path().to_str().unwrap();
        let path = format!("{}/etc/localdesktop", base_dir);
        fs::create_dir_all(&path).unwrap();
        let file_path = format!("{}/localdesktop.toml", path);
        fs::write(&file_path, content).unwrap();
        f(file_path)
    }

    #[test]
    fn should_handle_configs_without_try() {
        with_config_file(
            r#"
                [user]
                username = "alice"

                [command]
                launch = "launch-cmd"
            "#,
            |full_config_path| {
                let config = parse_config(full_config_path);
                assert_eq!(config.user.username, "alice");
                assert_eq!(config.command.launch, "launch-cmd");
            },
        );
    }

    #[test]
    fn should_handle_configs_with_try() {
        with_config_file(
            r#"
                [user]
                username = "root"
                try_username = "testuser"

                [command]
                launch = "launch-cmd"
                try_launch = "try-launch"
            "#,
            |full_config_path| {
                let config = parse_config(full_config_path);
                assert_eq!(config.user.username, "testuser");
                assert_eq!(config.command.launch, "try-launch")
            },
        );
    }

    #[test]
    fn should_comment_out_try_configs() {
        with_config_file(
            r#"
                username = "root"
                try_username = "commented"

                check = "normal"
                try_check = "try"
            "#,
            |full_config_path| {
                let _ = parse_config(full_config_path.clone()); // This triggers rewriting the config file
                let content = fs::read_to_string(full_config_path).unwrap();

                assert!(
                    content.contains("# try_username = \"commented\""),
                    "❌ `try_username` is not commented out after being applied"
                );
                assert!(
                    content.contains("# try_check = \"try\""),
                    "❌ `try_check` is not commented out after being  applied"
                );
            },
        );
    }

    #[test]
    fn picks_whole_number_scales() {
        let (w, h) = (3840, 2400);
        assert_eq!(guest_ui_scale(1.0, w, h), 1); // mdpi
        assert_eq!(guest_ui_scale(1.5, w, h), 1); // hdpi
        assert_eq!(guest_ui_scale(2.0, w, h), 2); // xhdpi
        assert_eq!(guest_ui_scale(2.625, w, h), 2); // 420 dpi
        assert_eq!(guest_ui_scale(2.75, w, h), 3); // 440 dpi
        assert_eq!(guest_ui_scale(3.5, w, h), 3); // 560 dpi
        assert_eq!(guest_ui_scale(0.75, w, h), 1);
    }

    #[test]
    fn keeps_the_desktop_big_enough_to_use() {
        // OnePlus Open at 520 dpi: cover screen and inner screen.
        assert_eq!(guest_ui_scale(3.25, 1116, 2484), 2);
        assert_eq!(guest_ui_scale(3.25, 2268, 2440), 2);
        // Phones keep 2x in either orientation.
        assert_eq!(guest_ui_scale(2.625, 1080, 1920), 2);
        assert_eq!(guest_ui_scale(2.625, 2400, 1080), 2);
        assert_eq!(guest_ui_scale(3.5, 1440, 3120), 2);
        // A tablet keeps its density's scale.
        assert_eq!(guest_ui_scale(2.0, 1600, 2560), 2);
        // Never below 1x.
        assert_eq!(guest_ui_scale(2.0, 600, 800), 1);
    }

    #[test]
    fn keeps_text_readable_on_small_screens() {
        // Galaxy Z Fold5 cover screen at 400 dpi: 1x would fit more, with text 2.5 times smaller
        // than Android's.
        assert_eq!(guest_ui_scale(2.5, 904, 2316), 2);
    }
}
