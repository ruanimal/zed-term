use std::{env, error::Error, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=resources/windows/app-icon.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let manifest_dir = PathBuf::from(
            env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR is not set")?,
        );
        let icon_path = manifest_dir.join("resources/windows/app-icon.ico");
        let escaped_icon_path = icon_path.to_string_lossy().replace('\\', "\\\\");
        let output_directory = PathBuf::from(env::var_os("OUT_DIR").ok_or("OUT_DIR is not set")?);
        let resource_path = output_directory.join("terminal_app.rc");

        fs::write(&resource_path, format!("1 ICON \"{escaped_icon_path}\"\n"))?;
        embed_resource::compile(&resource_path, embed_resource::NONE).manifest_required()?;
    }

    Ok(())
}
