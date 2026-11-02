/// A friendly greeting for `name`.
pub fn hello(name: &str) -> String {
    format!("Hello, {name}!")
}

/// The same greeting, louder.
pub fn shout(name: &str) -> String {
    hello(name).to_uppercase()
}
