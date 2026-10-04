// The desktop shell.
//
// Entry point only. Everything real lives in the library beside this file, so
// that it can be linked by tests and by the mobile entry points without a
// `main`.
fn main() {
    ifami_desktop::run();
}
