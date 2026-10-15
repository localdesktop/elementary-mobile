fn main() {
    // Prebuilt Android libraries. Leaving them out of host builds lets `cargo test` run on a PC,
    // where the host linker would otherwise pick up the Android libiconv.so.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
        let lib_path = "./assets/libs/arm64-v8a";
        println!("cargo::rustc-link-search={}", lib_path);
    }
}
