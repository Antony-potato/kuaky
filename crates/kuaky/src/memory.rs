//! Memoria que usa kuaky y los procesos que lanzó (agentes, LSP, terminales).
//!
//! Se mide la "huella física" (`phys_footprint`), la misma cifra que muestra
//! Activity Monitor en la columna Memoria. En otros sistemas no se mide.

/// Memoria de kuaky y de sus procesos hijos, en bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub app: u64,
    /// Suma de todos los descendientes (agentes y lo que estos lanzan).
    pub children: u64,
    pub child_count: usize,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.app + self.children
    }
}

/// Mide ahora; `None` si el sistema no lo permite.
pub fn sample() -> Option<Usage> {
    imp::sample()
}

/// "312 MB", "1.4 GB".
pub fn format(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Usage;
    use std::ffi::c_void;

    const RUSAGE_INFO_V0: i32 = 0;
    const PROC_PPID_ONLY: u32 = 6;

    /// `struct rusage_info_v0` de `<sys/resource.h>` (solo se lee la huella,
    /// el resto de campos dan la forma que espera el sistema).
    #[repr(C)]
    #[derive(Default)]
    #[allow(dead_code)]
    struct RusageInfoV0 {
        uuid: [u8; 16],
        user_time: u64,
        system_time: u64,
        pkg_idle_wkups: u64,
        interrupt_wkups: u64,
        pageins: u64,
        wired_size: u64,
        resident_size: u64,
        phys_footprint: u64,
        proc_start_abstime: u64,
        proc_exit_abstime: u64,
    }

    unsafe extern "C" {
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageInfoV0) -> i32;
        fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut c_void, buffersize: i32) -> i32;
    }

    fn footprint(pid: i32) -> Option<u64> {
        let mut info = RusageInfoV0::default();
        // SAFETY: `info` es un `rusage_info_v0` válido y del tamaño que pide el flavor.
        let rc = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V0, &mut info) };
        (rc == 0).then_some(info.phys_footprint)
    }

    fn children(pid: i32) -> Vec<i32> {
        let mut buf = vec![0i32; 256];
        // SAFETY: el búfer tiene el tamaño en bytes que se le pasa.
        let bytes = unsafe { proc_listpids(PROC_PPID_ONLY, pid as u32, buf.as_mut_ptr().cast(), (buf.len() * 4) as i32) };
        if bytes <= 0 {
            return Vec::new();
        }
        buf.truncate(bytes as usize / 4);
        buf.retain(|p| *p > 0);
        buf
    }

    pub fn sample() -> Option<Usage> {
        let me = std::process::id() as i32;
        let app = footprint(me)?;
        let mut usage = Usage { app, ..Usage::default() };
        let mut pending = children(me);
        while let Some(pid) = pending.pop() {
            if let Some(bytes) = footprint(pid) {
                usage.children += bytes;
                usage.child_count += 1;
            }
            pending.extend(children(pid));
        }
        Some(usage)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn sample() -> Option<super::Usage> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(format(312 * 1024 * 1024), "312 MB");
        assert_eq!(format(1536 * 1024 * 1024), "1.5 GB");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn measures_itself_and_children() {
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let usage = sample().unwrap();
        let _ = child.kill();
        assert!(usage.app > 1024 * 1024, "{usage:?}");
        assert!(usage.child_count >= 1, "{usage:?}");
    }
}
