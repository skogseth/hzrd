# hzrd
Provides shared mutability by utilizing hazard pointers for memory reclamation

## Overview
The core API centers around the `HzrdCell`, which provides a safe API for shared mutability. This is done using an API similar to that of `std::cell::Cell`: A user can read and write, but never get direct access to any references. In contrast to `std::cell::Cell` there is quite a lot more going on behind the scenes: `HzrdCell` uses hazard pointers to mark data as still-in-use, which keeps them from being freed. `HzrdCell` is most convenient if the type implements `Copy`, but is not exclusive to this.

There are multiple methods for indirect access to the value, which is convenient for non-copy types. The core method for referencing the value (indirectly) is through aqcuiring a `ReadHandle`, which dereferences to a shared borrow of the contained value. The shared reference is safe to access as it won't "change underneath you" as it only refers to the value of the cell when the read was called. The value is held alive by the cell's associated hazard pointer.

## Usage
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
