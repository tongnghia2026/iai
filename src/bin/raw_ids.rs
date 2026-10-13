//! Dev tool: print the clean camera identity (make, model) rawloader assigns to
//! each RAW file argument, tab-separated. Used to name per-camera profile files
//! for the default `camera_profiles/<make>__<model>.dcp` lookup. Not part of the
//! app; safe to remove.

fn main() {
    for arg in std::env::args().skip(1) {
        let path = std::path::PathBuf::from(&arg);
        match rawloader::decode_file(&path) {
            Ok(img) => println!("{}\t{}\t{}", img.clean_make, img.clean_model, arg),
            Err(error) => eprintln!("SKIP\t{arg}\t{error}"),
        }
    }
}
