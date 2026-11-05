fn main() {
    if let Err(err) = macos_kvm::session::cli() {
        eprintln!("macos-kvm: {err}");
        std::process::exit(1);
    }
}
