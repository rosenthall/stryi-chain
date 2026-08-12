fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/");
    println!("cargo::rustc-check-cfg=cfg(tokio_unstable)");

    tonic_prost_build::compile_protos("proto/sync.proto")?;

    Ok(())
}
