//! The one poller of the app: a single task that keeps the window current by asking the Daemon for `Status { all:
//! true }`, and forwards each answer unchanged (`docs/rules/tauri.md`). It owns the cadence, the last answer and its
//! time, and it only ever sends a `Status` or connects, it stops nothing.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use procly_client::protocol::Response;
use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, sleep_until};

use crate::daemon::CommandError;

/// Where the window stands, which sets how often the Daemon is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowState {
    /// Shown and in front of the other windows.
    Front,
    /// Shown, but another window is in front of it.
    Visible,
    /// Closed, hidden or minimized, so nobody looks at it.
    Closed,
}

/// How many times the duration of the last `Status` the poller waits before the next one at least.
const PERFORMANCE_FACTOR: u32 = 4;

impl WindowState {
    /// The wait between two `Status` while the Daemon answers.
    pub(crate) fn status_interval(self) -> Duration {
        match self {
            Self::Front => Duration::from_secs(2),
            Self::Visible => Duration::from_secs(10),
            Self::Closed => Duration::from_secs(30),
        }
    }

    /// The state of a window from what Tauri reports about it.
    pub(crate) fn from_flags(visible: bool, minimized: bool, focused: bool) -> Self {
        match (visible && !minimized, focused) {
            (false, _) => Self::Closed,
            (true, true) => Self::Front,
            (true, false) => Self::Visible,
        }
    }

    /// The wait between two attempts to connect while the Daemon does not answer.
    pub(crate) fn connect_interval(self) -> Duration {
        match self {
            Self::Front | Self::Visible => Duration::from_secs(5),
            Self::Closed => Duration::from_secs(30),
        }
    }
}

/// The wait before the next tick, from the end of the last one.
///
/// While the Daemon answers, the interval of the window state, stretched to 4 times the duration of the last
/// `Status` when that is longer. Without a Daemon, the interval to connect.
pub(crate) fn next_delay(window: WindowState, connected: bool, last_status: Option<Duration>) -> Duration {
    if !connected {
        return window.connect_interval();
    }
    let interval = window.status_interval();
    last_status.map_or(interval, |last| interval.max(last * PERFORMANCE_FACTOR))
}

/// What reaches the poller from the rest of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Message {
    /// ⌘R or the Refresh button: ask for a tick now.
    Refresh,
    /// The window moved to another state.
    Window(WindowState),
    /// The Mac goes to sleep.
    Sleep,
    /// The Mac woke up.
    Wake,
}

/// What the Daemon answered at `at_ms`, or why it did not, as the frontend receives it. The `Response` is the one
/// the Daemon wrote.
#[derive(Debug, Serialize)]
pub(crate) struct Update {
    /// When the answer came, in milliseconds since the Unix epoch.
    pub(crate) at_ms: u64,
    #[serde(flatten)]
    pub(crate) answer: Answer,
}

/// The two ways a tick ends.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Answer {
    Response(Response),
    Failure(CommandError),
}

/// The two things the poller does with the Daemon. It cannot start it, stop it or change anything in it.
pub(crate) trait Source: Send + Sync + 'static {
    /// A bare connect, which tells whether a Daemon accepts connections.
    fn reachable(&self) -> impl Future<Output = bool> + Send;
    /// One `Status { all: true }`.
    fn status(&self) -> impl Future<Output = Result<Response, CommandError>> + Send;
}

/// What a message changes in the schedule.
#[derive(Debug, PartialEq, Eq)]
enum Effect {
    /// Nothing.
    None,
    /// The wait before the next tick is worked out again.
    Retime,
    /// A tick is due now.
    Tick,
}

/// What the poller knows between two ticks.
struct Schedule {
    window: WindowState,
    asleep: bool,
    /// Whether the last tick found a Daemon. A poller starts by assuming one runs, so that its first tick is a
    /// `Status` and the window learns at once that none does.
    connected: bool,
    /// How long the last `Status` took.
    last_status: Option<Duration>,
}

impl Schedule {
    fn delay(&self) -> Duration {
        next_delay(self.window, self.connected, self.last_status)
    }

    fn receive(&mut self, message: Message) -> Effect {
        match message {
            // The Mac cannot be asked anything while it sleeps, and the wake ticks anyway.
            Message::Refresh if self.asleep => Effect::None,
            Message::Refresh => Effect::Tick,
            Message::Window(window) => {
                self.window = window;
                Effect::Retime
            }
            Message::Sleep => {
                self.asleep = true;
                Effect::None
            }
            Message::Wake => {
                self.asleep = false;
                Effect::Tick
            }
        }
    }

    /// One tick: a connect first when the Daemon did not answer last time, then the `Status`. What it returns is
    /// what the window has to be told, nothing when a connect found no Daemon since the window already knows.
    async fn tick<S: Source>(&mut self, source: &S) -> Option<Update> {
        if !self.connected && !source.reachable().await {
            return None;
        }
        let started = Instant::now();
        let answer = source.status().await;
        match &answer {
            Err(error) if error.is_unreachable() => self.connected = false,
            Ok(_) | Err(_) => {
                self.connected = true;
                self.last_status = Some(started.elapsed());
            }
        }
        Some(Update::now(match answer {
            Ok(response) => Answer::Response(response),
            Err(error) => Answer::Failure(error),
        }))
    }
}

impl Update {
    fn now(answer: Answer) -> Self {
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX));
        Self { at_ms, answer }
    }
}

/// How often the wall clock is read to notice that the Mac slept.
const SLEEP_WATCH_PERIOD: Duration = Duration::from_secs(1);

/// How far past its period a read of the wall clock has to land to count as a sleep, which a busy machine does not
/// reach.
const SLEEP_GAP: Duration = Duration::from_secs(5);

/// Whether the wall clock advanced much more between two reads than the period between them, which only a sleep
/// does: the timers of a sleeping Mac stand still while its clock does not.
fn slept_through(before: SystemTime, after: SystemTime) -> bool {
    after
        .duration_since(before)
        .is_ok_and(|elapsed| elapsed > SLEEP_WATCH_PERIOD + SLEEP_GAP)
}

/// Tells the poller when the Mac slept and woke, until the poller is gone.
///
/// The system notifications of a sleep need native code the app does not carry, so the sleep is noticed when it
/// ends: the first read of the clock after a wake has jumped. The poller is told of both, so it ticks at once.
pub(crate) async fn watch_power(messages: mpsc::UnboundedSender<Message>) {
    let mut before = SystemTime::now();
    loop {
        tokio::time::sleep(SLEEP_WATCH_PERIOD).await;
        let after = SystemTime::now();
        if slept_through(before, after) {
            if messages.send(Message::Sleep).is_err() || messages.send(Message::Wake).is_err() {
                return;
            }
        } else if messages.is_closed() {
            return;
        }
        before = after;
    }
}

/// The poller as the rest of the app holds it: a way to talk to it and to read its last answer.
pub(crate) struct Poller {
    messages: mpsc::UnboundedSender<Message>,
    updates: watch::Receiver<Option<Arc<Update>>>,
}

impl Poller {
    /// Spawns the one poller task of the app, and the watch that tells it when the Mac wakes.
    pub(crate) fn start<S: Source>(source: S, window: WindowState) -> Self {
        let (messages, receiver) = mpsc::unbounded_channel();
        let (sender, updates) = watch::channel(None);
        tauri::async_runtime::spawn(run(source, window, receiver, sender));
        tauri::async_runtime::spawn(watch_power(messages.clone()));
        Self { messages, updates }
    }

    /// Asks for a tick now. It never starts, stops or restarts anything.
    pub(crate) fn refresh(&self) {
        self.tell(Message::Refresh);
    }

    /// Tells the poller where the window stands.
    pub(crate) fn window(&self, window: WindowState) {
        self.tell(Message::Window(window));
    }

    /// The last answer and its time, nothing before the first tick ended.
    pub(crate) fn last(&self) -> Option<Arc<Update>> {
        self.updates.borrow().clone()
    }

    /// A receiver that wakes on each new answer, for the task that hands them to the window.
    pub(crate) fn subscribe(&self) -> watch::Receiver<Option<Arc<Update>>> {
        self.updates.clone()
    }

    fn tell(&self, message: Message) {
        // The poller ends with the app, a message sent after that has nobody to reach.
        let _ = self.messages.send(message);
    }
}

/// Runs the poller until every sender of `messages` is gone, publishing each answer to `updates`.
///
/// One `Status` is in flight at a time, since the task awaits it. A refresh that arrives while it runs asks for a
/// tick that is already running, so it is dropped.
pub(crate) async fn run<S: Source>(
    source: S,
    window: WindowState,
    mut messages: mpsc::UnboundedReceiver<Message>,
    updates: watch::Sender<Option<Arc<Update>>>,
) {
    let mut schedule = Schedule {
        window,
        asleep: false,
        connected: true,
        last_status: None,
    };
    let mut finished = Instant::now();
    let mut due = finished;
    loop {
        loop {
            tokio::select! {
                () = sleep_until(due), if !schedule.asleep => break,
                message = messages.recv() => {
                    let Some(message) = message else { return };
                    match schedule.receive(message) {
                        Effect::None => {}
                        Effect::Retime => due = finished + schedule.delay(),
                        Effect::Tick => break,
                    }
                }
            }
        }

        if let Some(update) = schedule.tick(&source).await {
            updates.send_replace(Some(Arc::new(update)));
        }
        finished = Instant::now();
        due = finished + schedule.delay();

        loop {
            match messages.try_recv() {
                Ok(Message::Refresh) => {}
                Ok(message) => match schedule.receive(message) {
                    Effect::None => {}
                    Effect::Retime => due = finished + schedule.delay(),
                    Effect::Tick => due = finished,
                },
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_in_front_asks_every_2_seconds_a_visible_one_every_10_and_a_closed_one_every_30() {
        assert_eq!(next_delay(WindowState::Front, true, None), Duration::from_secs(2));
        assert_eq!(next_delay(WindowState::Visible, true, None), Duration::from_secs(10));
        assert_eq!(next_delay(WindowState::Closed, true, None), Duration::from_secs(30));
    }

    #[test]
    fn without_a_daemon_it_connects_every_5_seconds_with_the_window_shown_and_every_30_otherwise() {
        assert_eq!(next_delay(WindowState::Front, false, None), Duration::from_secs(5));
        assert_eq!(next_delay(WindowState::Visible, false, None), Duration::from_secs(5));
        assert_eq!(next_delay(WindowState::Closed, false, None), Duration::from_secs(30));
    }

    #[test]
    fn a_slow_status_stretches_the_wait_to_4_times_its_duration() {
        let slow = Some(Duration::from_millis(900));

        assert_eq!(next_delay(WindowState::Front, true, slow), Duration::from_millis(3600));
        // Quick enough that the interval of the window state is the longer one.
        assert_eq!(
            next_delay(WindowState::Front, true, Some(Duration::from_millis(100))),
            Duration::from_secs(2)
        );
        // The guard is about Status: a connect that fails has no duration to stretch by.
        assert_eq!(next_delay(WindowState::Front, false, slow), Duration::from_secs(5));
    }

    #[test]
    fn a_window_is_in_front_when_shown_and_focused_visible_when_shown_and_closed_otherwise() {
        assert_eq!(WindowState::from_flags(true, false, true), WindowState::Front);
        assert_eq!(WindowState::from_flags(true, false, false), WindowState::Visible);
        // Hidden, as a window closed to the menu bar is, and minimized, nobody can look at it.
        assert_eq!(WindowState::from_flags(false, false, false), WindowState::Closed);
        assert_eq!(WindowState::from_flags(true, true, false), WindowState::Closed);
        assert_eq!(WindowState::from_flags(true, true, true), WindowState::Closed);
    }

    #[test]
    fn a_clock_that_jumped_far_ahead_of_its_ticks_means_the_mac_slept() {
        let before = UNIX_EPOCH + Duration::from_secs(1_000);

        assert!(!slept_through(before, before + SLEEP_WATCH_PERIOD));
        assert!(!slept_through(
            before,
            before + SLEEP_WATCH_PERIOD + Duration::from_secs(2)
        ));
        assert!(slept_through(before, before + Duration::from_secs(600)));
        // A clock set back is not a sleep.
        assert!(!slept_through(before, before - Duration::from_secs(600)));
    }

    use std::io;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use procly_client::ClientError;
    use procly_client::protocol::StatusReport;
    use tokio::time::sleep;

    /// What the poller asked of the fake Daemon, and when, in whole milliseconds after the start of the test.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Call {
        Connect(u64),
        Status(u64),
    }

    /// A Daemon that answers `Status` after `takes`, or does not run.
    #[derive(Clone)]
    struct Fake {
        start: Instant,
        up: Arc<AtomicBool>,
        takes: Duration,
        calls: Arc<Mutex<Vec<Call>>>,
    }

    impl Fake {
        fn new(up: bool, takes: Duration) -> Self {
            Self {
                start: Instant::now(),
                up: Arc::new(AtomicBool::new(up)),
                takes,
                calls: Arc::default(),
            }
        }

        fn ms(&self) -> u64 {
            u64::try_from(self.start.elapsed().as_millis()).expect("a test lasts less than 584 million years")
        }

        fn calls(&self) -> Vec<Call> {
            self.calls
                .lock()
                .expect("no test panics while holding the lock")
                .clone()
        }

        fn statuses(&self) -> Vec<u64> {
            self.calls()
                .into_iter()
                .filter_map(|call| if let Call::Status(at) = call { Some(at) } else { None })
                .collect()
        }

        fn record(&self, call: Call) {
            self.calls
                .lock()
                .expect("no test panics while holding the lock")
                .push(call);
        }
    }

    impl Source for Fake {
        fn reachable(&self) -> impl Future<Output = bool> + Send {
            self.record(Call::Connect(self.ms()));
            std::future::ready(self.up.load(Ordering::SeqCst))
        }

        async fn status(&self) -> Result<Response, CommandError> {
            self.record(Call::Status(self.ms()));
            sleep(self.takes).await;
            if self.up.load(Ordering::SeqCst) {
                Ok(Response::Status(StatusReport {
                    repositories: Vec::new(),
                    jobs_outside_repositories: Vec::new(),
                }))
            } else {
                Err(CommandError::Client(ClientError::Unreachable {
                    path: PathBuf::from("/p/state/procly.sock"),
                    source: io::Error::from(io::ErrorKind::NotFound),
                }))
            }
        }
    }

    /// A poller running on the paused clock of the test.
    struct Rig {
        messages: mpsc::UnboundedSender<Message>,
        updates: watch::Receiver<Option<Arc<Update>>>,
        daemon: Fake,
    }

    fn start(window: WindowState, daemon: Fake) -> Rig {
        let (messages, receiver) = mpsc::unbounded_channel();
        let (sender, updates) = watch::channel(None);
        tokio::spawn(run(daemon.clone(), window, receiver, sender));
        Rig {
            messages,
            updates,
            daemon,
        }
    }

    impl Rig {
        fn send(&self, message: Message) {
            self.messages.send(message).expect("the poller still runs");
        }
    }

    /// Lets the test clock run for `ms`, which runs whatever the poller has due in that time.
    async fn pass(ms: u64) {
        sleep(Duration::from_millis(ms)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_window_in_front_gets_a_status_at_once_then_every_2_seconds() {
        let rig = start(WindowState::Front, Fake::new(true, Duration::ZERO));

        pass(6_500).await;

        assert_eq!(rig.daemon.statuses(), [0, 2_000, 4_000, 6_000]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_closed_window_gets_a_status_every_30_seconds() {
        let rig = start(WindowState::Closed, Fake::new(true, Duration::ZERO));

        pass(31_000).await;

        assert_eq!(rig.daemon.statuses(), [0, 30_000]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_window_coming_to_the_front_is_not_left_waiting_for_the_slow_interval_it_had() {
        let rig = start(WindowState::Closed, Fake::new(true, Duration::ZERO));
        pass(3_000).await;

        rig.send(Message::Window(WindowState::Front));
        pass(100).await;

        assert_eq!(rig.daemon.statuses(), [0, 3_000]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_status_makes_the_next_one_wait_4_times_as_long_as_it_took() {
        let rig = start(WindowState::Front, Fake::new(true, Duration::from_millis(1_500)));

        pass(10_000).await;

        // The first ends at 1.5 s, the next waits 6 s from there.
        assert_eq!(rig.daemon.statuses(), [0, 7_500]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refresh_asks_for_a_status_at_once() {
        let rig = start(WindowState::Visible, Fake::new(true, Duration::ZERO));
        pass(1_000).await;

        rig.send(Message::Refresh);
        pass(100).await;

        assert_eq!(rig.daemon.statuses(), [0, 1_000]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_tick_asked_for_during_a_request_is_skipped() {
        let rig = start(WindowState::Front, Fake::new(true, Duration::from_secs(3)));
        pass(1_000).await;

        rig.send(Message::Refresh);
        pass(15_000).await;

        // The refresh found a request in flight, so it asked for nothing more, and the guard waits 12 s.
        assert_eq!(rig.daemon.statuses(), [0, 15_000]);
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_daemon_it_only_connects_then_asks_for_the_status_once_one_answers() {
        let rig = start(WindowState::Front, Fake::new(false, Duration::ZERO));
        pass(12_000).await;
        rig.daemon.up.store(true, Ordering::SeqCst);
        pass(4_000).await;

        // One Status to learn there is no Daemon, then a bare connect every 5 s, and the Status at once on the answer.
        assert_eq!(
            rig.daemon.calls(),
            [
                Call::Status(0),
                Call::Connect(5_000),
                Call::Connect(10_000),
                Call::Connect(15_000),
                Call::Status(15_000),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_poller_pauses_while_the_mac_sleeps_and_asks_at_once_when_it_wakes() {
        let rig = start(WindowState::Front, Fake::new(true, Duration::ZERO));
        pass(1_000).await;

        rig.send(Message::Sleep);
        pass(60_000).await;
        let asleep = rig.daemon.statuses();
        rig.send(Message::Wake);
        pass(100).await;

        assert_eq!(asleep, [0]);
        assert_eq!(rig.daemon.statuses(), [0, 61_000]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sleep_and_a_wake_that_arrive_during_a_request_still_end_in_a_tick() {
        let rig = start(WindowState::Front, Fake::new(true, Duration::from_secs(1)));
        pass(500).await;

        rig.send(Message::Sleep);
        rig.send(Message::Wake);
        pass(1_100).await;

        assert_eq!(rig.daemon.statuses(), [0, 1_000]);
    }

    #[tokio::test(start_paused = true)]
    async fn each_answer_is_published_unchanged_with_its_time_and_a_missing_daemon_is_published_as_a_failure() {
        let mut rig = start(WindowState::Front, Fake::new(true, Duration::ZERO));
        pass(100).await;

        let update = rig
            .updates
            .borrow_and_update()
            .clone()
            .expect("the first answer is published");
        assert!(update.at_ms > 0);
        let Answer::Response(Response::Status(report)) = &update.answer else {
            panic!("the answer of the Daemon is published as it came: {update:?}");
        };
        assert!(report.repositories.is_empty());

        rig.daemon.up.store(false, Ordering::SeqCst);
        pass(2_000).await;

        let update = rig.updates.borrow_and_update().clone().expect("a failure is published");
        let json = serde_json::to_value(&*update).expect("the update serializes");
        assert_eq!(json["failure"]["kind"], "unreachable");
        assert!(json["at_ms"].as_u64().is_some_and(|at| at > 0), "{json}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_poller_ends_with_the_app_and_has_asked_for_nothing_but_a_status_or_a_connect() {
        let rig = start(WindowState::Front, Fake::new(true, Duration::ZERO));
        pass(100).await;
        let Rig { messages, daemon, .. } = rig;

        drop(messages);
        pass(10_000).await;

        // Quitting stops nothing: the only things the poller can ask of a Daemon are in `Source`.
        assert_eq!(daemon.calls(), [Call::Status(0)]);
    }
}
