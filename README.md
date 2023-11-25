# hzrd
Provides shared, mutable state by utilizing hazard pointers.

The core concept of the crate is to trade memory for speed. The containers avoid locking the value, and instead accumulate garbage: Excess data that will need to be freed at a later point. The garbage collection is controlled using hazard pointers. Each reader of the value can hold one reference to the value. If the value of the container is swapped, then the reference they hold is kept valid through their hazard pointer. They can then (at some later point) drop the reference, and the value will be cleaned up _at some point_.

The crate currently provides two core interfaces:

- HzrdCell
- HzrdReader/HzrdWriter

## HzrdCell

HzrdCell aims to provide something akin to a multithreaded version of std's Cell-type. A basic example:

```rust
use hzrd::HzrdCell;

let mut cell_1 = HzrdCell::new(false);
let mut cell_2 = HzrdCell::clone(&cell_1);

std::thread::spawn(move || {
    // Loop until the value is true
    while !cell_1.get() {
        std::hint::spin_loop();
    }

    // And then set it back to false!
    cell_1.set(false);
});

std::thread::spawn(move || {
    // Set the value to true
    cell_2.set(true);

    // And then read the value!
    // This might print either `true` or `false`
    println!("{}", cell_2.get()); 
});
```

HzrdCell provides memory safe, multithreaded, shared mutability. But this isn't all that useful. We often want some sort of synchronization to avoid races (not data races, just general races).

## HzrdReader/HzrdWriter
One way to avoid race conditions is to allow multiple readers, but only one writer. This is what the HzrdReader/HzrdWriter pair provides. The readers hold a reference to the writer, no internal synchronization is used, so we need to use scoped threads in this case:

```rust
use hzrd::pair::{HzrdReader, HzrdWriter};

let writer = HzrdWriter::new(false);
let mut reader = HzrdReader::from_writer(&writer);

std::thread::scope(|s| {
    s.spawn(move || {
        // Loop until the value is true
        while !reader.get() {
            std::hint::spin_loop();
        }

        // This thread can only read!
        // reader.set(false); <- will not compile
    });

    // Set the value to true
    writer.set(true);

    // And then read the value!
    // This will always print `true`
    // TODO: The writer should be able to read the value directly
    println!("{}", writer.new_reader().get());
});
```

A key consequence of the strategies used in these containers is that mutation is a shared operation, whilst reading is an exclusive one. This has the fun (up for debate) consequence that readers must be marked as `mut`, whilst writers do not.
