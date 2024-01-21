fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("../shim/proto/vm_service.proto")?;
    Ok(())
}
