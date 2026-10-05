# The paddock owns what lives on its host, nothing else

shep-kelpie leases three things: the `gpu` lock, CodeRabbit's review window, and a share of the Mac's CPU for `cargo-test`. Only the first is on the paddock's host, so only it moves. kelpie keeps the other two and becomes a paddock client for the GPU. Making the paddock the authority for every shared resource would stop the Mac's review pacing whenever the GPU host is down, and would put an account-wide quota inside a dog that exists for one machine's GPU and RAM.
