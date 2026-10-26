//! The `ember-sign-in` helper; see the library for what it does.

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(ember_sign_in::main(std::env::args().skip(1)))
}
