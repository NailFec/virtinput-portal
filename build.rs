fn main() {
    pkg_config::probe_library("libeis-1.0").expect("libeis-1.0 is required");
}
