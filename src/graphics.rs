//! Which graphics backend the renderer runs on, and what follows from it.
//!
//! On Windows the renderer sits on Direct3D 12 unless `WGPU_BACKEND` says
//! otherwise. OpenGL (`WGPU_BACKEND=gl`) is worth saying: measured on an
//! NVIDIA card its driver is ready in about a third of the time, with some
//! 30 MB less. It is not the default because it has been tried on that one
//! card only, and because it costs more processor time while scrolling.
//!
//! One thing has to be done differently on OpenGL. Left to wait for the
//! display itself, NVIDIA's driver spins a processor core. So frames are
//! presented at once and paced against the compositor here: see [`pace`].

/// Settles the backend. Call before anything else is running: it sets
/// environment variables, which is how the renderer is told.
pub fn select() {
    #[cfg(windows)]
    {
        use std::sync::atomic::Ordering;

        let backend = std::env::var("WGPU_BACKEND").ok();

        // SAFETY: nothing else is running yet, so no thread can be reading
        // the environment.
        unsafe {
            match backend.as_deref() {
                // Windows always has Direct3D 12. Naming it stops the
                // renderer from also loading the Vulkan and OpenGL drivers
                // just to compare adapters, which costs start-up time and
                // tens of megabytes.
                None => std::env::set_var("WGPU_BACKEND", "dx12"),
                Some("gl") if std::env::var_os("ICED_PRESENT_MODE").is_none() => {
                    std::env::set_var("ICED_PRESENT_MODE", "immediate");
                    windows::PACED.store(true, Ordering::Relaxed);
                }
                Some(_) => {}
            }
        }
    }
}

/// Holds a frame back until the compositor is ready for one, when frames
/// follow closely on each other and the driver is not doing so itself. Call
/// once a frame, while it is being drawn.
pub fn pace() {
    #[cfg(windows)]
    windows::pace();
}

#[cfg(windows)]
mod windows {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use windows::Win32::Graphics::Dwm::DwmFlush;

    /// Frames are presented without waiting for the display.
    pub static PACED: AtomicBool = AtomicBool::new(false);

    /// Frames closer together than this are part of one motion. The first
    /// frame after a pause is drawn at once; the rest keep step with the
    /// compositor.
    const MOTION: Duration = Duration::from_millis(50);

    static LAST_FRAME: Mutex<Option<Instant>> = Mutex::new(None);

    pub fn pace() {
        if !PACED.load(Ordering::Relaxed) {
            return;
        }

        let Ok(mut last) = LAST_FRAME.lock() else {
            return;
        };

        if last.is_some_and(|last| last.elapsed() < MOTION) {
            // Returns when the compositor has next put a frame on screen.
            // SAFETY: the call takes no arguments and touches nothing of ours.
            unsafe {
                let _ = DwmFlush();
            }
        }

        *last = Some(Instant::now());
    }
}
