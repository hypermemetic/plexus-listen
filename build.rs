fn main() {
    // The `screencapturekit` crate compiles Swift code that links against
    // @rpath/libswift_Concurrency.dylib. Without an rpath entry pointing
    // to the Swift runtime, dyld will SIGABRT at launch.
    //
    // On macOS the Swift runtime ships at /usr/lib/swift.
    #[cfg(all(target_os = "macos", feature = "app-capture"))]
    {
        println!("cargo:rustc-link-arg=-Xlinker");
        println!("cargo:rustc-link-arg=-rpath");
        println!("cargo:rustc-link-arg=-Xlinker");
        println!("cargo:rustc-link-arg=/usr/lib/swift");
    }
}
