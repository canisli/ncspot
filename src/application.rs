use std::error::Error;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use cursive::traits::Nameable;
use cursive::{Cursive, CursiveRunner};
use log::{error, info, trace};

#[cfg(unix)]
use signal_hook::{consts::SIGHUP, consts::SIGTERM, iterator::Signals};

use crate::command::Command;
use crate::commands::CommandManager;
use crate::config::{Config, PlaybackState};
use crate::events::{Event, EventManager};
use crate::library::Library;
use crate::queue::Queue;
use crate::spotify::{PlayerEvent, Spotify};
use crate::ui::create_cursive;
use crate::theme;
use crate::{authentication, ui, utils};
use crate::{command, queue, spotify};

#[cfg(feature = "mpris")]
use crate::mpris::MprisManager;

#[cfg(unix)]
use crate::ipc::{self, IpcSocket};

/// Set up the global logger to log to `filename`.
pub fn setup_logging(filename: &Path) -> Result<(), fern::InitError> {
    fern::Dispatch::new()
        // Perform allocation-free log formatting
        .format(|out, message, record| {
            out.finish(format_args!(
                "{} [{}] [{}] {}",
                chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S]"),
                record.target(),
                record.level(),
                message
            ))
        })
        // Add blanket level filter -
        .level(log::LevelFilter::Debug)
        // Set runtime log level for modules
        .level_for("ncspot", log::LevelFilter::Trace)
        // Output to stdout, files, and other Dispatch configurations
        .chain(fern::log_file(filename)?)
        // Apply globally
        .apply()?;
    Ok(())
}

pub type UserData = Rc<UserDataInner>;
pub struct UserDataInner {
    pub cmd: CommandManager,
}

/// The global Tokio runtime for running asynchronous tasks.
pub static ASYNC_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// The representation of an ncspot application.
pub struct Application {
    /// The music queue which controls playback order.
    queue: Arc<Queue>,
    /// Internally shared
    spotify: Spotify,
    /// Internally shared
    event_manager: EventManager,
    /// Configuration
    cfg: Arc<Config>,
    /// An IPC implementation using the D-Bus MPRIS protocol, used to control and inspect ncspot.
    #[cfg(unix)]
    ipc: Option<IpcSocket>,
    /// The object to render to the terminal.
    cursive: CursiveRunner<Cursive>,
}

impl Application {
    /// Create a new ncspot application.
    ///
    /// # Arguments
    ///
    /// * `configuration_file_path` - Relative path to the configuration file inside the base path
    pub fn new(configuration_file_path: Option<String>) -> Result<Self, Box<dyn Error>> {
        // Things here may cause the process to abort; we must do them before creating curses
        // windows otherwise the error message will not be seen by a user

        ASYNC_RUNTIME
            .set(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            )
            .unwrap();

        let configuration = Arc::new(Config::new(configuration_file_path));
        let credentials = authentication::get_credentials(&configuration)?;
        let theme = configuration.build_theme();

        println!("Connecting to Spotify..");

        // DON'T USE STDOUT AFTER THIS CALL!
        let mut cursive = create_cursive().map_err(|error| error.to_string())?;

        cursive.set_theme(theme.clone());
        #[cfg(target_os = "macos")]
        {
            use tokio::time::Duration;

            let cb_sink = cursive.cb_sink().clone();
            let theme_cfg = configuration.values().theme.clone();
            // Periodically check system appearance and update theme if it changes.
            ASYNC_RUNTIME.get().unwrap().spawn(async move {
                let mut last = theme::detect_appearance();
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    let current = theme::detect_appearance();
                    if current != last {
                        last = current;
                        let theme_cfg = theme_cfg.clone();
                        let _ = cb_sink.send(Box::new(move |s| {
                            s.set_theme(theme::load(&theme_cfg));
                        }));
                    }
                }
            });
        }

        #[cfg(all(unix, feature = "pancurses_backend"))]
        cursive.add_global_callback(cursive::event::Event::CtrlChar('z'), |_s| unsafe {
            libc::raise(libc::SIGTSTP);
        });

        let event_manager = EventManager::new(cursive.cb_sink().clone());

        let spotify =
            spotify::Spotify::new(event_manager.clone(), credentials, configuration.clone())?;

        let library = Arc::new(Library::new(
            event_manager.clone(),
            spotify.clone(),
            configuration.clone(),
        ));

        let queue = Arc::new(queue::Queue::new(
            spotify.clone(),
            configuration.clone(),
            library.clone(),
        ));

        #[cfg(feature = "mpris")]
        let mpris_manager = MprisManager::new(
            event_manager.clone(),
            queue.clone(),
            library.clone(),
            spotify.clone(),
        );

        #[cfg(feature = "mpris")]
        spotify.set_mpris(mpris_manager.clone());

        // Load the last played track into the player
        let playback_state = configuration.state().playback_state.clone();
        let queue_state = configuration.state().queuestate.clone();

        if let Some(playable) = queue.get_current() {
            spotify.load(
                &playable,
                playback_state == PlaybackState::Playing,
                queue_state.track_progress.as_millis() as u32,
            );
            spotify.update_track();
            match playback_state {
                PlaybackState::Stopped => {
                    spotify.stop();
                }
                PlaybackState::Paused | PlaybackState::Playing | PlaybackState::Default => {
                    spotify.pause();
                }
            }
        }

        #[cfg(unix)]
        let ipc = if let Ok(runtime_directory) = utils::create_runtime_directory() {
            Some(
                ipc::IpcSocket::new(
                    ASYNC_RUNTIME.get().unwrap().handle(),
                    runtime_directory.join("ncspot.sock"),
                    event_manager.clone(),
                )
                .map_err(|e| e.to_string())?,
            )
        } else {
            error!("failed to create IPC socket: no suitable user runtime directory found");
            None
        };

        let mut cmd_manager = CommandManager::new(
            spotify.clone(),
            queue.clone(),
            library.clone(),
            configuration.clone(),
            event_manager.clone(),
        );

        cmd_manager.register_all();
        cmd_manager.register_keybindings(&mut cursive);

        cursive.set_user_data(Rc::new(UserDataInner { cmd: cmd_manager }));

        // Start macOS audio device monitoring if on macOS
        // Do this asynchronously to avoid blocking startup if CoreAudio has issues
        #[cfg(target_os = "macos")]
        {
            use crate::macos_audio;
            use tokio::sync::mpsc as tokio_mpsc;
            let (device_tx, mut device_rx) = tokio_mpsc::unbounded_channel();
            let event_manager_clone = event_manager.clone();

            // Spawn task to handle device change notifications first
            ASYNC_RUNTIME.get().unwrap().spawn(async move {
                while let Some(device_name) = device_rx.recv().await {
                    info!("Audio device changed, sending event");
                    event_manager_clone.send(Event::AudioDeviceChanged(device_name));
                }
            });

            // Start the monitor in a separate task to avoid blocking
            let device_tx_clone = device_tx.clone();
            ASYNC_RUNTIME.get().unwrap().spawn(async move {
                // Small delay to ensure runtime is fully initialized
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                
                match macos_audio::start_device_monitor(device_tx_clone) {
                    Ok(()) => {
                        info!("Started macOS audio device monitor");
                    }
                    Err(e) => {
                        error!("Failed to start audio device monitor: {e}");
                    }
                }
            });
        }

        let search =
            ui::search::SearchView::new(event_manager.clone(), queue.clone(), library.clone());

        let libraryview = ui::library::LibraryView::new(queue.clone(), library.clone());

        let queueview = ui::queue::QueueView::new(queue.clone(), library.clone());

        #[cfg(feature = "cover")]
        let coverview = ui::cover::CoverView::new(queue.clone(), library.clone(), &configuration);

        let status = ui::statusbar::StatusBar::new(queue.clone(), Arc::clone(&library));

        let mut layout =
            ui::layout::Layout::new(status, &event_manager, theme, Arc::clone(&configuration))
                .screen("search", search.with_name("search"))
                .screen("library", libraryview.with_name("library"))
                .screen("queue", queueview);

        #[cfg(feature = "cover")]
        layout.add_screen("cover", coverview.with_name("cover"));

        // initial screen is library
        let initial_screen = configuration
            .values()
            .initial_screen
            .clone()
            .unwrap_or_else(|| "library".to_string());
        if layout.has_screen(&initial_screen) {
            layout.set_screen(initial_screen);
        } else {
            error!("Invalid screen name: {initial_screen}");
            layout.set_screen("library");
        }

        cursive.add_fullscreen_layer(layout.with_name("main"));

        Ok(Self {
            queue,
            spotify,
            event_manager,
            cfg: configuration,
            #[cfg(unix)]
            ipc,
            cursive,
        })
    }

    /// Start the application and run the event loop.
    pub fn run(&mut self) -> Result<(), String> {
        #[cfg(unix)]
        let mut signals =
            Signals::new([SIGTERM, SIGHUP]).expect("could not register signal handler");

        // cursive event loop
        while self.cursive.is_running() {
            self.cursive.step();
            #[cfg(unix)]
            for signal in signals.pending() {
                if signal == SIGTERM || signal == SIGHUP {
                    info!("Caught {signal}, cleaning up and closing");
                    if let Some(data) = self.cursive.user_data::<UserData>().cloned() {
                        data.cmd.handle(&mut self.cursive, Command::Quit);
                    }
                }
            }
            for event in self.event_manager.msg_iter() {
                match event {
                    Event::Player(state) => {
                        trace!("event received: {state:?}");
                        self.spotify.update_status(state.clone());

                        #[cfg(unix)]
                        if let Some(ref ipc) = self.ipc {
                            ipc.publish(&state, self.queue.get_current());
                        }

                        if state == PlayerEvent::FinishedTrack {
                            self.queue.next(false);
                        }
                    }
                    Event::Queue(event) => {
                        self.queue.handle_event(event);
                    }
                    Event::SessionDied => {
                        if self.spotify.start_worker(None).is_err() {
                            let data: UserData = self
                                .cursive
                                .user_data()
                                .cloned()
                                .expect("user data should be set");
                            data.cmd.handle(&mut self.cursive, Command::Quit);
                        };
                    }
                    Event::IpcInput(input) => match command::parse(&input) {
                        Ok(commands) => {
                            if let Some(data) = self.cursive.user_data::<UserData>().cloned() {
                                for cmd in commands {
                                    info!("Executing command from IPC: {cmd}");
                                    data.cmd.handle(&mut self.cursive, cmd);
                                }
                            }
                        }
                        Err(e) => error!("Parsing error: {e}"),
                    },
                    #[cfg(target_os = "macos")]
                    Event::AudioDeviceChanged(device_name) => {
                        info!("Handling audio device change to: {}", if device_name.is_empty() { "default" } else { &device_name });
                        
                        // Save current track info before shutting down
                        let status = self.spotify.get_current_status();
                        let was_playing = matches!(status, PlayerEvent::Playing(_));
                        let current_track = self.queue.get_current().clone();
                        // Get current position from status or progress
                        let current_position = match status {
                            PlayerEvent::Playing(_) | PlayerEvent::Paused(_) => {
                                Some(self.spotify.get_current_progress().as_millis() as u32)
                            }
                            _ => None,
                        };
                        
                        // Pause playback first
                        if was_playing {
                            info!("Pausing playback due to device change");
                            self.spotify.pause();
                            // Give pause command time to process
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        
                        // Update config with new device (or None if empty/default)
                        let device = if device_name.is_empty() {
                            None
                        } else {
                            Some(device_name)
                        };
                        
                        // Update the config BEFORE starting new worker
                        self.cfg.set_backend_device(device.clone());
                        info!("Updated backend_device config to: {:?}", device);
                        
                        // Start a new worker with the new device
                        info!("Starting new worker with audio device: {}", device.as_ref().map(|s| s.as_str()).unwrap_or("default"));
                        if self.spotify.start_worker(None).is_err() {
                            error!("Failed to start new worker after device change");
                            continue;
                        }
                        
                        // Reload the current track if there was one playing
                        if let Some(track) = current_track {
                            info!("Reloading track after device change");
                            if let Some(pos) = current_position {
                                self.spotify.load(&track, false, pos); // Load paused
                            } else {
                                self.spotify.load(&track, false, 0);
                            }
                        }
                    },
                }
            }
        }
        Ok(())
    }
}
