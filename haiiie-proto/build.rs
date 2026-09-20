//! Generate the protobuf types at build time.
//!
//! Generated rather than checked in: a checked-in copy is a second source of
//! truth that drifts from the `.proto` silently, and this project has spent
//! enough of its time on claims that stopped matching the thing they described.
//! The cost is a `protoc` on the build host, which `tonic-build` vendors.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/haiiie.v1.proto");
    // Use the vendored compiler unless the host names one explicitly, so a
    // checkout builds with no system package installed.
    if std::env::var_os("PROTOC").is_none() {
        unsafe { std::env::set_var("PROTOC", protoc_bin_vendored::protoc_bin_path()?) };
    }
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_protos(&["proto/haiiie.v1.proto"], &["proto"])?;
    Ok(())
}
