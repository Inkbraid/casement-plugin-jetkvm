//! casement-jetkvm: JetKVM support for Casement.
//!
//! Casement starts this program once per tab and talks to it over stdin and
//! stdout (see docs/plugin-protocol.md). Logs go to stderr.
//!
//! On Windows it is built for the GUI subsystem, so no console window
//! appears however it is started; the redirected standard handles work the
//! same.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    casement_jetkvm::stdio::init_logging();
    let out = casement_jetkvm::stdio::take_protocol_output();
    let host = casement_jetkvm::host::Host::new(out);
    let code = casement_jetkvm::run(host, std::io::stdin().lock());
    std::process::exit(code);
}
