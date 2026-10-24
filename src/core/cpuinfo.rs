//! The guest's view of `/proc/cpuinfo`.
//!
//! The desktop is drawn by Mesa's llvmpipe, which compiles shaders with LLVM for "the host CPU".
//! On arm64, LLVM picks that CPU from the `CPU part` lines alone and assumes everything the core
//! can do. Many Armv9 phones (Snapdragon 8 Gen 1 to 3, for example) have cores whose SVE is turned
//! off by the firmware, so shaders compiled for them hit an illegal instruction: Gala, the window
//! manager, dies with SIGILL seconds after the desktop starts, and the screen stays black.
//!
//! Without the `CPU part` lines LLVM targets a generic Armv8 CPU, which every arm64 device runs.
//! The `Features` lines, which say what the kernel really enables, are kept.

/// Where the app writes the guest's `/proc/cpuinfo`, relative to the root filesystem.
pub const GUEST_CPUINFO: &str = "proc/.cpuinfo";

/// `/proc/cpuinfo` as the guest should see it, given the host's.
pub fn guest_cpuinfo(host: &str) -> String {
    if kernel_enables_sve(host) {
        return host.to_string();
    }
    host.lines()
        .filter(|line| !line.starts_with("CPU part"))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Whether every CPU lists SVE among the features the kernel enables.
fn kernel_enables_sve(cpuinfo: &str) -> bool {
    let mut features = cpuinfo
        .lines()
        .filter_map(|line| line.strip_prefix("Features"))
        .peekable();
    features.peek().is_some()
        && features.all(|line| {
            line.trim_start_matches([' ', '\t', ':'])
                .split_whitespace()
                .any(|feature| feature == "sve")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two of the eight CPUs of a Snapdragon 8 Gen 2 (Cortex-A510 and Cortex-X3), SVE disabled.
    const SNAPDRAGON_8_GEN_2: &str = "\
processor\t: 0
BogoMIPS\t: 38.40
Features\t: fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm jscvt fcma lrcpc dcpop sha3 sm3 sm4 asimddp sha512 asimdfhm dit uscat ilrcpc flagm sb paca pacg dcpodp flagm2 frint i8mm bf16 bti
CPU implementer\t: 0x41
CPU architecture: 8
CPU variant\t: 0x1
CPU part\t: 0xd46
CPU revision\t: 1

processor\t: 7
BogoMIPS\t: 38.40
Features\t: fp asimd evtstrm aes pmull sha1 sha2 crc32 atomics fphp asimdhp cpuid asimdrdm jscvt fcma lrcpc dcpop sha3 sm3 sm4 asimddp sha512 asimdfhm dit uscat ilrcpc flagm sb paca pacg dcpodp flagm2 frint i8mm bf16 bti
CPU implementer\t: 0x41
CPU architecture: 8
CPU variant\t: 0x1
CPU part\t: 0xd4e
CPU revision\t: 0
";

    #[test]
    fn hides_the_cpu_model_when_the_kernel_disables_sve() {
        let guest = guest_cpuinfo(SNAPDRAGON_8_GEN_2);

        assert!(!guest.contains("CPU part"));
        assert_eq!(guest.matches("Features\t: fp asimd").count(), 2);
        assert_eq!(guest.matches("CPU implementer\t: 0x41").count(), 2);
        assert!(guest.ends_with("CPU revision\t: 0\n"));
    }

    #[test]
    fn keeps_the_cpu_model_when_the_kernel_enables_sve() {
        let host = SNAPDRAGON_8_GEN_2.replace(" bti", " bti sve sve2");

        assert_eq!(guest_cpuinfo(&host), host);
    }

    #[test]
    fn hides_the_cpu_model_unless_every_cpu_has_sve() {
        let host = SNAPDRAGON_8_GEN_2.replacen(" bti", " bti sve", 1);

        assert!(!guest_cpuinfo(&host).contains("CPU part"));
    }

    #[test]
    fn hides_the_cpu_model_when_features_are_unknown() {
        let guest = guest_cpuinfo("processor\t: 0\nCPU part\t: 0xd4e\n");

        assert_eq!(guest, "processor\t: 0\n");
    }
}
