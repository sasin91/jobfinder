//! Whether a game runs, which background work shouldn't slow down: a game, a presentation, or
//! another full-screen app. Each signal the computer gives is asked in turn, and the first that
//! says so is why (Playing):
//!
//! - Windows' own "do not disturb" state (SHQueryUserNotificationState, which Windows also uses
//!   to hold back notifications): exclusive full-screen games and presentations.
//! - The app in front is one the Xbox Game Bar knows as a game (its list in
//!   HKCU\System\GameConfigStore\Children, the one Windows' Game Mode goes by). This catches
//!   the borderless windowed games the first misses.
//! - The app in front is in a folder the Epic Games launcher or Battle.net installed a game in
//!   (launchers): neither says which game runs, as Steam does.
//! - A program keeps the graphics card's 3D engine busy, a quarter or more since the last look,
//!   in front or behind the window the user switched to (gpu): any game, by what it does rather
//!   than its name (Windows only).
//! - Steam runs a game: its RunningAppID (Windows' registry, or ~/.steam/registry.vdf).
//! - Feral's gamemode runs a game (its ClientCount on the session D-Bus), which games started
//!   with `gamemoderun` (Steam's launch option `gamemoderun %command%`) ask for on Linux.
//!
//! Windows' Game Mode itself can't be asked from outside: HasExpandedResources answers for the
//! calling process only.
//!
//! The automatic answer is kept for LOOK_AGAIN_AFTER (GameMode, one per Context), so calls of a
//! scan don't each read the registry or D-Bus. While a game runs, work that would run on this
//! computer runs on the user's cloud key (stand_in); without one the job scan waits until the
//! game ends (wait_until_over). The models let go of the graphics card as soon as no call needs
//! them (idle).

#[cfg(any(windows, test))]
mod gpu;
#[cfg(any(windows, test))]
mod launchers;
#[cfg(target_os = "linux")]
mod linux;
pub mod stand_in;
mod steam;
#[cfg(windows)]
mod windows;

use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// How often the desktop checks for games and a waiting scan checks whether it can resume.
pub(crate) const LOOK_EVERY: Duration = Duration::from_secs(30);

/// Share one detection result between the desktop monitor and background work.
const LOOK_AGAIN_AFTER: Duration = Duration::from_secs(10);

/// Why a game counts as running: the first signal that said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Playing {
    Manual,
    /// Windows' "do not disturb" state: a full-screen game, a presentation or another
    /// full-screen app.
    FullScreen,
    /// The app in front is in the Xbox Game Bar's list of games: its program file's name.
    KnownGame {
        program: String,
    },
    /// The app in front is a game this launcher installed (launchers).
    LauncherGame {
        launcher: &'static str,
        game: String,
    },
    /// A program (its file's name) kept this share of the 3D engine busy (gpu).
    GpuHeavy {
        program: String,
        share: u32,
    },
    /// Steam runs the game with this app ID.
    Steam {
        app_id: u32,
    },
    /// Feral's gamemode runs for this many games.
    GameMode {
        games: i32,
    },
}

impl fmt::Display for Playing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Playing::Manual => f.write_str("manual game mode is on"),
            Playing::FullScreen => f.write_str("a full-screen app or game runs"),
            Playing::KnownGame { program } => write!(f, "the game {program} is in front"),
            Playing::LauncherGame { launcher, game } => write!(f, "{game} ({launcher}) is in front"),
            Playing::GpuHeavy { program, share } => {
                write!(f, "{program} keeps the graphics card busy ({share} %)")
            }
            Playing::Steam { app_id } => write!(f, "Steam runs a game (app {app_id})"),
            Playing::GameMode { games: 1 } => f.write_str("gamemode runs a game"),
            Playing::GameMode { games } => write!(f, "gamemode runs {games} games"),
        }
    }
}

/// What one signal says: Some when a game runs, Err when it couldn't be asked.
type Signal = fn() -> Result<Option<Playing>, String>;

/// Whether a game runs, as last looked: kept for LOOK_AGAIN_AFTER.
#[derive(Default)]
pub struct GameMode {
    watch: Mutex<Watch>,
    manual: AtomicBool,
    detected: watch::Sender<Option<Playing>>,
}

/// The last look, and the GPU counters, whose shares are averaged since the look before.
#[derive(Default)]
struct Watch {
    last_look: Option<Look>,
    gpu: GpuWatch,
}

#[cfg(windows)]
use windows::GpuWatch;

/// No GPU counters per process off Windows (gpu).
#[cfg(not(windows))]
#[derive(Default)]
struct GpuWatch;

#[cfg(not(windows))]
impl GpuWatch {
    fn heavy_app(&mut self) -> Result<Option<Playing>, String> {
        Ok(None)
    }
}

/// One look at the signals.
#[derive(Clone)]
struct Look {
    at: Instant,
    playing: Option<Playing>,
}

impl GameMode {
    pub fn manual(&self) -> bool {
        self.manual.load(Ordering::Relaxed)
    }

    pub fn set_manual(&self, enabled: bool) {
        self.manual.store(enabled, Ordering::Relaxed);
    }

    pub(crate) fn detected(&self) -> watch::Receiver<Option<Playing>> {
        self.detected.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn cache_detection_for_test(&self, playing: Option<Playing>) {
        self.watch.lock().unwrap().last_look = Some(Look {
            at: Instant::now(),
            playing,
        });
    }

    /// Whether a game runs now (why_running).
    pub fn running(&self) -> bool {
        self.why_running().is_some()
    }

    /// Why a game runs now, or None; the last look's answer while it is younger than
    /// LOOK_AGAIN_AFTER. A signal that couldn't be asked is logged and counts as saying nothing.
    pub fn why_running(&self) -> Option<Playing> {
        let detected = self.detect();
        if self.manual() { Some(Playing::Manual) } else { detected }
    }

    fn detect(&self) -> Option<Playing> {
        let mut watch = self.watch.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(look) = watch
            .last_look
            .as_ref()
            .filter(|look| look.at.elapsed() < LOOK_AGAIN_AFTER)
        {
            return look.playing.clone();
        }
        // The counters are read at every look, so their average covers the time since the last.
        let heavy = watch.gpu.heavy_app().unwrap_or_else(|why| {
            crate::log_line!("Couldn't tell whether a game runs: {why}");
            None
        });
        let playing = first_playing(&SIGNALS).or(heavy);
        watch.last_look = Some(Look {
            at: Instant::now(),
            playing: playing.clone(),
        });
        self.detected.send_if_modified(|last| {
            if *last == playing {
                return false;
            }
            last.clone_from(&playing);
            true
        });
        playing
    }

    /// Waits while a game runs, looking every LOOK_EVERY, for at most give_up_after. Err says
    /// so when it still ran then.
    pub async fn wait_until_over(&self, give_up_after: Duration) -> Result<(), String> {
        wait_while(|| self.why_running(), give_up_after, LOOK_EVERY).await
    }
}

/// The signals this computer gives, in the order they're asked.
#[cfg(windows)]
const SIGNALS: [Signal; 4] = [
    windows::full_screen,
    windows::known_game_in_front,
    windows::launcher_game_in_front,
    windows::steam_game,
];

#[cfg(target_os = "linux")]
const SIGNALS: [Signal; 2] = [linux::steam_game, linux::feral_game_mode];

#[cfg(not(any(windows, target_os = "linux")))]
const SIGNALS: [Signal; 0] = [];

/// The first signal's Playing, asking them in turn; failures are logged and skipped.
fn first_playing(signals: &[Signal]) -> Option<Playing> {
    signals.iter().find_map(|signal| match signal() {
        Ok(playing) => playing,
        Err(why) => {
            crate::log_line!("Couldn't tell whether a game runs: {why}");
            None
        }
    })
}

/// Waits while why_running says so, looking every look_every, for at most give_up_after.
async fn wait_while(
    why_running: impl Fn() -> Option<Playing>,
    give_up_after: Duration,
    look_every: Duration,
) -> Result<(), String> {
    let Some(playing) = why_running() else {
        return Ok(());
    };
    crate::say!("{}, so the job scan waits until game mode ends.", capitalized(&playing));
    let started = Instant::now();
    loop {
        if started.elapsed() >= give_up_after {
            return Err(format!(
                "A game or full-screen app ran for {} minutes, so the scan stopped; the posts left are matched by the next scan.",
                give_up_after.as_secs() / 60
            ));
        }
        tokio::time::sleep(look_every).await;
        if why_running().is_none() {
            crate::say!(
                "Game mode ended, so the job scan goes on ({:.0} minutes later).",
                started.elapsed().as_secs_f64() / 60.0
            );
            return Ok(());
        }
    }
}

/// playing's sentence with its first letter capitalized ("Steam runs a game (app 570)").
pub fn capitalized(playing: &Playing) -> String {
    let sentence = playing.to_string();
    let mut chars = sentence.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => sentence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn manual_mode_overrides_detection_until_disabled() {
        let mode = GameMode::default();
        mode.cache_detection_for_test(None);
        mode.set_manual(true);
        assert_eq!(mode.why_running(), Some(Playing::Manual));
        mode.cache_detection_for_test(Some(Playing::Steam { app_id: 570 }));
        mode.set_manual(false);
        assert_eq!(mode.why_running(), Some(Playing::Steam { app_id: 570 }));
    }

    #[tokio::test]
    async fn a_free_user_isnt_waited_for() {
        assert_eq!(wait_while(|| None, Duration::ZERO, Duration::ZERO).await, Ok(()));
    }

    #[tokio::test]
    async fn the_scan_goes_on_once_the_game_ends() {
        let looks = AtomicUsize::new(0);
        // Playing for the first two looks.
        let busy = || (looks.fetch_add(1, Ordering::Relaxed) < 2).then_some(Playing::FullScreen);
        let waited = wait_while(busy, Duration::from_secs(60), Duration::from_millis(1)).await;
        assert_eq!(waited, Ok(()));
        assert_eq!(looks.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn a_game_that_goes_on_stops_the_scan() {
        let why = wait_while(
            || Some(Playing::Steam { app_id: 570 }),
            Duration::from_millis(5),
            Duration::from_millis(1),
        )
        .await
        .unwrap_err();
        assert!(why.starts_with("A game or full-screen app ran for"), "{why}");
    }

    fn free() -> Result<Option<Playing>, String> {
        Ok(None)
    }

    fn broken() -> Result<Option<Playing>, String> {
        Err("no answer".to_string())
    }

    fn steam() -> Result<Option<Playing>, String> {
        Ok(Some(Playing::Steam { app_id: 570 }))
    }

    fn game_mode() -> Result<Option<Playing>, String> {
        Ok(Some(Playing::GameMode { games: 2 }))
    }

    #[test]
    fn the_first_signal_that_says_a_game_runs_is_why() {
        assert_eq!(
            first_playing(&[free, steam, game_mode]),
            Some(Playing::Steam { app_id: 570 })
        );
    }

    #[test]
    fn a_signal_that_cant_be_asked_says_nothing() {
        assert_eq!(
            first_playing(&[broken, game_mode]),
            Some(Playing::GameMode { games: 2 })
        );
        assert_eq!(first_playing(&[broken, free]), None);
    }

    #[test]
    fn each_reason_reads_as_a_sentence() {
        let program = "Cyberpunk2077.exe".to_string();
        assert_eq!(
            capitalized(&Playing::KnownGame { program }),
            "The game Cyberpunk2077.exe is in front"
        );
        let fortnite = Playing::LauncherGame {
            launcher: "Epic Games",
            game: "Fortnite".to_string(),
        };
        assert_eq!(fortnite.to_string(), "Fortnite (Epic Games) is in front");
        assert_eq!(Playing::GameMode { games: 1 }.to_string(), "gamemode runs a game");
        assert_eq!(Playing::GameMode { games: 3 }.to_string(), "gamemode runs 3 games");
    }
}
