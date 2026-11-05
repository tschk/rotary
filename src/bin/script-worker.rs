fn main() {
    if rx4::tools::script_worker_main().is_err() {
        std::process::exit(1);
    }
}
