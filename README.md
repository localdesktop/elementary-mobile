# elementary Mobile

elementary OS on your Android device, offline. Install the APK, tap the icon, and you get the
Pantheon desktop with Files, Terminal, Code, Web and System Settings. Nothing is downloaded:
the whole OS ships inside the APK.

This is a fork of [Local Desktop](https://github.com/localdesktop/localdesktop.github.io), which
installs Arch Linux with Xfce on first launch. elementary Mobile keeps Local Desktop's engine (a
Wayland compositor in the Android NDK plus proot) and replaces what runs on it.

**Note**: like Local Desktop, this expects a large enough display (a tablet, a foldable or DeX),
ideally with a keyboard and mouse. It brings a desktop to Android; it does not turn the desktop
into a phone UI.

## How it works

1. The APK carries an elementary OS 8 root filesystem as a ~190 MB xz archive
   (`elementary-os-rootfs.tar.xz`, ~1 GB unpacked).
2. On first launch the app streams it straight out of the APK into its private storage, showing
   progress, then opens the desktop. No restart, no network.
3. proot runs the OS in a chroot-like environment, as the user `elementary`.
4. The app's built-in Wayland compositor shows one rootful Xwayland window, and Gala (elementary's
   window manager, which is X11-only when it cannot drive the display itself) runs the Pantheon
   Classic session inside it: Wingpanel, the Dock, notifications and the apps.

Rendering is done on the CPU (Mesa llvmpipe): Android's GPU drivers cannot be used from a glibc
userland.

### Updates

The OS is never updated in place. A newer APK that carries a different archive replaces the OS on
its next launch. Your files survive because they are kept outside the OS:

| On the device (`/data/data/app.elementarymobile/files/`) | In elementary OS            |
| -------------------------------------------------------- | --------------------------- |
| `rootfs/`                                                | `/` (replaced on updates)   |
| `home/`                                                  | `/home` (kept)              |
| `config/`                                                | `/etc/localdesktop` (kept)  |

Packages added with `apt` while online live in the OS, so they are gone after an update that
replaces it.

### Configuration

`/etc/localdesktop/localdesktop.toml` works as in Local Desktop, minus the package install
commands (there is nothing to install):

```toml
[user]
username = "elementary"

[command]
# The default; change it to start something else instead of the elementary session.
launch = "export PIPEWIRE_RUNTIME_DIR=/tmp PULSE_SERVER=unix:/tmp/pulse/native WAYLAND_DISPLAY=/tmp/wayland-0; exec /usr/local/bin/elementary-mobile-session 2>&1"
```

Crash reporting is off. Set `SENTRY_DSN` when building to send crashes to your own Sentry project.

### Android's child process limit

Android 12 and later kill an app's child processes once it has more than 32 of them (the "phantom
process killer"); everything in the desktop then disappears at once. The session is trimmed to
about 23 processes at idle, which leaves room for a couple of apps (Terminal and Web together use
6). To open more at the same time, lift the limit once:

- Android 14 and later: Developer options → **Disable child process restrictions**.
- Otherwise, from a computer with adb:

  ```bash
  adb shell device_config set_sync_disabled_for_tests persistent
  adb shell device_config put activity_manager max_phantom_processes 2147483647
  adb shell settings put global settings_enable_monitor_phantom_procs false
  ```

### What does not work

- Apps elementary now ships only as Flatpaks (Calculator, Screenshot, Music, Videos, Camera, Maps,
  Photos) and AppCenter: Flatpak needs Linux user namespaces, which Android does not give apps.
  Web (Epiphany), Evince and File Roller are included as native packages instead.
- Hardware and system services: network, Bluetooth, power, printing and display settings are
  managed by Android. Screen locking is too.
- Translations are stripped to keep the APK small, so the desktop is in English.
- Shut down and restart in the session menu (there is no logind). Log out starts a fresh session.
- Known rough edges: the panel and dock sometimes appear only after the first tap, and sound is
  not working reliably yet.

## Building

You need Rust, Docker with buildx, and the Android toolchain used by Local Desktop
(`brew install llvm lld gradle@8` on a Mac).

### 1. Build the OS image

```bash
scripts/build-rootfs.sh
```

This builds `rootfs/Dockerfile` for linux/arm64 and writes
`assets/rootfs/elementary-os-rootfs.tar.xz` plus its manifest. It runs natively on Apple silicon,
arm64 Linux and GitHub's `ubuntu-24.04-arm` runners (about 5 minutes); on x86 machines Docker needs
QEMU and is much slower. The image is:

- `ubuntu:24.04` with `ppa:elementary-os/stable` and `ppa:elementary-os/os-patches`, the same
  packages elementary OS is built from (elementary publishes no arm64 image to start from);
- the package selection in [`rootfs/packages.list`](rootfs/packages.list), trimmed by the dpkg
  filters in `rootfs/overlay/etc/dpkg/dpkg.cfg.d/`;
- an Xwayland with one patch ([`rootfs/patches/`](rootfs/patches/)) so that Gala keeps the window
  size instead of switching to a 5120×2880 mode;
- the session scripts and defaults in [`rootfs/overlay/`](rootfs/overlay/).

### 2. Build the APK

```bash
cargo install --path patches/xbuild/xbuild --force
x build --release --platform android --arch arm64 --format apk
```

The APK is about 210 MB, almost all of it the OS image.

### Tests

```bash
cargo test --lib                                          # on a PC or in Termux
cargo test --lib -- --ignored installs_the_built_archive  # unpacks the image you built
```

## Credits

Local Desktop by its contributors, GPL-3.0 (see [LICENSE](LICENSE)). elementary OS and Pantheon by
[elementary, Inc.](https://elementary.io) and contributors; the OS image is assembled from their
public packages. This project is not affiliated with or endorsed by elementary, Inc.
