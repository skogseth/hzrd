use hzrd::HzrdCell;

#[test]
fn single_threaded() {
    let mut cell_1 = HzrdCell::new(["hello", "world"]);
    let cell_2 = HzrdCell::clone(&cell_1);
    let handle = cell_1.read();
    println!("{}", handle[0]);
    let mut cell_3 = HzrdCell::clone(&cell_2);
    println!("{}", handle[1]);
    println!("{:?}", cell_3.cloned());
}
