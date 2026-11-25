//! Robustness harness: reads NUL-separated sources from a file, checks each
//! as a one-file package, and reports any panic with its input.
use std::io::Read;
fn main() {
    let path = std::env::args().nth(1).unwrap();
    let mut data = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    std::panic::set_hook(Box::new(|_| {}));
    let mut panics = 0;
    let mut n = 0;
    for prog in data.split(|&b| b == 0) {
        n += 1;
        let src = String::from_utf8_lossy(prog).to_string();
        let s2 = src.clone();
        let r = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(move || pegasus_sema::check_source(&s2).diags.len())
            .unwrap()
            .join();
        if r.is_err() {
            panics += 1;
            std::fs::write(format!("/tmp/panic_{panics}.peg"), &src).unwrap();
        }
    }
    println!("{n} programs, {panics} panics");
}
