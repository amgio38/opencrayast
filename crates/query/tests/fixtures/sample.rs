//! Sample module.

/// A configuration.
pub struct Config {
    pub name: String,
}

impl Config {
    /// Load it.
    pub fn load(path: &str) -> Result<Config, Error> {
        todo!()
    }

    fn validate(&self) -> bool {
        true
    }
}

pub enum Error {
    Io,
    Parse,
}

pub trait Shape {
    fn area(&self) -> f64;
}

const MAX: usize = 10;

mod inner {
    pub fn helper() {}
}

pub fn main() {}
