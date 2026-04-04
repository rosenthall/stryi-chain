use tonic_build::compile_protos;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/");
    println!("cargo::rustc-check-cfg=cfg(tokio_unstable)");

    compile_protos("proto/sync.proto")?;

    Ok(())
}
