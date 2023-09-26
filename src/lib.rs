/*!
This crate provides a safe API for shared mutability using hazard pointers for memory reclamation.

# [`HzrdCell`]
The simplest entrypoint to this crate is the [`HzrdCell`], which provides an API similar to that of the standard library's [`Cell`](std::cell::Cell)-type. However, [`HzrdCell`] allows shared mutation across multiple threads.

The main advantage of [`HzrdCell`], compared to something like a [`Mutex`](std::sync::Mutex), is that reading and writing to the value is lock-free. This is offset by an increased memory use, a significant overhead for creating/destroying cells, as well as some... funkiness. [`HzrdCell`] requires in contrast to the [`Mutex`](std::sync::Mutex) no additional wrapping, such as reference counting, in order to keep references valid for threads that may outlive eachother. There is an inherent reference count in the core functionality of [`HzrdCell`] which maintains this safety.

[`HzrdCell`] is particularly nice to work with if the underlying type implements copy. The [`get`](HzrdCell::get) method is lock-free, and requires minimal overhead. The [`set`](HzrdCell::set) method is mostly lock-free: The value is instantly updated, but there is some overhead following the swap which requires a lock to be acquired. However, this lock holds no contention with the reading methods of the cell.

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

let state = HzrdCell::new(State::Idle);

let state_1 = HzrdCell::clone(&state);
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

If you want to immutably borrow the underlying value then this is done by acquiring a [`RefHandle`]. At this point the [`HzrdCell`] shows off some of its "funkiness". Acquiring a [`RefHandle`] requires an exclusive (aka mutable) borrow of the cell, which in turn means the cell must be marked as mutable. Here is an example for a non-copy type, where a [`RefHandle`] is acquired and used.

```
# fn main() -> Result<(), &'static str> {
use hzrd::HzrdCell;

let string = String::from("Hello, world!");
let cell = HzrdCell::new(string);

// Notice the strangeness that aquiring the `RefHandle` requires mut
let mut cell_1 = HzrdCell::clone(&cell);
let handle_1 = std::thread::spawn(move || {
    let string = HzrdCell::read(&mut cell_1);
    if *string == "Hello, world!" {
        println!("I was first");
    } else {
        println!("The other thread was first");
    }
});

// ...whilst changing the value does not
let cell_2 = HzrdCell::clone(&cell);
let handle_2 = std::thread::spawn(move || {
    cell_2.set(String::new());
});
#
#     handle_1.join().map_err(|_| "Thread 1 failed")?;
#     handle_2.join().map_err(|_| "Thread 2 failed")?;
#
#     Ok(())
# }
```

There is no way to acquire a mutable borrow to the underlying value as that inherently requires locking the value.
*/

mod cell;
mod core;
mod utils;

pub mod pair;

pub use crate::cell::HzrdCell;
pub use crate::core::RefHandle;
