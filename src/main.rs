//! A shep dog that leases one host's GPU and RAM to model servers and jobs, behind a single endpoint.

// Nothing outside the tests calls this until the book is built on it.
#[cfg_attr(not(test), expect(dead_code))]
mod footprint;
#[cfg(test)]
mod test_support;

fn main() {}
