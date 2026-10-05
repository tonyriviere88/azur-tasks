//! `cargo deploy` — build the release binary and put it where you run it from.
//!
//! ```text
//! cargo deploy                     release build -> D:\Programs
//! cargo deploy E:\Tools            somewhere else
//! cargo deploy --debug             the debug build, for once
//! cargo deploy --profile fast      the iteration profile, without the two-minute link
//! ```
//!
//! The work is [`azur_egui_theme::deploy`], which every Azur application shares — including
//! why any of this is an example target rather than a shell script. What has to be here is
//! the two things only this package knows, and `env!` can only answer them where it is
//! written: asked inside the theme crate it would deploy the theme crate.
//!
//! `bin` is what makes this package's second binary stay out of it: `memprobe` is a
//! measurement tool behind a feature, and a deploy asks for the application by name rather
//! than for whatever the profile directory happens to hold.

fn main() -> std::process::ExitCode {
    azur_egui_theme::deploy::main(azur_egui_theme::deploy::Program {
        bin: env!("CARGO_PKG_NAME"),
        manifest: env!("CARGO_MANIFEST_DIR"),
    })
}
