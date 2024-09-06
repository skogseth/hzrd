# hzrd
Provides shared, mutable state by utilizing hazard pointers.

## HzrdCell

The core API of this crate is the HzrdCell, which provides an API reminiscent to that of the standard library's Cell-type. However, HzrdCell allows shared mutation across multiple threads.

```rust
use hzrd::HzrdCell;

let cell = HzrdCell::new(false);

std::thread::scope(|s| {
    s.spawn(|| {
        // Loop until the value is true ...
        while !cell.get() {
            std::hint::spin_loop();
        }

        // ... and then set it back to false!
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
