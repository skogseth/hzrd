use std::sync::Arc;
use std::thread;

use hzrd::domains::SharedDomain;
use hzrd::HzrdCell;

fn main() {
    let domain = Arc::new(SharedDomain::new());

    let mut handles = Vec::new();
    for i in 0..4 {
        let domain = Arc::clone(&domain);

        let handle = thread::spawn(move || {
            let cell = Arc::new(HzrdCell::new_in(false, domain));

            let mut handles = Vec::new();
            for j in 0..3 {
                let cell = Arc::clone(&cell);

                let handle = thread::spawn(move || {
                    for _ in 0..1000 {
                        cell.set(!cell.get());
                    }
                    println!("[{i},{j}] {}", cell.get());
                });

                handles.push(handle);
            }

            for handle in handles {
                handle.join().unwrap();
            }

            println!("Thread {i} is done!");
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    println!("All done!");
}
