use std::thread;
use std::time::Duration;

use hzrd::pair::{HzrdReader, HzrdWriter};

const TIMEOUT: Duration = Duration::from_millis(1);
const UPDATE_TIME: Duration = Duration::from_millis(5);

pub fn main() {
    let progress = HzrdWriter::new(0);

    thread::scope(|s| {
        let mut progress_r = HzrdReader::from_writer(&progress);
        s.spawn(move || loop {
            let percentage = progress_r.get();
            if percentage == 100 {
                println!("We're done!");
                break;
            } else {
                println!("Progress: {percentage}%");
                thread::sleep(UPDATE_TIME);
            }
        });

        for _ in 0..100 {
            thread::sleep(TIMEOUT);
            let current = progress.new_reader().get();
            progress.set(current + 1);
        }
    });
}
