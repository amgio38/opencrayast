//! Golden source for the Rust parse shape (skeleton + symbols).
//! Line numbers in parse_golden_spec.rs refer to this file.

/// A configuration.
pub struct Config {
    pub name: String,
}

pub enum Error {
    Io,
    Parse,
}

pub trait Shape {
    fn area(&self) -> f64;
}

impl Config {
    pub fn load(path: &str) -> Result<Config, Error> {
        todo!()
    }
}

const MAX: usize = 10;

pub fn main() {}
