use iai::core::develop::DevelopSettings;
use iai::core::develop_scene::{eval_scene_pixel, BaseLook};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

const VECTORS: &[(&str, [f32; 3])] = &[
    ("black", [0.0, 0.0, 0.0]),
    ("grey_18", [0.18, 0.18, 0.18]),
    ("grey_50", [0.5, 0.5, 0.5]),
    ("white", [1.0, 1.0, 1.0]),
    ("red", [0.64, 0.06, 0.03]),
    ("orange", [0.75, 0.25, 0.03]),
    ("yellow", [0.75, 0.65, 0.03]),
    ("green", [0.05, 0.55, 0.08]),
    ("cyan", [0.02, 0.55, 0.7]),
    ("blue", [0.03, 0.08, 0.8]),
    ("magenta", [0.65, 0.04, 0.55]),
    ("negative_red", [-0.05, 0.2, 0.3]),
    ("hdr_blue", [0.4, 1.2, 3.0]),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("color-baseline.csv"));
    let file = File::create(&output)?;
    let mut writer = BufWriter::new(file);
    writeln!(
        writer,
        "algorithm,look,control,value,vector,in_r,in_g,in_b,out_r,out_g,out_b"
    )?;

    write_case(&mut writer, "neutral", 0.0, &DevelopSettings::default())?;
    for value in [-100.0, -50.0, 50.0, 100.0] {
        let saturation = DevelopSettings {
            saturation: value,
            ..Default::default()
        };
        write_case(&mut writer, "saturation", value, &saturation)?;
        let exposure = DevelopSettings {
            exposure: value,
            ..Default::default()
        };
        write_case(&mut writer, "exposure", value, &exposure)?;
        for band in 0..8 {
            let mut mixer = DevelopSettings::default();
            mixer.mixer_saturation[band] = value;
            write_case(
                &mut writer,
                &format!("mixer_saturation_{band}"),
                value,
                &mixer,
            )?;
        }
    }
    writer.flush()?;
    eprintln!("wrote {}", output.display());
    Ok(())
}

fn write_case(
    writer: &mut impl Write,
    control: &str,
    value: f32,
    settings: &DevelopSettings,
) -> std::io::Result<()> {
    for &(name, input) in VECTORS {
        let output = eval_scene_pixel(input, settings, BaseLook::Raw);
        writeln!(
            writer,
            "iai-scene-v1,raw,{control},{value:.1},{name},{:.8},{:.8},{:.8},{:.8},{:.8},{:.8}",
            input[0], input[1], input[2], output[0], output[1], output[2]
        )?;
    }
    Ok(())
}
