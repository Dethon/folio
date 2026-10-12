//! Development aid: a timeline of start-up and a memory report.
//!
//! Set `FOLIO_TRACE` to a file path and every [`mark`] is appended to it as
//! `<milliseconds since the process was created> <label>`. Unset, a mark is
//! one atomic load. Setting `FOLIO_TRACE_LOG` as well adds what the windowing
//! and graphics libraries log, at some cost to the timings.

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);

struct State {
    file: File,
    start: Instant,
    /// Milliseconds between the creation of the process and `start`: time
    /// spent loading the executable and its libraries.
    before_main: f64,
    seen: HashSet<&'static str>,
}

/// Call first thing in `main`.
pub fn init() {
    let Some(path) = std::env::var_os("FOLIO_TRACE") else {
        return;
    };

    let Ok(file) = File::options().create(true).append(true).open(path) else {
        return;
    };

    if let Ok(mut state) = STATE.lock() {
        *state = Some(State {
            file,
            start: Instant::now(),
            before_main: platform::since_process_creation().unwrap_or(0.0),
            seen: HashSet::new(),
        });
        ENABLED.store(true, Ordering::Relaxed);
    }

    mark("main");

    // What the windowing and graphics libraries say they are doing. Asked
    // for separately, because the renderer does extra work when it has a
    // listener: it probes the graphics adapters a second time to list them.
    if std::env::var_os("FOLIO_TRACE_LOG").is_some() && log::set_logger(&Logger).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
}

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        // The shader compiler narrates every expression it resolves.
        metadata.level() <= log::Level::Debug && !metadata.target().starts_with("naga")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        with(|state| {
            let at = state.before_main + state.start.elapsed().as_secs_f64() * 1000.0;
            let line = record.args().to_string();
            let line = line.lines().next().unwrap_or_default();
            let _ = writeln!(state.file, "{at:9.1}   [{}] {:.160}", record.target(), line);
        });
    }

    fn flush(&self) {}
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

fn with(f: impl FnOnce(&mut State)) {
    if enabled()
        && let Ok(mut state) = STATE.lock()
        && let Some(state) = state.as_mut()
    {
        f(state);
    }
}

/// Records the first time `label` is reached; later calls are ignored, so
/// this can sit in code that runs every frame.
pub fn mark(label: &'static str) {
    with(|state| {
        if state.seen.insert(label) {
            let at = state.before_main + state.start.elapsed().as_secs_f64() * 1000.0;
            let _ = writeln!(state.file, "{at:9.1} {label}");
        }
    });
}

/// Records a free-form line.
pub fn note(line: impl FnOnce() -> String) {
    with(|state| {
        let at = state.before_main + state.start.elapsed().as_secs_f64() * 1000.0;
        let _ = writeln!(state.file, "{at:9.1} {}", line());
    });
}

/// Records the memory the process is using.
pub fn memory(label: &str) {
    with(|state| {
        let _ = writeln!(state.file, "          {}", usage_line(label));
    });
}

/// The memory the process is using, in a line.
pub fn usage_line(label: &str) -> String {
    let usage = platform::usage();
    let mib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);

    format!(
        "memory[{label}]: working set {:.1} MiB ({:.1} private), peak {:.1} MiB, committed {:.1} MiB, \
         {} threads, {:.0} ms of processor time so far ({:.0} on this thread)",
        mib(usage.working_set),
        mib(usage.private_working_set),
        mib(usage.peak_working_set),
        mib(usage.private),
        usage.threads,
        usage.processor,
        usage.this_thread,
    )
}

#[derive(Default)]
struct Usage {
    working_set: u64,
    /// The part of the working set no other process shares: the figure Task
    /// Manager shows as "Memory".
    private_working_set: u64,
    peak_working_set: u64,
    private: u64,
    threads: u32,
    /// Milliseconds of processor time used, on every thread.
    processor: f64,
    /// And on the thread asking, where that can be told.
    this_thread: f64,
}

#[cfg(windows)]
mod platform {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
    };
    use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetProcessTimes,
    };

    use super::Usage;

    fn ticks(time: FILETIME) -> u64 {
        (time.dwHighDateTime as u64) << 32 | time.dwLowDateTime as u64
    }

    pub fn since_process_creation() -> Option<f64> {
        let mut created = FILETIME::default();
        let mut unused = [FILETIME::default(); 3];
        let [exit, kernel, user] = &mut unused;

        unsafe {
            GetProcessTimes(GetCurrentProcess(), &mut created, exit, kernel, user).ok()?;

            // File times count in units of 100 ns.
            Some(ticks(GetSystemTimeAsFileTime()).saturating_sub(ticks(created)) as f64 / 10_000.0)
        }
    }

    pub fn usage() -> Usage {
        let mut usage = Usage::default();
        let mut counters = PROCESS_MEMORY_COUNTERS_EX2::default();

        unsafe {
            let read = K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                (&raw mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
            );

            let mut times = [FILETIME::default(); 4];
            let [created, exited, kernel, user] = &mut times;

            if GetProcessTimes(GetCurrentProcess(), created, exited, kernel, user).is_ok() {
                usage.processor = (ticks(*kernel) + ticks(*user)) as f64 / 10_000.0;
            }

            if windows::Win32::System::Threading::GetThreadTimes(
                windows::Win32::System::Threading::GetCurrentThread(),
                created,
                exited,
                kernel,
                user,
            )
            .is_ok()
            {
                usage.this_thread = (ticks(*kernel) + ticks(*user)) as f64 / 10_000.0;
            }

            if read.as_bool() {
                usage.working_set = counters.WorkingSetSize as u64;
                usage.peak_working_set = counters.PeakWorkingSetSize as u64;
                usage.private = counters.PrivateUsage as u64;
                usage.private_working_set = counters.PrivateWorkingSetSize as u64;
            }

            if let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) {
                let process = GetCurrentProcessId();
                let mut entry = THREADENTRY32 {
                    dwSize: size_of::<THREADENTRY32>() as u32,
                    ..THREADENTRY32::default()
                };
                let mut more = Thread32First(snapshot, &mut entry).is_ok();

                while more {
                    if entry.th32OwnerProcessID == process {
                        usage.threads += 1;
                    }

                    more = Thread32Next(snapshot, &mut entry).is_ok();
                }

                let _ = CloseHandle(snapshot);
            }
        }

        usage
    }
}

#[cfg(not(windows))]
mod platform {
    use super::Usage;

    pub fn since_process_creation() -> Option<f64> {
        None
    }

    /// From `/proc/self/status`, where the kernel reports kibibytes.
    pub fn usage() -> Usage {
        let mut usage = Usage::default();
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();

        // Fields 14 and 15 of `stat`, after the name in brackets, are the
        // time spent in user and kernel mode, in ticks of 10 ms.
        if let Some(fields) = std::fs::read_to_string("/proc/self/stat")
            .ok()
            .as_deref()
            .and_then(|stat| stat.rsplit_once(')'))
        {
            usage.processor = fields
                .1
                .split_whitespace()
                .skip(11)
                .take(2)
                .filter_map(|ticks| ticks.parse::<f64>().ok())
                .sum::<f64>()
                * 10.0;
        }

        for line in status.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };

            let number = value
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);

            match key {
                "VmRSS" => usage.working_set = number * 1024,
                "VmHWM" => usage.peak_working_set = number * 1024,
                "RssAnon" => {
                    usage.private = number * 1024;
                    usage.private_working_set = number * 1024;
                }
                "Threads" => usage.threads = number as u32,
                _ => {}
            }
        }

        usage
    }
}
