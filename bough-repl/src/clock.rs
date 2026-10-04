//! The timers behind `tick`. A tick is an input like any other; what makes
//! it tick is I/O code on a thread of its own, which sends the count
//! through a [`RemoteIo`] for the driver's next pump.

use std::thread;
use std::time::Duration;

use bough::{Input, RemoteIo};

/// Starts the timer each `tick` asks for.
pub trait Clock {
    /// Starts a timer that sends 1, 2, 3, ... to `input` through `remote`,
    /// one every `period`, until the runtime is gone.
    fn start(&mut self, period: Duration, remote: RemoteIo, input: Input<i64>);
}

/// The wall clock: a thread per timer, sleeping `period` between sends.
pub struct WallClock;

impl Clock for WallClock {
    fn start(&mut self, period: Duration, remote: RemoteIo, input: Input<i64>) {
        thread::spawn(move || {
            for count in 1.. {
                thread::sleep(period);
                if remote.send(input, count).is_err() {
                    break;
                }
            }
        });
    }
}
