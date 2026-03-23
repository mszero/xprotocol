fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_files = &["proto/xprotocol.proto"];
    tonic_build::configure()
        .out_dir("src/")
        .compile(proto_files, &["proto/"])?;
    Ok(())
}
