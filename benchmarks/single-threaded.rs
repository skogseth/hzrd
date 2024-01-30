use hzrd::HzrdCell;

fn main() {
    let cell = HzrdCell::new(0);

    for i in 0..1_000_000 {
        let _handle = cell.get();
        cell.set(i)
    }
}
