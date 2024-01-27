use hzrd::HzrdCell;

const N: usize = 1000;

fn main() {
    let cell = HzrdCell::new(None);

    std::thread::scope(|s| {
        s.spawn(|| {
            for i in 0..N {
                while cell.read().is_some() {
                    std::hint::spin_loop();
                }
                cell.set(Some(i));
            }
        });

        s.spawn(|| {
            for _ in 0..N {
                while cell.read().is_none() {
                    std::hint::spin_loop();
                }
                cell.set(None);
            }
        });
    });
}
