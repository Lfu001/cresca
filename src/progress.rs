use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

fn should_show(stderr_is_terminal: bool, verbose: bool) -> bool {
    stderr_is_terminal && !verbose
}

fn progress_bar(percent: u8) -> String {
    const WIDTH: usize = 20;
    let filled = usize::from(percent) * WIDTH / 100;
    if filled == WIDTH {
        return format!("[{}]", "=".repeat(WIDTH));
    }
    format!(
        "[{}>{}]",
        "=".repeat(filled),
        " ".repeat(WIDTH - filled - 1)
    )
}

static ACTIVE_INDICATOR: OnceLock<WaitIndicator> = OnceLock::new();

pub fn finish_active() {
    if let Some(indicator) = ACTIVE_INDICATOR.get() {
        indicator.finish();
    }
}

struct WorkerState {
    current: Mutex<IndicatorState>,
    wake: Condvar,
}

struct IndicatorState {
    stopped: bool,
    completed: bool,
    label: &'static str,
    percent: Option<u8>,
    generation: u64,
}

struct WorkerConfig {
    label: &'static str,
    percent: Option<u8>,
    show_line: bool,
    report_osc: bool,
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
        Self::install_interrupt_handler(indicator, visible)
    }

    pub fn start_review(verbose: bool) -> Self {
        let terminal = io::stderr().is_terminal();
        let indicator = if terminal {
            Self::start_worker(
                WorkerConfig {
                    label: "Preparing review branch",
                    percent: Some(0),
                    show_line: !verbose,
                    report_osc: true,
                },
                Duration::from_millis(200),
                Duration::from_millis(100),
                io::stderr(),
            )
        } else {
            Self::disabled()
        };
        Self::install_interrupt_handler(indicator, terminal)
    }

    fn install_interrupt_handler(indicator: Self, visible: bool) -> Self {
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
                    current: Mutex::new(IndicatorState {
                        stopped: true,
                        completed: false,
                        label: "",
                        percent: None,
                        generation: 0,
                    }),
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
        writer: W,
    ) -> Self
    where
        W: Write + Send + 'static,
    {
        Self::start_worker(
            WorkerConfig {
                label,
                percent: None,
                show_line: true,
                report_osc: false,
            },
            delay,
            interval,
            writer,
        )
    }

    fn start_worker<W>(
        config: WorkerConfig,
        delay: Duration,
        interval: Duration,
        mut writer: W,
    ) -> Self
    where
        W: Write + Send + 'static,
    {
        let state = Arc::new(WorkerState {
            current: Mutex::new(IndicatorState {
                stopped: false,
                completed: false,
                label: config.label,
                percent: config.percent,
                generation: 0,
            }),
            wake: Condvar::new(),
        });
        let worker_state = Arc::clone(&state);
        let worker = thread::spawn(move || {
            let current = worker_state.current.lock().unwrap();
            let (current, _) = worker_state
                .wake
                .wait_timeout_while(current, delay, |current| !current.stopped)
                .unwrap();
            let stopped_before_delay = current.stopped;
            drop(current);

            const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut frame_index = 0;
            let mut rendered = false;
            let mut reported = None;

            if !stopped_before_delay {
                loop {
                    let current = worker_state.current.lock().unwrap();
                    if current.stopped {
                        break;
                    }
                    let generation = current.generation;
                    let percent = current.percent;
                    let line = match percent {
                        Some(percent) => format!(
                            "\r\x1b[2K{} {} {}",
                            FRAMES[frame_index],
                            current.label,
                            progress_bar(percent)
                        ),
                        None => format!("\r{} {}", FRAMES[frame_index], current.label),
                    };
                    drop(current);

                    if config.report_osc && percent != reported {
                        if let Some(percent) = percent {
                            if write!(writer, "\x1b]9;4;1;{percent}\x07").is_err() {
                                break;
                            }
                            reported = Some(percent);
                        }
                    }
                    if config.show_line {
                        if writer.write_all(line.as_bytes()).is_err() {
                            break;
                        }
                        rendered = true;
                        frame_index = (frame_index + 1) % FRAMES.len();
                    }
                    if writer.flush().is_err() {
                        break;
                    }

                    let current = worker_state.current.lock().unwrap();
                    let (current, _) = worker_state
                        .wake
                        .wait_timeout_while(current, interval, |current| {
                            !current.stopped && current.generation == generation
                        })
                        .unwrap();
                    if current.stopped {
                        break;
                    }
                }
            }

            let completed = worker_state.current.lock().unwrap().completed;
            if config.report_osc && completed && reported != Some(100) {
                let _ = writer.write_all(b"\x1b]9;4;1;100\x07");
                reported = Some(100);
            }
            if rendered {
                let _ = write!(writer, "\r\x1b[2K");
            }
            if config.report_osc && reported.is_some() {
                let _ = writer.write_all(b"\x1b]9;4;0;0\x07");
            }
            let _ = writer.flush();
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
            let mut current = self.inner.state.current.lock().unwrap();
            current.stopped = true;
            self.inner.state.wake.notify_all();
        }

        if let Some(worker) = self.inner.worker.lock().unwrap().take() {
            worker.join().unwrap();
        }
    }

    pub fn update(&self, percent: u8) {
        let mut current = self.inner.state.current.lock().unwrap();
        if current.stopped {
            return;
        }
        current.percent = Some(percent.max(current.percent.unwrap_or(0)).min(100));
        current.generation += 1;
        self.inner.state.wake.notify_all();
    }

    pub fn complete(&self) {
        {
            let mut current = self.inner.state.current.lock().unwrap();
            current.completed = true;
        }
        self.finish();
    }
}

impl Drop for WaitIndicator {
    fn drop(&mut self) {
        self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::{should_show, WaitIndicator, WorkerConfig};
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

    #[test]
    fn review_progress_is_monotonic_and_clears_terminal_state() {
        let writer = SharedWriter::default();
        let indicator = WaitIndicator::start_worker(
            WorkerConfig {
                label: "Preparing review branch",
                percent: Some(0),
                show_line: true,
                report_osc: true,
            },
            Duration::ZERO,
            Duration::from_millis(5),
            writer.clone(),
        );
        let wait_for = |needle: &str| {
            let deadline = Instant::now() + Duration::from_secs(1);
            while !String::from_utf8(writer.bytes()).unwrap().contains(needle) {
                assert!(Instant::now() < deadline, "missing {needle:?}");
                thread::sleep(Duration::from_millis(1));
            }
        };

        wait_for("\x1b]9;4;1;0\x07");
        indicator.update(40);
        wait_for("\x1b]9;4;1;40\x07");
        indicator.update(10);
        indicator.complete();

        let output = String::from_utf8(writer.bytes()).unwrap();
        assert!(output.contains("Preparing review branch [========>           ]"));
        assert!(!output.contains("40%"));
        assert!(!output.contains("Validating review range"));
        assert!(!output.contains("\x1b]9;4;1;10\x07"));
        assert!(output.contains("\x1b]9;4;1;100\x07"));
        assert!(output.ends_with("\r\x1b[2K\x1b]9;4;0;0\x07"));
    }
}
