use std::thread;
use std::time::Duration;

use hzrd::HzrdCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Running,
    Finished,
}

fn main() {
    let state = HzrdCell::new(State::Idle);

    thread::scope(|s| {
        s.spawn(|| {
            thread::sleep(Duration::from_millis(1));
            match state.get() {
                State::Idle => println!("Waiting is boring, ugh"),
                State::Running => println!("Let's go!"),
                State::Finished => println!("We got here too late :/"),
            }
        });

        s.spawn(|| {
            state.set(State::Running);
            thread::sleep(Duration::from_millis(1));
            state.set(State::Finished);
        });
    });

    assert_eq!(state.get(), State::Finished);
}
