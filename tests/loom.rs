#![cfg(loom)]

use loom::sync::Arc;

use hzrd::{HzrdCell, SharedDomain};

#[test]
fn my_first_loom_test() {
    loom::model(|| {
        let domain = Arc::new(SharedDomain::new());

        let cell = Arc::new(HzrdCell::new_in(0, Arc::clone(&domain)));

        assert_eq!(cell.get(), 0);

        let handle_1 = loom::thread::spawn({
            let cell = Arc::clone(&cell);
            move || {
                cell.set(1);
            }
        });

        let handle_2 = loom::thread::spawn({
            let cell = Arc::clone(&cell);
            move || {
                cell.set(2);
            }
        });

        let val = cell.get();
        assert!(matches!(val, 0 | 1 | 2), "Value was {val}");

        handle_1.join().unwrap();
        handle_2.join().unwrap();

        cell.set(3);
        assert_eq!(cell.get(), 3);
    });
}
