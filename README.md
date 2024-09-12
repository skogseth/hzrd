# hzrd

[![Crate](https://img.shields.io/crates/v/hzrd.svg)](https://crates.io/crates/hzrd)
[![API](https://docs.rs/hzrd/badge.svg)](https://docs.rs/hzrd)

Provides shared, mutable state by utilizing hazard pointers.

## Usage

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

## License
This project is licensed under the [MIT license].

[MIT license]: https://github.com/skogseth/hzrd/blob/main/LICENSE
