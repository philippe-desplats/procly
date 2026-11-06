//! The Dock icon, and the one rule of macOS it has to live with.
//!
//! Tao, the windowing layer under Tauri, ignores a request to hide the Dock icon that comes less than a second after
//! a request to show it, because macOS leaves a second icon behind when the two follow each other too fast. The
//! request is dropped with no error, so the icon stays. A window that opens at launch and a developer who turns "Show
//! in Dock" off a moment later hit exactly that. [`DockIcon`] plans around it: a hide that comes too soon waits for
//! the rest of the second, and a show that comes meanwhile cancels it.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long a hide has to follow a show. Tao ignores it for 1 second, and the margin covers the time a request takes
/// to reach the main thread, since tao measures from there.
pub(crate) const SETTLE: Duration = Duration::from_millis(1500);

/// What to do with the Dock icon right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Show,
    Hide,
    /// Too soon after a show to hide it: ask again once this long has passed.
    Wait(Duration),
    /// Nothing to do, the icon was asked to show again while this request waited.
    Nothing,
}

/// What the Dock icon was last asked to be, and when it was last shown.
#[derive(Debug, Default)]
pub(crate) struct DockIcon {
    wanted: bool,
    last_show: Option<Instant>,
}

impl DockIcon {
    /// The icon is asked to show or hide.
    pub(crate) fn want(&mut self, visible: bool, now: Instant) -> Step {
        self.wanted = visible;
        if visible {
            self.last_show = Some(now);
            Step::Show
        } else {
            self.hide_when_settled(now)
        }
    }

    /// A wait ended. The icon hides unless it was asked to show again meanwhile, and waits again if that show came
    /// less than a second ago.
    pub(crate) fn settle(&self, now: Instant) -> Step {
        if self.wanted {
            Step::Nothing
        } else {
            self.hide_when_settled(now)
        }
    }

    fn hide_when_settled(&self, now: Instant) -> Step {
        let left = self.last_show.map_or(Duration::ZERO, |shown| {
            SETTLE.saturating_sub(now.saturating_duration_since(shown))
        });
        if left.is_zero() { Step::Hide } else { Step::Wait(left) }
    }
}

/// What actually shows and hides the Dock icon.
pub(crate) trait Raw {
    fn set_visible(&self, visible: bool);
}

/// Asks for the icon to show or hide. Returns how long to wait before calling [`settle`] when the hide came too soon.
pub(crate) fn request(icon: &Mutex<DockIcon>, raw: &impl Raw, visible: bool, now: Instant) -> Option<Duration> {
    let step = icon.lock().unwrap_or_else(PoisonError::into_inner).want(visible, now);
    run(step, raw)
}

/// A wait of [`request`] ended. Returns how long to wait again when a show came meanwhile and the hide is still too soon.
pub(crate) fn settle(icon: &Mutex<DockIcon>, raw: &impl Raw, now: Instant) -> Option<Duration> {
    let step = icon.lock().unwrap_or_else(PoisonError::into_inner).settle(now);
    run(step, raw)
}

fn run(step: Step, raw: &impl Raw) -> Option<Duration> {
    match step {
        Step::Show => raw.set_visible(true),
        Step::Hide => raw.set_visible(false),
        Step::Wait(left) => return Some(left),
        Step::Nothing => {}
    }
    None
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// The Dock as tao treats it: a hide less than a second after a show is dropped with no error.
    struct TaoDock {
        now: Cell<Instant>,
        visible: Cell<bool>,
        last_show: Cell<Option<Instant>>,
        dropped: Cell<u32>,
        calls: RefCell<Vec<bool>>,
    }

    impl TaoDock {
        fn new(start: Instant) -> Self {
            Self {
                now: Cell::new(start),
                visible: Cell::new(false),
                last_show: Cell::new(None),
                dropped: Cell::new(0),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn pass(&self, time: Duration) {
            self.now.set(self.now.get() + time);
        }
    }

    impl Raw for TaoDock {
        fn set_visible(&self, visible: bool) {
            self.calls.borrow_mut().push(visible);
            if visible {
                self.last_show.set(Some(self.now.get()));
                self.visible.set(true);
            } else if self
                .last_show
                .get()
                .is_some_and(|shown| self.now.get().duration_since(shown) < Duration::from_secs(1))
            {
                self.dropped.set(self.dropped.get() + 1);
            } else {
                self.visible.set(false);
            }
        }
    }

    fn icon() -> Mutex<DockIcon> {
        Mutex::new(DockIcon::default())
    }

    #[test]
    fn tao_drops_a_hide_that_follows_a_show_too_closely() {
        let tao = TaoDock::new(Instant::now());

        tao.set_visible(true);
        tao.pass(Duration::from_millis(200));
        tao.set_visible(false);

        assert!(tao.visible.get(), "the model of tao keeps the icon");
        assert_eq!(tao.dropped.get(), 1);
    }

    #[test]
    fn turning_the_dock_off_just_after_the_window_opens_still_hides_the_icon() {
        let start = Instant::now();
        let tao = TaoDock::new(start);
        let icon = icon();

        // The app starts with no icon, opens its window, and the developer turns "Show in Dock" off 200 ms later.
        assert_eq!(request(&icon, &tao, false, tao.now.get()), None);
        assert_eq!(request(&icon, &tao, true, tao.now.get()), None);
        tao.pass(Duration::from_millis(200));
        let wait = request(&icon, &tao, false, tao.now.get()).expect("a hide this soon waits");
        assert!(tao.visible.get(), "the icon is still there while the hide waits");
        tao.pass(wait);
        let again = settle(&icon, &tao, tao.now.get());

        assert_eq!(again, None);
        assert!(!tao.visible.get(), "the icon is gone once the wait is over");
        assert_eq!(tao.dropped.get(), 0, "tao dropped nothing");
    }

    #[test]
    fn a_hide_long_after_the_show_applies_at_once() {
        let tao = TaoDock::new(Instant::now());
        let icon = icon();

        request(&icon, &tao, true, tao.now.get());
        tao.pass(SETTLE);

        assert_eq!(request(&icon, &tao, false, tao.now.get()), None);
        assert!(!tao.visible.get());
        assert_eq!(*tao.calls.borrow(), [true, false]);
    }

    #[test]
    fn the_first_hide_of_a_launch_applies_at_once() {
        let tao = TaoDock::new(Instant::now());

        assert_eq!(request(&icon(), &tao, false, tao.now.get()), None);

        assert_eq!(*tao.calls.borrow(), [false]);
    }

    #[test]
    fn a_show_that_comes_while_a_hide_waits_cancels_the_hide() {
        let tao = TaoDock::new(Instant::now());
        let icon = icon();
        request(&icon, &tao, true, tao.now.get());
        tao.pass(Duration::from_millis(200));
        let wait = request(&icon, &tao, false, tao.now.get()).expect("a hide this soon waits");
        tao.pass(Duration::from_millis(300));
        request(&icon, &tao, true, tao.now.get());
        tao.pass(wait);

        assert_eq!(settle(&icon, &tao, tao.now.get()), None);

        assert!(tao.visible.get());
        assert_eq!(*tao.calls.borrow(), [true, true]);
    }

    #[test]
    fn a_hide_that_waits_across_a_second_show_waits_for_that_show_too() {
        let tao = TaoDock::new(Instant::now());
        let icon = icon();
        request(&icon, &tao, true, tao.now.get());
        tao.pass(Duration::from_millis(200));
        let first = request(&icon, &tao, false, tao.now.get()).expect("a hide this soon waits");
        tao.pass(Duration::from_millis(300));
        request(&icon, &tao, true, tao.now.get());
        tao.pass(Duration::from_millis(100));
        request(&icon, &tao, false, tao.now.get()).expect("a hide this soon waits");

        // The first wait ends while the second show is only 1 s old, so it waits again instead of being dropped.
        tao.pass(first.saturating_sub(Duration::from_millis(400)));
        let again = settle(&icon, &tao, tao.now.get()).expect("the second show is still recent");
        tao.pass(again);

        assert_eq!(settle(&icon, &tao, tao.now.get()), None);
        assert!(!tao.visible.get());
        assert_eq!(tao.dropped.get(), 0);
    }
}
