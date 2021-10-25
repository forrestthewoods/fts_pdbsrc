use std::env;
use std::path::Path;
use std::path::PathBuf;

fn main() {
    let filename = "fts_pdbsrc_config.json";
    let relpath = format!("data/{}", filename);
    println!("cargo:rerun-if-changed={}", relpath);

    // Compute paths
    let output_path = get_output_path();
    let input_path = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join(relpath);
    let output_path = Path::new(&output_path).join(filename);

    // Copy file
    match std::fs::copy(&input_path, &output_path) {
        Ok(_) => (),
        Err(e) => {
            println!(
                "cargo:warning=Failed to copy from [{:?}] to [{:?}]. Err: [{:?}]",
                input_path, output_path, e
            );
            std::process::exit(1);
        }
    }
}

// Compute path to: ../fts_pdbsrc/target/<profile>/
fn get_output_path() -> PathBuf {
    let manifest_dir_string = env::var("CARGO_MANIFEST_DIR").unwrap();
    let build_type = env::var("PROFILE").unwrap();
    let path = Path::new(&manifest_dir_string).join("target").join(build_type);
    return PathBuf::from(path);
}
