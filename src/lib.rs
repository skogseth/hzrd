/*!
This crate provides a safe API for shared mutability using hazard pointers for memory reclamation.

# HzrdCell
The simplest entrypoint to this crate is the [`HzrdCell`], which provides an API reminiscent to that of the standard library's [`Cell`](std::cell::Cell)-type. However, [`HzrdCell`] allows shared mutation across multiple threads.

The main advantage of [`HzrdCell`], compared to something like a [`Mutex`](std::sync::Mutex), is that reading and writing to the value is lock-free. This is offset by an increased memory use, a significant overhead for creating/destroying cells, as well as some... funkiness. [`HzrdCell`] requires in contrast to the [`Mutex`](std::sync::Mutex) no additional wrapping, such as reference counting, in order to keep references valid for threads that may outlive eachother: There is an inherent reference count in the core functionality of [`HzrdCell`] which maintains this. A consequence of this is that [`HzrdCell`]s should not form cycles.

Reading the value of the cell, e.g. via the [`get`](HzrdCell::get) method, is lock-free. Writing to the value is instant, but there is some overhead following the swap which requires locking the metadata of the cell (to allow some synchronization of garbage collection between the cells). The main way of writing to the value is via the [`set`](HzrdCell::set) method. Here is an example of [`HzrdCell`] in use.

```
# fn main() -> Result<(), &'static str> {
use std::time::Duration;
use std::thread;

use hzrd::HzrdCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Running,
    Finished,
}

let mut state = HzrdCell::new(State::Idle);

let mut state_1 = HzrdCell::clone(&state);
let handle_1 = thread::spawn(move || {
    thread::sleep(Duration::from_millis(1));
    match state_1.get() {
        State::Idle => println!("Waiting is boring, ugh"),
        State::Running => println!("Let's go!"),
        State::Finished => println!("We got here too late :/"),
    }
});

let state_2 = HzrdCell::clone(&state);
let handle_2 = thread::spawn(move || {
    state_2.set(State::Running);
    thread::sleep(Duration::from_millis(1));
    state_2.set(State::Finished);
});

handle_1.join().map_err(|_| "Thread 1 failed")?;
handle_2.join().map_err(|_| "Thread 2 failed")?;

assert_eq!(state.get(), State::Finished);
#
#     Ok(())
# }
```

# The elephant in the room
The keen eye might have observed some of the "funkiness" of [`HzrdCell`] in the previous example: Reading the value of the cell required it to be mutable, whilst writing to the cell did not. Exclusivity is usually associated with mutation, but for the [`HzrdCell`] this relationship is inversed in order to bend the rules of mutation. Another example, here using the [`read`](HzrdCell::read) function to acquire a [`RefHandle`] to the underlying value, as it doesn't implement copy:

```
# use hzrd::HzrdCell;
#
// NOTE: The cell must be marked as mutable to allow reading the value
let mut cell = HzrdCell::new([0, 1, 2]);

// NOTE: Associated function syntax used to clarify mutation requirement
let handle = HzrdCell::read(&mut cell);
assert_eq!(handle[0], 0);
```
*/

mod cell;
mod core;
mod stack;
mod utils;

pub mod pair;

pub use crate::cell::HzrdCell;
pub use crate::core::RefHandle;

mod private {
    // We want to test the code in the readme
    #![doc = include_str!("../README.md")]
}
