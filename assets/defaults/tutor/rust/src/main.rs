mod greet;

use greet::{hello, shout};

fn main() {
    let name = "Dust";
    let message = hello(name);
    println!("{}", message);
    let unused = 42;
    for _ in 0..3 {
        println!("{}", shout(name));
    }
}
