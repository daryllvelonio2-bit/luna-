//! Network reach for `terminal`'s child processes: a policy, and the kernel enforcement of it.
//!
//! The approval gate decides whether a command runs; the sandbox decides which paths it may touch.
//! Neither answers "may this command talk to the network", which is the question that matters for
//! `curl | sh`, a package manager, or a build script fetching a dependency. `tools.network` is that
//! answer, and it is enforced by the kernel rather than by inspecting the command string — a
//! string-prefix check can be walked around with `sh -c`, `$(...)`, an alias, or a wrapper script,
//! and a boundary that can be walked around is decoration.
//!
//! ## How
//!
//! Under `deny`, the child gets a seccomp-BPF filter installed after `fork` and before `exec`
//! (`pre_exec`), so the filter is inherited by everything the command spawns and cannot be undone
//! by the child: `socket(2)` succeeds only for `AF_UNIX`, and every other family returns `EPERM`.
//! Local sockets stay usable (they are how `nss`, D-Bus and editors talk to each other), loopback
//! included in the denied set — "localhost" is a network too, and the point is a boundary, not a
//! heuristic. `PR_SET_NO_NEW_PRIVS` is set first, so a setuid binary cannot drop the filter.
//!
//! ## What it does not do
//!
//! It does not limit an *already open* descriptor: stdio is the only one inherited, so there is
//! nothing else to inherit. It cannot police `terminal` commands that run outside the harness
//! (a shell a person starts themselves), and it is Linux-only — on other platforms `deny` fails
//! closed with an error rather than silently being ignored.

use std::io;

/// `tools.network`: whether a `terminal` child may open non-local sockets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkPolicy {
    /// Do what the process could always do. The default, so behaviour is unchanged until asked.
    #[default]
    Allow,
    /// No socket other than `AF_UNIX`; enforced by seccomp, inherited by every child.
    Deny,
}

impl NetworkPolicy {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "allow" | "on" | "yes" | "true" => Ok(Self::Allow),
            "deny" | "off" | "no" | "false" | "none" => Ok(Self::Deny),
            other => Err(format!(
                "invalid network policy '{other}': expected 'allow' or 'deny'"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    pub fn denies_sockets(&self) -> bool {
        matches!(self, Self::Deny)
    }
}

impl std::str::FromStr for NetworkPolicy {
    type Err = String;
    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

impl serde::Serialize for NetworkPolicy {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for NetworkPolicy {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Apply `policy` to a command about to be spawned.
///
/// `Allow` is a no-op. `Deny` installs the filter in the child between `fork` and `exec`, and fails
/// closed when the platform cannot enforce it: a boundary that quietly disappears is worse than one
/// that was never configured.
pub fn apply(cmd: &mut tokio::process::Command, policy: NetworkPolicy) -> io::Result<()> {
    if !policy.denies_sockets() {
        return Ok(());
    }
    install_pre_exec(cmd)
}

#[cfg(target_os = "linux")]
fn install_pre_exec(cmd: &mut tokio::process::Command) -> io::Result<()> {
    // SAFETY: the closure runs in the forked child before `exec`. It only makes two `prctl` calls
    // and touches no shared state, no allocator-owned memory it does not own, and no locks —
    // requirements of a `pre_exec` hook. A failure is returned to the parent as a spawn error
    // rather than being swallowed, so the command does not run unconfined.
    unsafe {
        cmd.pre_exec(install_no_network);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn install_pre_exec(_cmd: &mut tokio::process::Command) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "tools.network = deny is enforced with seccomp and is only available on Linux; \
         refusing to run rather than run unconfined",
    ))
}

#[cfg(target_os = "linux")]
const AUDIT_ARCH: u32 = if cfg!(target_arch = "x86_64") {
    0xC000_003E
} else if cfg!(target_arch = "aarch64") {
    0xC000_00B7
} else if cfg!(target_arch = "riscv64") {
    0xC000_00F3
} else {
    0
};

#[cfg(target_os = "linux")]
const NR_SOCKET: u32 = if cfg!(target_arch = "x86_64") {
    41
} else if cfg!(target_arch = "aarch64") || cfg!(target_arch = "riscv64") {
    198
} else {
    0
};

/// The classic-BPF program that allows everything except non-`AF_UNIX` `socket(2)` calls.
#[cfg(target_os = "linux")]
fn seccomp_filter() -> [libc::sock_filter; 9] {
    const BPF_LD_W_ABS: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const BPF_JMP_JEQ_K: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
    const BPF_RET_K: u16 = 0x06; // BPF_RET | BPF_K

    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_RET_ALLOW: u32 = 0x7FFF_0000;
    const AF_UNIX: u32 = 1;

    [
        // A = seccomp_data.arch
        libc::sock_filter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 4,
        },
        // Not our architecture: kill rather than let an unknown syscall table through.
        libc::sock_filter {
            code: BPF_JMP_JEQ_K,
            jt: 0,
            jf: 6,
            k: AUDIT_ARCH,
        },
        // A = seccomp_data.nr
        libc::sock_filter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 0,
        },
        // Not socket(2): allow (index 3 + 1 + 2 = 6).
        libc::sock_filter {
            code: BPF_JMP_JEQ_K,
            jt: 0,
            jf: 2,
            k: NR_SOCKET,
        },
        // A = seccomp_data.args[0] (the address family)
        libc::sock_filter {
            code: BPF_LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 16,
        },
        // AF_UNIX: allow (5 + 1 + 0 = 6); anything else: EPERM (5 + 1 + 1 = 7).
        libc::sock_filter {
            code: BPF_JMP_JEQ_K,
            jt: 0,
            jf: 1,
            k: AF_UNIX,
        },
        libc::sock_filter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ALLOW,
        },
        libc::sock_filter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_ERRNO | libc::EPERM as u32,
        },
        libc::sock_filter {
            code: BPF_RET_K,
            jt: 0,
            jf: 0,
            k: SECCOMP_RET_KILL_PROCESS,
        },
    ]
}

/// Install `program` on the calling process. Irreversible.
#[cfg(target_os = "linux")]
unsafe fn apply_filter(program: &libc::sock_fprog) -> io::Result<()> {
    const SECCOMP_MODE_FILTER: libc::c_ulong = 2;
    const PR_SET_SECCOMP: libc::c_int = 22;
    const PR_SET_NO_NEW_PRIVS: libc::c_int = 38;

    // SAFETY: `prctl` with these options only reads the pointers passed; the filter array outlives
    // the calls (it lives in `install_no_network`'s stack frame) and the kernel copies it.
    // Without `NO_NEW_PRIVS`, a setuid binary in the command could re-exec and lose the filter.
    if libc::prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
        return Err(io::Error::last_os_error());
    }
    if libc::prctl(
        PR_SET_SECCOMP,
        SECCOMP_MODE_FILTER,
        program as *const libc::sock_fprog,
        0,
        0,
    ) != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Install the no-network filter on the calling thread's process. Irreversible.
///
/// Called from a `pre_exec` hook, so it must not allocate after the filter is installed; everything
/// it needs lives on the stack.
#[cfg(target_os = "linux")]
pub fn install_no_network() -> io::Result<()> {
    use std::io::Error;

    // A seccomp filter can only be *tightened*, so refusing on an unknown architecture is the only
    // honest option: guessing a syscall number would either block the wrong thing or nothing.
    if AUDIT_ARCH == 0 || NR_SOCKET == 0 {
        return Err(Error::new(
            io::ErrorKind::Unsupported,
            "tools.network = deny: no seccomp program for this architecture",
        ));
    }

    let filter = seccomp_filter();

    let program = libc::sock_fprog {
        len: filter.len() as libc::c_ushort,
        filter: filter.as_ptr() as *mut libc::sock_filter,
    };

    // SAFETY: the filter and program are live on this stack frame for the whole call.
    unsafe { apply_filter(&program) }
}

#[cfg(not(target_os = "linux"))]
pub fn install_no_network() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no seccomp on this platform",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// The probe runs in a *child process*: installing the filter is irreversible, so it must never
    /// happen in the test harness's own process.
    #[test]
    fn the_filter_denies_non_unix_sockets_and_keeps_local_ones() {
        if std::env::var("LUNA_NETGUARD_PROBE").is_ok() {
            probe();
            return;
        }

        let exe = std::env::current_exe().expect("test binary");
        let output = std::process::Command::new(exe)
            .args([
                "--exact",
                "tools::netguard::tests::the_filter_denies_non_unix_sockets_and_keeps_local_ones",
                "--nocapture",
            ])
            .env("LUNA_NETGUARD_PROBE", "1")
            .output()
            .expect("probe runs");
        assert!(
            output.status.success(),
            "probe failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn probe() {
        install_no_network().expect("the filter installs without privileges");

        // AF_UNIX still works: nss, D-Bus and local tooling would otherwise break for no reason.
        std::os::unix::net::UnixDatagram::unbound().expect("AF_UNIX must still be allowed");

        // TCP is refused at socket(2), before any packet or DNS lookup happens.
        let error = std::net::TcpStream::connect(("127.0.0.1", 9)).expect_err("TCP must be denied");
        assert_eq!(
            error.raw_os_error(),
            Some(libc::EPERM),
            "expected EPERM, got {error}"
        );

        // ...and so is UDP, which is what a DNS query needs.
        let error = std::net::UdpSocket::bind("127.0.0.1:0").expect_err("UDP must be denied");
        assert_eq!(error.raw_os_error(), Some(libc::EPERM), "got {error}");

        // Loopback is a network too: the boundary is by address family, not by destination.
        let error = std::net::TcpStream::connect(("127.0.0.1", 1))
            .expect_err("loopback TCP must be denied");
        assert_eq!(error.raw_os_error(), Some(libc::EPERM), "got {error}");
    }

    #[test]
    fn the_policy_parses_both_spellings_and_refuses_nonsense() {
        assert_eq!(NetworkPolicy::parse("allow").unwrap(), NetworkPolicy::Allow);
        assert_eq!(NetworkPolicy::parse("DENY").unwrap(), NetworkPolicy::Deny);
        assert_eq!(NetworkPolicy::parse(" off ").unwrap(), NetworkPolicy::Deny);
        assert!(NetworkPolicy::parse("sometimes").is_err());
        assert_eq!(NetworkPolicy::default(), NetworkPolicy::Allow);
        assert!(NetworkPolicy::Deny.denies_sockets());
        assert!(!NetworkPolicy::Allow.denies_sockets());
    }
}
