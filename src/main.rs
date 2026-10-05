//! A shep dog that leases one host's GPU and RAM to model servers and jobs, behind a single endpoint.

// The bin does not call these items yet. `allow`, not `expect`: 1.88 counts
// them used through `config`'s own dead code and stable does not.
#[cfg_attr(not(test), allow(dead_code))]
mod footprint;
// The bin does not call these items yet, only the section for the probe.
#[cfg_attr(not(test), expect(dead_code))]
mod config;
// The bin runs no engine to drive the book, so only its tests reach it.
#[cfg_attr(not(test), allow(dead_code))]
mod book;
#[cfg(test)]
mod test_support;

fn main() {
    shep_client::dogs::probe::<config::section::Section>(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
    );
}
