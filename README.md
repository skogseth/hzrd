# hzrd
Provides shared, mutable state by utilizing hazard pointers.

The core concept of the crate is to trade memory for speed. The containers avoid locking the value, and instead accumulate garbage: Excess data that will need to be freed at a later point. The garbage collection is controlled using hazard pointers. Each reader of the value can hold one reference to the value. If the value of the container is swapped, then the reference they hold is kept valid through their hazard pointer. They can then (at some later point) drop the reference, and the value will be cleaned up _at some point_. The core API in the crate is the HzrdCell.

## HzrdCell

HzrdCell aims to provide something akin to a multithreaded version of std's Cell-type. A basic example:

```rust
use hzrd::HzrdCell;

let cell = HzrdCell::new(false);

std::thread::scope(|s| {
    s.spawn(|| {
        // Loop until the value is true
        while !cell.get() {
            std::hint::spin_loop();
        }

        // And then set it back to false!
        cell.set(false);
    });

    s.spawn(|| {
        // Set the value to true
        cell.set(true);

        // And then read the value!
        // This might print either `true` or `false`
        println!("{}", cell.get()); 
    });
});
```

HzrdCell provides memory safe, multithreaded, shared mutability. But this isn't all that useful. We often want some sort of synchronization to avoid races (not data races, just general races).
