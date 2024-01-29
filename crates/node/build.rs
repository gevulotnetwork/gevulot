fn main() -> Result<(), Box<dyn std::error::Error>> {
    vergen::EmitBuilder::builder()
        .build_timestamp()
        .all_git()
        .emit()?;

    tonic_build::compile_protos("../shim/proto/vm_service.proto")?;
    Ok(())
}
