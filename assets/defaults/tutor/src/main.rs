mod greet;

fn main() {
    let name = "Dust";
    let message = greet::hello(name);
    println!("{message}");
    let unused = 42;
    for _ in 0..3 {
        println!("{}", greet::shout(name));
    }
}
