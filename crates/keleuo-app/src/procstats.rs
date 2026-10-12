//! What this process and the processes it started use, and a few facts about the machine,
//! for the About window. Memory is the physical footprint, the number Activity Monitor shows
//! as "Memory" (resident size counts shared system libraries and overstates it).

/// One reading.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// This process's physical footprint, in bytes.
    pub footprint: u64,
    pub threads: u32,
    /// CPU time used so far (user and system), in nanoseconds.
    pub cpu_ns: u64,
    /// Processes this one started, and everything they started (the agents' interfaces,
    /// their tools, terminals' shells).
    pub children: u32,
    /// Their physical footprint together, in bytes.
    pub children_footprint: u64,
    /// The WebKit processes serving this app's web pages (the pages, their drawing and, when
    /// it can be told, their networking). macOS starts them as system services, not as
    /// children, so they are counted apart.
    pub web: u32,
    /// Their physical footprint together, in bytes.
    pub web_footprint: u64,
}

/// Facts about the machine, read once.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Machine {
    pub os: String,
    pub chip: String,
    pub cores: u32,
    pub memory: u64,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{Machine, Usage};
    use std::ffi::CString;

    /// Physical footprint and CPU time of `pid`, when it can be read.
    fn rusage(pid: i32) -> Option<libc::rusage_info_v4> {
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid rusage_info_v4 and the flavor says so.
        let r = unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, &mut info as *mut _ as *mut libc::rusage_info_t) };
        (r == 0).then_some(info)
    }

    fn threads(pid: i32) -> u32 {
        let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        // SAFETY: the buffer is a proc_taskinfo of the size given.
        let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, &mut info as *mut _ as *mut libc::c_void, size) };
        if r == size { info.pti_threadnum.max(0) as u32 } else { 0 }
    }

    #[repr(C)]
    struct Timebase {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn mach_timebase_info(info: *mut Timebase) -> libc::c_int;
    }

    /// Mach time units to nanoseconds (1 on Intel; not 1 on Apple silicon).
    fn to_ns(ticks: u64) -> u64 {
        static RATIO: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
        let (numer, denom) = *RATIO.get_or_init(|| {
            let mut tb = Timebase { numer: 0, denom: 0 };
            // SAFETY: a valid out pointer to the struct the call fills.
            unsafe { mach_timebase_info(&mut tb) };
            if tb.denom == 0 { (1, 1) } else { (tb.numer, tb.denom) }
        });
        (ticks as u128 * numer as u128 / denom as u128) as u64
    }

    fn child_pids(pid: i32) -> Vec<i32> {
        let mut buf = vec![0i32; 256];
        loop {
            let bytes = (buf.len() * 4) as i32;
            // SAFETY: the buffer holds `bytes` bytes.
            let n = unsafe { libc::proc_listchildpids(pid, buf.as_mut_ptr() as *mut libc::c_void, bytes) };
            if n <= 0 {
                return Vec::new();
            }
            // The count comes back in pids (not bytes) on current macOS; a full buffer may
            // mean more.
            let n = n as usize;
            if n < buf.len() {
                buf.truncate(n);
                return buf.into_iter().filter(|p| *p > 0).collect();
            }
            buf.resize(buf.len() * 2, 0);
        }
    }

    pub fn usage(web: &[i32]) -> Usage {
        let me = std::process::id() as i32;
        let mut u = Usage::default();
        if let Some(r) = rusage(me) {
            u.footprint = r.ri_phys_footprint;
            u.cpu_ns = to_ns(r.ri_user_time + r.ri_system_time);
        }
        u.threads = threads(me);
        // Everything below this process, breadth first.
        let mut queue = child_pids(me);
        let mut seen = std::collections::HashSet::new();
        while let Some(pid) = queue.pop() {
            if !seen.insert(pid) || seen.len() > 4096 {
                continue;
            }
            if let Some(r) = rusage(pid) {
                u.children += 1;
                u.children_footprint += r.ri_phys_footprint;
            }
            queue.extend(child_pids(pid));
        }
        for pid in web_pids(me, web) {
            if let Some(r) = rusage(pid) {
                u.web += 1;
                u.web_footprint += r.ri_phys_footprint;
            }
        }
        u
    }

    type Responsible = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;

    /// The process macOS holds responsible for `pid` (the app an XPC service works for), from
    /// the system library's `responsibility_get_pid_responsible_for_pid`. Not public API, so
    /// looked up when running: without it, no figure rather than a failure to start.
    fn responsible_for(pid: i32) -> Option<i32> {
        static FUNC: std::sync::OnceLock<Option<Responsible>> = std::sync::OnceLock::new();
        let f = *FUNC.get_or_init(|| {
            // SAFETY: a lookup by a NUL-terminated name in the loaded images.
            let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"responsibility_get_pid_responsible_for_pid".as_ptr()) };
            // SAFETY: the symbol has this signature (pid in, pid out) since macOS 10.14.
            (!p.is_null()).then(|| unsafe { std::mem::transmute::<*mut libc::c_void, Responsible>(p) })
        });
        // SAFETY: the function only reads the kernel's bookkeeping for `pid`.
        f.map(|f| unsafe { f(pid) }).filter(|r| *r > 0)
    }

    fn all_pids() -> Vec<i32> {
        let mut buf = vec![0i32; 2048];
        loop {
            let bytes = (buf.len() * 4) as i32;
            // SAFETY: the buffer holds `bytes` bytes.
            let n = unsafe { libc::proc_listallpids(buf.as_mut_ptr() as *mut libc::c_void, bytes) };
            if n <= 0 {
                return Vec::new();
            }
            let n = n as usize;
            if n < buf.len() {
                buf.truncate(n);
                return buf.into_iter().filter(|p| *p > 0).collect();
            }
            buf.resize(buf.len() * 2, 0);
        }
    }

    fn name(pid: i32) -> String {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is as long as the size given.
        let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32) };
        if n <= 0 { String::new() } else { String::from_utf8_lossy(&buf[..n as usize]).into_owned() }
    }

    /// WebKit's processes (com.apple.WebKit.WebContent, .GPU, .Networking) working for `me`:
    /// those the web views named (`known`), and, when macOS holds this app responsible for
    /// itself (opened as an app; started from a terminal, the terminal is held responsible
    /// for it and for everything WebKit runs for it), every WebKit process it holds this app
    /// responsible for.
    fn web_pids(me: i32, known: &[i32]) -> Vec<i32> {
        let mut pids: Vec<i32> = known.to_vec();
        if responsible_for(me) == Some(me) {
            pids.extend(all_pids().into_iter().filter(|&pid| pid != me && responsible_for(pid) == Some(me)));
        }
        pids.sort_unstable();
        pids.dedup();
        // A pid that ended and was reused by something else is not counted.
        pids.retain(|&pid| name(pid).starts_with("com.apple.WebKit."));
        pids
    }

    fn sysctl_string(name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        let mut len = 0usize;
        // SAFETY: asking for the size first, then filling a buffer of that size.
        unsafe {
            if libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0) != 0 || len == 0 {
                return None;
            }
            let mut buf = vec![0u8; len];
            if libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr() as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) != 0 {
                return None;
            }
            buf.truncate(len);
            Some(String::from_utf8_lossy(&buf).trim_end_matches('\0').trim().to_string())
        }
    }

    fn sysctl_u64(name: &str) -> Option<u64> {
        let name = CString::new(name).ok()?;
        let mut value = 0u64;
        let mut len = std::mem::size_of::<u64>();
        // SAFETY: an 8-byte buffer; smaller values (u32) fill its low bytes.
        let r = unsafe { libc::sysctlbyname(name.as_ptr(), &mut value as *mut u64 as *mut libc::c_void, &mut len, std::ptr::null_mut(), 0) };
        (r == 0).then_some(if len == 4 { value & 0xffff_ffff } else { value })
    }

    pub fn machine() -> Machine {
        Machine {
            os: sysctl_string("kern.osproductversion").map(|v| format!("macOS {v}")).unwrap_or_else(|| "macOS".into()),
            chip: sysctl_string("machdep.cpu.brand_string").unwrap_or_default(),
            cores: sysctl_u64("hw.ncpu").unwrap_or(0) as u32,
            memory: sysctl_u64("hw.memsize").unwrap_or(0),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Machine, Usage};
    pub fn usage(_web: &[i32]) -> Usage {
        Usage::default()
    }
    pub fn machine() -> Machine {
        Machine { os: std::env::consts::OS.into(), ..Default::default() }
    }
}

pub use imp::{machine, usage};

/// Bytes as people read them: "142 MB", "1.4 GB".
pub fn bytes(n: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = n as f64 / MB;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else if mb >= 10.0 { format!("{mb:.0} MB") } else { format!("{mb:.1} MB") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_this_process() {
        let u = usage(&[]);
        if cfg!(target_os = "macos") {
            assert!(u.footprint > 0 && u.threads > 0, "{u:?}");
            let m = machine();
            assert!(m.memory > 0 && m.cores > 0 && m.os.starts_with("macOS"), "{m:?}");
        }
    }

    #[test]
    fn counts_the_processes_it_started() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let before = usage(&[]).children;
        let mut kids: Vec<_> = (0..3).map(|_| std::process::Command::new("sleep").arg("5").spawn().unwrap()).collect();
        let during = usage(&[]);
        for k in &mut kids {
            let _ = k.kill();
            let _ = k.wait();
        }
        assert!(during.children >= before + 3, "{before} then {during:?}");
        assert!(during.children_footprint > 0);
    }

    #[test]
    fn no_web_pages_without_web_views() {
        // This test process shows no web page, so WebKit works for none of it; a pid that is
        // not WebKit's (this one) is not counted as one.
        let u = usage(&[std::process::id() as i32]);
        assert_eq!((u.web, u.web_footprint), (0, 0), "{u:?}");
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(bytes(5 * 1024 * 1024 + 300 * 1024), "5.3 MB");
        assert_eq!(bytes(142 * 1024 * 1024), "142 MB");
        assert_eq!(bytes(1536 * 1024 * 1024), "1.5 GB");
    }
}
