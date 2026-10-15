use super::process::GuestProcess;
use crate::android::utils::application_context::get_application_context;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

static LAUNCH_RUNNING: AtomicBool = AtomicBool::new(false);

struct LaunchRunningGuard;

impl Drop for LaunchRunningGuard {
    fn drop(&mut self) {
        LAUNCH_RUNNING.store(false, Ordering::Release);
    }
}

pub fn launch() {
    if LAUNCH_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        log::info!("Skipping launch because the desktop session is already running");
        return;
    }

    thread::spawn(move || {
        let _guard = LaunchRunningGuard;

        let local_config = get_application_context().local_config;
        let username = local_config.user.username;

        GuestProcess {
            command: local_config.command.launch,
            user: Some(username),
            log: Some(Arc::new(|it| log::info!("session: {}", it))),
        }
        .run();
    });
}
