//! Seguimiento de la sesión de juego: saber cuándo el juego está en marcha y cuándo se cerró, para
//! poder volver a mostrar UGL. Funciona con la lista de procesos del sistema (Windows):
//! - Inicio directo: el proceso lanzado y todos sus descendientes.
//! - Steam: además, cualquier proceso cuyo ejecutable esté dentro de la carpeta del juego.
//! Solo se consulta mientras hay una sesión activa; en reposo no hay ningún sondeo.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::integrations::executables::is_inside;

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub parent: u32,
    /// Ruta del ejecutable, si el sistema la deja leer (puede faltar en procesos elevados).
    pub exe: Option<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct SessionSpec {
    /// Proceso que lanzó UGL (inicio directo).
    pub root_pid: Option<u32>,
    /// Carpeta del juego (juegos de Steam).
    pub watch_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// El juego se estaba ejecutando y ya no.
    Ended,
    /// Nunca se detectó el juego (no arrancó, o no se puede observar).
    NeverStarted,
    /// El sistema no permite listar procesos.
    Unsupported,
}

/// Procesos de la lista que pertenecen al juego.
pub fn active_pids(processes: &[ProcessInfo], spec: &SessionSpec) -> HashSet<u32> {
    let mut active = HashSet::new();

    if let Some(root) = spec.root_pid {
        // Familia = el proceso lanzado y sus descendientes (aunque el lanzador ya haya terminado).
        let mut family: HashSet<u32> = HashSet::new();
        family.insert(root);
        let mut changed = true;
        while changed {
            changed = false;
            for process in processes {
                if process.pid != process.parent
                    && family.contains(&process.parent)
                    && family.insert(process.pid)
                {
                    changed = true;
                }
            }
        }
        for process in processes {
            if family.contains(&process.pid) {
                active.insert(process.pid);
            }
        }
    }

    if let Some(dir) = &spec.watch_dir {
        for process in processes {
            if let Some(exe) = &process.exe {
                if is_inside(exe, dir) {
                    active.insert(process.pid);
                }
            }
        }
    }
    active
}

/// Bucle de seguimiento con la lista de procesos y la espera inyectadas (así se puede probar).
/// - Espera hasta `startup_timeout` a que aparezca el juego.
/// - Una vez visto, da la sesión por terminada tras `grace_polls` consultas seguidas sin procesos
///   (algunos juegos pasan de un lanzador a otro proceso con un breve hueco).
pub fn track<F, S>(
    spec: &SessionSpec,
    mut snapshot: F,
    mut sleep: S,
    startup_timeout: Duration,
    poll: Duration,
    grace_polls: u32,
) -> SessionEnd
where
    F: FnMut() -> Option<Vec<ProcessInfo>>,
    S: FnMut(Duration),
{
    let mut waited = Duration::ZERO;
    let mut seen = false;
    let mut empty_streak = 0u32;
    loop {
        let Some(processes) = snapshot() else {
            return SessionEnd::Unsupported;
        };
        let alive = !active_pids(&processes, spec).is_empty();
        if alive {
            seen = true;
            empty_streak = 0;
        } else if seen {
            empty_streak += 1;
            if empty_streak >= grace_polls {
                return SessionEnd::Ended;
            }
        } else if waited >= startup_timeout {
            return SessionEnd::NeverStarted;
        }
        sleep(poll);
        waited += poll;
    }
}

/// Espera a que termine el juego usando la lista real de procesos. Bloquea: hilo aparte.
pub fn wait_for_game(spec: &SessionSpec) -> SessionEnd {
    let want_paths = spec.watch_dir.is_some();
    track(
        spec,
        || snapshot(want_paths),
        std::thread::sleep,
        Duration::from_secs(120),
        Duration::from_secs(2),
        3,
    )
}

#[cfg(windows)]
pub fn snapshot(with_paths: bool) -> Option<Vec<ProcessInfo>> {
    win::snapshot(with_paths)
}

#[cfg(not(windows))]
pub fn snapshot(_with_paths: bool) -> Option<Vec<ProcessInfo>> {
    None
}

#[cfg(windows)]
mod win {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use super::ProcessInfo;

    type Handle = isize;
    const TH32CS_SNAPPROCESS: u32 = 0x2;
    const INVALID_HANDLE_VALUE: Handle = -1;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        usage: u32,
        process_id: u32,
        default_heap_id: usize,
        module_id: u32,
        threads: u32,
        parent_process_id: u32,
        pri_class_base: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn QueryFullProcessImageNameW(
            process: Handle,
            flags: u32,
            name: *mut u16,
            size: *mut u32,
        ) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    pub fn snapshot(with_paths: bool) -> Option<Vec<ProcessInfo>> {
        // SAFETY: llamadas a la API de Windows con estructuras inicializadas y manejadores que se
        // cierran siempre.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE || snap == 0 {
                return None;
            }
            let mut entry: ProcessEntry32W = std::mem::zeroed();
            entry.size = std::mem::size_of::<ProcessEntry32W>() as u32;

            let mut processes = Vec::new();
            let mut more = Process32FirstW(snap, &mut entry);
            while more != 0 {
                processes.push(ProcessInfo {
                    pid: entry.process_id,
                    parent: entry.parent_process_id,
                    exe: if with_paths {
                        image_path(entry.process_id)
                    } else {
                        None
                    },
                });
                more = Process32NextW(snap, &mut entry);
            }
            CloseHandle(snap);
            Some(processes)
        }
    }

    unsafe fn image_path(pid: u32) -> Option<PathBuf> {
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process == 0 {
            return None; // proceso protegido o ya terminado
        }
        let mut buffer = vec![0u16; 1024];
        let mut length = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        Some(PathBuf::from(OsString::from_wide(&buffer[..length as usize])))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, exe: Option<&str>) -> ProcessInfo {
        ProcessInfo { pid, parent, exe: exe.map(PathBuf::from) }
    }

    #[test]
    fn the_launched_process_and_its_descendants_belong_to_the_game() {
        let processes = vec![
            p(10, 1, None),   // ajeno
            p(100, 10, None), // el que lanzó UGL
            p(200, 100, None), // hijo
            p(300, 200, None), // nieto
            p(400, 10, None), // hermano ajeno
        ];
        let spec = SessionSpec { root_pid: Some(100), watch_dir: None };
        let active = active_pids(&processes, &spec);
        assert_eq!(active, HashSet::from([100, 200, 300]));
    }

    #[test]
    fn children_are_found_even_after_the_launcher_exited() {
        // El proceso 100 ya no existe, pero su hijo (parent = 100) sigue vivo.
        let processes = vec![p(200, 100, None), p(5, 1, None)];
        let spec = SessionSpec { root_pid: Some(100), watch_dir: None };
        assert_eq!(active_pids(&processes, &spec), HashSet::from([200]));
    }

    #[test]
    fn steam_games_are_recognised_by_their_folder() {
        let processes = vec![
            p(1, 0, Some("C:\\Windows\\explorer.exe")),
            p(2, 1, Some("C:\\Steam\\steamapps\\common\\HL2\\hl2.exe")),
            p(3, 1, Some("c:\\steam\\steamapps\\common\\hl2\\bin\\helper.exe")),
            p(4, 1, Some("C:\\Steam\\steamapps\\common\\HL22\\x.exe")),
            p(5, 1, None),
        ];
        let spec = SessionSpec {
            root_pid: None,
            watch_dir: Some(PathBuf::from("C:\\Steam\\steamapps\\common\\HL2")),
        };
        assert_eq!(active_pids(&processes, &spec), HashSet::from([2, 3]));
    }

    #[test]
    fn tracking_ends_after_the_grace_period() {
        let spec = SessionSpec { root_pid: Some(100), watch_dir: None };
        let mut script = vec![
            Some(vec![]),                  // todavía no ha arrancado
            Some(vec![p(100, 1, None)]),   // en marcha
            Some(vec![p(100, 1, None)]),
            Some(vec![]),                  // hueco (p. ej. del lanzador al juego)
            Some(vec![p(200, 100, None)]), // sigue vivo: el hueco no cuenta
            Some(vec![]),
            Some(vec![]),
            Some(vec![]), // 3 seguidas vacías: terminó
        ]
        .into_iter();
        let mut sleeps = 0;
        let end = track(
            &spec,
            || script.next().unwrap(),
            |_| sleeps += 1,
            Duration::from_secs(30),
            Duration::from_secs(2),
            3,
        );
        assert_eq!(end, SessionEnd::Ended);
        assert_eq!(sleeps, 7);
    }

    #[test]
    fn a_game_that_never_appears_times_out() {
        let spec = SessionSpec { root_pid: Some(100), watch_dir: None };
        let end = track(
            &spec,
            || Some(vec![p(1, 0, None)]),
            |_| {},
            Duration::from_secs(10),
            Duration::from_secs(2),
            3,
        );
        assert_eq!(end, SessionEnd::NeverStarted);
    }

    #[test]
    fn unsupported_platforms_are_reported() {
        let end = track(
            &SessionSpec::default(),
            || None,
            |_| {},
            Duration::from_secs(10),
            Duration::from_secs(2),
            3,
        );
        assert_eq!(end, SessionEnd::Unsupported);
    }
}
