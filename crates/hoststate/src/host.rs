//! Reads facts about the host. Nothing here needs Docker or an engine, and
//! system tools run by absolute path with a clean environment and no shell.

use std::ffi::OsStr;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const TOOL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TOOL_OUTPUT: u64 = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hypervisor {
    Available(String),
    Missing { detail: String, next: String },
    Unknown(String),
}

#[derive(Debug, Clone)]
pub struct HostInfo {
    pub os: String,
    pub arch: String,
    pub hypervisor: Hypervisor,
    pub cpus: Option<u32>,
    pub memory_total: Option<u64>,
    /// An estimate of memory that could be used without pressure.
    pub memory_available: Option<u64>,
    pub disk_free: Option<u64>,
}

/// Probes the real host. `disk_path` is where the state folder would live.
pub fn probe(disk_path: &Path) -> HostInfo {
    HostInfo {
        os: os_label(),
        arch: std::env::consts::ARCH.to_string(),
        hypervisor: hypervisor(),
        cpus: std::thread::available_parallelism()
            .ok()
            .and_then(|n| u32::try_from(n.get()).ok()),
        memory_total: memory_total(),
        memory_available: memory_available(),
        disk_free: disk_free(disk_path),
    }
}

/// Runs a tool with a time limit. `None` covers any failure.
fn tool(program: &str, args: &[&OsStr]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let read = stdout.take(MAX_TOOL_OUTPUT).read_to_string(&mut text);
        let _ = send.send(read.map(|_| text));
    });
    match receive.recv_timeout(TOOL_TIMEOUT) {
        Ok(Ok(text)) => {
            let status = child.wait().ok()?;
            status.success().then_some(text)
        }
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

fn os_label() -> String {
    let os = std::env::consts::OS;
    if cfg!(target_os = "macos") {
        return match tool("/usr/bin/sw_vers", &[OsStr::new("-productVersion")]) {
            Some(version) => format!("macOS {}", version.trim()),
            None => "macOS".to_string(),
        };
    }
    os.to_string()
}

fn sysctl(key: &str) -> Option<String> {
    tool("/usr/sbin/sysctl", &[OsStr::new("-n"), OsStr::new(key)]).map(|t| t.trim().to_string())
}

fn hypervisor() -> Hypervisor {
    if cfg!(target_os = "macos") {
        return match sysctl("kern.hv_support").as_deref() {
            Some("1") => Hypervisor::Available("Hypervisor.framework is supported".to_string()),
            Some(_) => Hypervisor::Missing {
                detail: "this Mac reports no hypervisor support".to_string(),
                next: "use a Mac with Apple Silicon, not a VM without nested virtualization"
                    .to_string(),
            },
            None => Hypervisor::Unknown("could not read kern.hv_support".to_string()),
        };
    }
    if cfg!(target_os = "linux") {
        let kvm = Path::new("/dev/kvm");
        if !kvm.exists() {
            return Hypervisor::Missing {
                detail: "/dev/kvm does not exist".to_string(),
                next: "enable KVM; cloud VMs need nested virtualization".to_string(),
            };
        }
        return match std::fs::OpenOptions::new().read(true).write(true).open(kvm) {
            Ok(_) => Hypervisor::Available("/dev/kvm is usable".to_string()),
            Err(_) => Hypervisor::Missing {
                detail: "/dev/kvm exists but this user cannot open it".to_string(),
                next: "add your user to the kvm group, then sign in again".to_string(),
            },
        };
    }
    Hypervisor::Missing {
        detail: "this OS has no supported hypervisor".to_string(),
        next: "use macOS on Apple Silicon, or Linux with KVM".to_string(),
    }
}

fn meminfo(key: &str) -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    parse_meminfo(&text, key)
}

fn parse_meminfo(text: &str, key: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with(key))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

fn memory_total() -> Option<u64> {
    if cfg!(target_os = "macos") {
        sysctl("hw.memsize")?.parse().ok()
    } else {
        meminfo("MemTotal:")
    }
}

fn memory_available() -> Option<u64> {
    if cfg!(target_os = "macos") {
        parse_vm_stat(&tool("/usr/bin/vm_stat", &[])?)
    } else {
        meminfo("MemAvailable:")
    }
}

/// Free, inactive and speculative pages: what macOS can hand out quickly.
pub(crate) fn parse_vm_stat(text: &str) -> Option<u64> {
    let first = text.lines().next()?;
    let page: u64 = first
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |label: &str| -> Option<u64> {
        let line = text.lines().find(|l| l.starts_with(label))?;
        line.rsplit(':')
            .next()?
            .trim()
            .trim_end_matches('.')
            .parse()
            .ok()
    };
    let total = pages("Pages free")?
        .checked_add(pages("Pages inactive")?)?
        .checked_add(pages("Pages speculative")?)?;
    total.checked_mul(page)
}

fn disk_free(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    let df = ["/bin/df", "/usr/bin/df"]
        .into_iter()
        .find(|p| Path::new(p).is_file())?;
    parse_df(&tool(df, &[OsStr::new("-Pk"), existing.as_os_str()])?)
}

/// Finds the capacity column, then takes the number before it. A filesystem
/// name can hold spaces, so counting from the left would not be reliable.
pub(crate) fn parse_df(text: &str) -> Option<u64> {
    let fields: Vec<&str> = text.lines().last()?.split_whitespace().collect();
    let capacity = fields.iter().position(|f| f.ends_with('%'))?;
    let kib: u64 = fields.get(capacity.checked_sub(1)?)?.parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_vm_stat() {
        let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                    Pages free:                  10.\n\
                    Pages active:                 5.\n\
                    Pages inactive:              20.\n\
                    Pages speculative:            2.\n";
        assert_eq!(parse_vm_stat(text), Some(32 * 16384));
        assert_eq!(parse_vm_stat("nonsense"), None);
    }

    #[test]
    fn reads_df_with_spaces_in_the_filesystem_name() {
        let text = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                    my disk 100 40 60 40% /Volumes/My Disk\n";
        assert_eq!(parse_df(text), Some(60 * 1024));
    }

    #[test]
    fn reads_meminfo() {
        let text = "MemTotal:       2048 kB\nMemAvailable:   1024 kB\n";
        assert_eq!(parse_meminfo(text, "MemTotal:"), Some(2048 * 1024));
        assert_eq!(parse_meminfo(text, "Nope:"), None);
    }
}
