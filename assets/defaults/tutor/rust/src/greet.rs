/// A friendly greeting for `name`.
pub fn hello(name: &str) -> String {
    format!("Hello, {name}!")
}

/// The same greeting, louder.
pub fn shout(name: &str) -> String {
    hello(name).to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_greets_by_name() {
        assert_eq!(hello("Dust"), "Hello, Dust!");
    }
}
