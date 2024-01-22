use hzrd::core::SharedDomain;
use hzrd::HzrdCell;

#[test]
fn simple_test() {
    let cell = HzrdCell::new_in(String::from("hello"), SharedDomain::new());

    std::thread::scope(|s| {
        s.spawn(|| {
            while *cell.read() != "32" {
                std::hint::spin_loop();
            }
            cell.set(String::from("world"));
        });

        for string in (0..40).map(|i| i.to_string()) {
            s.spawn(|| cell.just_set(string));
        }
    });
}
