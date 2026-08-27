use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

fn should_show(stderr_is_terminal: bool, verbose: bool) -> bool {
    stderr_is_terminal && !verbose
}

static ACTIVE_INDICATOR: OnceLock<WaitIndicator> = OnceLock::new();

pub fn finish_active() {
    if let Some(indicator) = ACTIVE_INDICATOR.get() {
        indicator.finish();
    }
}

struct WorkerState {
    stopped: Mutex<bool>,
    wake: Condvar,
}

struct IndicatorInner {
    state: Arc<WorkerState>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct WaitIndicator {
    inner: Arc<IndicatorInner>,
}

impl WaitIndicator {
    pub fn start(label: &'static str, verbose: bool) -> Self {
        let visible = should_show(io::stderr().is_terminal(), verbose);
        let indicator = if visible {
            Self::start_with_writer(
                label,
                Duration::from_millis(200),
                Duration::from_millis(100),
                io::stderr(),
            )
        } else {
            Self::disabled()
        };
        let _ = ACTIVE_INDICATOR.set(indicator.clone());
        if visible
            && ctrlc::set_handler(|| {
                finish_active();
                std::process::exit(130);
            })
            .is_err()
        {
            indicator.finish();
        }
        indicator
    }

    fn disabled() -> Self {
        Self {
            inner: Arc::new(IndicatorInner {
                state: Arc::new(WorkerState {
                    stopped: Mutex::new(true),
                    wake: Condvar::new(),
                }),
                worker: Mutex::new(None),
            }),
        }
    }

    fn start_with_writer<W>(
        label: &'static str,
        delay: Duration,
        interval: Duration,
        mut writer: W,
    ) -> Self
    where
        W: Write + Send + 'static,
    {
        let state = Arc::new(WorkerState {
            stopped: Mutex::new(false),
            wake: Condvar::new(),
        });
        let worker_state = Arc::clone(&state);
        let worker = thread::spawn(move || {
            let stopped = worker_state.stopped.lock().unwrap();
            let (stopped, _) = worker_state
                .wake
                .wait_timeout_while(stopped, delay, |stopped| !*stopped)
                .unwrap();
            if *stopped {
                return;
            }
            drop(stopped);

            const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut frame_index = 0;
            let mut rendered = false;

            loop {
                if write!(writer, "\r{} {}", FRAMES[frame_index], label).is_err()
                    || writer.flush().is_err()
                {
                    break;
                }
                rendered = true;
                frame_index = (frame_index + 1) % FRAMES.len();

                let stopped = worker_state.stopped.lock().unwrap();
                let (stopped, _) = worker_state
                    .wake
                    .wait_timeout_while(stopped, interval, |stopped| !*stopped)
                    .unwrap();
                if *stopped {
                    break;
                }
            }

            if rendered {
                let _ = write!(writer, "\r\x1b[2K");
                let _ = writer.flush();
            }
        });

        Self {
            inner: Arc::new(IndicatorInner {
                state,
                worker: Mutex::new(Some(worker)),
            }),
        }
    }

    pub fn finish(&self) {
        {
            let mut stopped = self.inner.state.stopped.lock().unwrap();
            *stopped = true;
            self.inner.state.wake.notify_all();
        }

        if let Some(worker) = self.inner.worker.lock().unwrap().take() {
            worker.join().unwrap();
        }
    }
}

impl Drop for WaitIndicator {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::{should_show, WaitIndicator};
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Clone, Default)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl SharedWriter {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Write for SharedWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn finishing_before_delay_is_silent_and_prompt() {
        let writer = SharedWriter::default();
        let started_at = Instant::now();
        let indicator = WaitIndicator::start_with_writer(
            "Preparing review branch",
            Duration::from_millis(250),
            Duration::from_millis(100),
            writer.clone(),
        );

        indicator.finish();

        assert!(writer.bytes().is_empty());
        assert!(
            started_at.elapsed() < Duration::from_millis(100),
            "finishing a hidden indicator should wake its worker immediately"
        );
    }

    #[test]
    fn visible_indicator_uses_plain_braille_frames_and_clears_its_line() {
        let writer = SharedWriter::default();
        let indicator = WaitIndicator::start_with_writer(
            "Preparing review branch",
            Duration::ZERO,
            Duration::from_millis(5),
            writer.clone(),
        );
        let deadline = Instant::now() + Duration::from_secs(1);

        while !String::from_utf8(writer.bytes())
            .unwrap()
            .contains("⠙ Preparing review branch")
        {
            assert!(
                Instant::now() < deadline,
                "indicator did not advance frames"
            );
            thread::sleep(Duration::from_millis(1));
        }
        indicator.finish();

        let output = String::from_utf8(writer.bytes()).unwrap();
        assert!(output.contains("⠋ Preparing review branch"));
        assert!(output.contains("⠙ Preparing review branch"));
        assert!(!output.contains('…'));
        assert!(
            !output.contains("\x1b[3"),
            "indicator must not set a foreground color"
        );
        assert!(
            !output.contains("\x1b[9"),
            "indicator must not set a bright foreground color"
        );
        assert!(
            !output.contains("\x1b[?25"),
            "indicator must not hide the cursor"
        );
        assert!(output.ends_with("\r\x1b[2K"));
    }

    #[test]
    fn visibility_requires_an_interactive_stderr_and_non_verbose_mode() {
        assert!(should_show(true, false));
        assert!(!should_show(false, false));
        assert!(!should_show(true, true));
        assert!(!should_show(false, true));
    }
}
