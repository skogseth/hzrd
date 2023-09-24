# hzrd
Provides shared mutability by utilizing hazard pointers for memory reclamation

## The cell type
The simplest API in this crate can be found in the `HzrdCell`, which provides a safe API for shared mutability. This is done using an API similar to that of `std::cell::Cell`: A user can read and write, but never get direct access to any references. In contrast to `std::cell::Cell` there is quite a lot more going on behind the scenes: `HzrdCell` uses hazard pointers to mark data as still-in-use, which keeps them from being freed. `HzrdCell` is most convenient if the type implements `Copy`, but is not exclusive to this.

There are multiple methods for indirect access to the value, which is convenient for non-copy types. The core method for referencing the value (indirectly) is through aqcuiring a `RefHandle`, which dereferences to a shared borrow of the contained value. The shared reference is safe to access as it won't "change underneath you" as it only refers to the value of the cell when the read was called. The value is held alive by the cell's associated hazard pointer.

Shared mutability is, in fact, not particularly useful. Although `HzrdCell` provides shared mutability in a safe API, meaning no use-after-frees and no data-races, it does not fundamentally do anything about race conditions. The classic "ABA-problem" still remains:
- Thread A reads the value, does something with that info, but is halted
- Thread B reads the value, does something with that info, and then writes data to the value
- Thread A then writes to the value completely clueless about the changes thread B made

This means that shared mutability is only really practical if the writing happens independent of the reading, meaning any updates to the value are independent of the current value. As one can imagine this is not what you want in a lot of cases. `HzrdCell` is therefore recommended mostly for cases when you want a shared state. An example:

```rust
use hzrd::HzrdCell;

#[derive(Clone, Copy)]
enum Weather {
    Sunny,
    Cloudy,
    Rainy(u16), // [mm/h]
    Snowy(u16), // [mm/h] 
}

fn current_weather() -> Weather { todo!() }
fn do_something(weather: Weather) { todo!() }
fn should_do_something() -> bool { todo!() }
fn check_for_shutdown_signal() -> bool { todo!() }

fn main() {
    let weather = HzrdCell::new(current_weather());

    let mut handles = Vec::new();
    for _ in 0..8 {
        let weather = HzrdCell::clone(&weather);
        let handle = std::thread::spawn(move || {
            while check_for_shutdown_signal() {
                if should_do_something() {
                    // This is gonna take some time
                    do_something(weather.get());
                } else {
                    // We got some time to check the weather
                    weather.set(current_weather());
                }
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}
```

The other case is when there is only one writer, which is (to be honest) the more useful way of using this. But we can optimize specifically for that case...

## Single writer, multiple readers
The reader/writer pair is the simplest, purest of the container types in this library. They use no internal synchronization in order to work, besides the hazard-pointers. The readers keep an internal reference to the writer in order to guarantee that cleanup occurs correctly. This means they hold a stack reference, and are locked to multithreading where the `'static` bound is not required, such as scoped threads:

```
use hzrd::pair::{HzrdWriter, HzrdReader};

enum State {
    Booting,
    Running {
        message: String,
    }
    Shutdown,
}

let state_writer = HzrdWrtier::new(State::Booting);

std::thread::scope(|s| {
    // The reader will hold an internal reference to the writer
    let state_reader: HzrdReader<'_, State> = HzrdReader::from_writer(&state_writer);
    s.spawn(move || {
        loop {
            match state_reader().get() {
                State::Booting => println!("Booting up..."),
                State::Running { message } => println!("Message for you, sir: \"{message}\"."),
                State::Shutdown => {
                    println!("You are the weakest link, goodbye.");
                    break;
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });

    // Booting up is such a hard job
    std::thread::sleep(Duration::from_secs(3));

    state_writer.set(State::Running {
        message: String::from("Work work work"),
    });

    // Wait some more
    std::thread::sleep(Duration::from_secs(1));

    state_writer.set(State::Shutdown);
});
```

This program will print something along the lines of
```
Booting up...
Booting up...
Booting up...
Message for you, sir: "Work work work".
You are the weakest link, goodbye.
```
