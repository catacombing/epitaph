//! Audio volume.

use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::thread;

use calloop::{LoopHandle, ping};
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::subscribe::InterestMaskSet;
use libpulse_binding::context::{Context, FlagSet as ContextFlagSet, State as PulseState};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::volume::Volume as PulseVolume;
use tracing::error;

use crate::config::{Color, Config};
use crate::module::{Module, PanelBackgroundModule};
use crate::{Result, State};

pub struct Volume {
    volume: Arc<AtomicU16>,
}

impl Volume {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        let volume = Arc::new(AtomicU16::new(0));

        // Setup calloop channel for redrawing on volume change.
        let (ping, source) = ping::make_ping()?;
        event_loop.insert_source(source, |_, _, state| state.unstall())?;

        // Listen for volume changes.
        let volume_setter = volume.clone();
        thread::spawn(move || {
            let mut pulse = match Pulseaudio::connect() {
                Ok(pulse) => pulse,
                Err(err) => {
                    error!("{err}");
                    return;
                },
            };

            pulse.on_volume_change(move |volume| {
                // Update the module's volume.
                let volume = (volume * 100.).round() as u16;
                volume_setter.store(volume, Ordering::Relaxed);

                // Notify event loop to force redraw.
                ping.ping();
            });

            if let Err(err) = pulse.run() {
                error!("{err}");
            }
        });

        Ok(Self { volume })
    }
}

impl Module for Volume {
    fn panel_background_module(&self) -> Option<&dyn PanelBackgroundModule> {
        Some(self)
    }
}

impl PanelBackgroundModule for Volume {
    fn value(&self) -> f64 {
        let volume = self.volume.load(Ordering::Relaxed);
        let modded = (volume % 100) as f64 / 100.;

        // Show 100% value for multiples of 100%, rather than 0%.
        if volume > 0 && modded == 0. { 100. } else { modded }
    }

    fn color(&self, config: &Config) -> Color {
        if self.volume.load(Ordering::Relaxed) > 100 {
            config.colors.volume_bad_bg
        } else {
            config.colors.volume_bg
        }
    }
}

struct Pulseaudio {
    mainloop: Mainloop,
    context: Context,
}

impl Pulseaudio {
    /// Connect to the pulseaudio server.
    fn connect() -> Result<Self> {
        // Connect with pulseaudio's standard event loop.
        let mut mainloop = Mainloop::new().ok_or("pulseaudio failed main loop creation")?;
        let mut context = Context::new(&mainloop, "EpitaphContext")
            .ok_or("pulseaudio failed context creation")?;
        context.connect(None, ContextFlagSet::NOFLAGS, None)?;

        // Wait for connection to be established.
        loop {
            match mainloop.iterate(true) {
                IterateResult::Quit(_) => {
                    return Err("pulseaudio quit before connection was established".into());
                },
                IterateResult::Err(err) => return Err(err.into()),
                IterateResult::Success(_) => (),
            }

            match context.get_state() {
                PulseState::Ready => break,
                state @ (PulseState::Failed | PulseState::Terminated) => {
                    return Err(
                        format!("pulseaudio {state:?} before connection was established").into()
                    );
                },
                _ => (),
            }
        }

        Ok(Self { mainloop, context })
    }

    /// Register a volume change listener.
    ///
    /// The new volume will be passed as a floating point value between 0 and 1.
    fn on_volume_change<F: FnMut(f64) + Clone + 'static>(&mut self, f: F) {
        let introspect = self.context.introspect();
        self.context.set_subscribe_callback(Some(Box::new(move |_, _, index| {
            let mut f = f.clone();
            introspect.get_sink_info_by_index(index, move |sink_info| {
                if let ListResult::Item(sink_info) = sink_info {
                    let volume = sink_info.volume.avg().0 as f64 / PulseVolume::NORMAL.0 as f64;
                    f(volume);
                }
            });
        })));
        self.context.subscribe(InterestMaskSet::SINK, |_subscribed| {});
    }

    /// Blockingly run main loop indefinitely.
    fn run(mut self) -> Result<()> {
        loop {
            match self.mainloop.iterate(true) {
                IterateResult::Quit(_) => break Err("pulseaudio connection shut down".into()),
                IterateResult::Err(err) => break Err(err.into()),
                IterateResult::Success(_) => (),
            }
        }
    }
}
