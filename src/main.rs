#[cfg(not(coverage))]
mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use ks3_parser::{load_config, run_pipeline};

#[derive(Parser)]
#[command(name = "ks3_parser", about = "KS3 を CSV に変換（デフォルトは TUI）")]
struct Args {
    /// 従来の CLI 一発実行モード
    #[arg(long)]
    cli: bool,
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.cli {
        run_cli(&args.config)
    } else {
        #[cfg(coverage)]
        {
            run_cli(&args.config)
        }
        #[cfg(not(coverage))]
        tui::run(args.config)
    }
}

fn run_cli(path: &PathBuf) -> Result<()> {
    let mut config = load_config(path)?;
    let summary = run_pipeline(&mut config)?;

    println!("done");
    println!("input: {}", config.input_path.display());
    println!("records: {}", summary.records);
    println!("channels: {}", summary.channels);
    println!("sampling_frequency_hz: {}", summary.sampling_frequency_hz);
    println!("output dir: {}", config.output_dir.display());
    println!("output file: {}", config.output_file_name);

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use encoding_rs::SHIFT_JIS;
    use tempfile::tempdir;

    #[test]
    fn run_cli_end_to_end_writes_csv() {
        let dir = tempdir().unwrap();
        let ks3 = dir.path().join("cli.KS3");
        fs::write(&ks3, minimal_ks3()).unwrap();
        let out = dir.path().join("csv_out");
        let cfg = dir.path().join("cli.toml");
        let body = format!(
            r#"input_path = "{}"
output_dir = "{}"
output_file_name = "r.csv"
"#,
            ks3.display(),
            out.display()
        );
        fs::write(&cfg, body).unwrap();

        super::run_cli(&cfg).expect("run_cli");
        let bytes = fs::read(out.join("r.csv")).unwrap();
        let (csv, _, _) = SHIFT_JIS.decode(&bytes);
        assert!(csv.contains("\"ID番号\",\"CTRS-100A\""));
        assert!(csv.contains("0.000,0.1220703125"));
    }

    fn item(major: u16, minor: u16, item_bytes: u64, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"@@@@");
        out.extend_from_slice(&major.to_le_bytes());
        out.extend_from_slice(&minor.to_le_bytes());
        out.extend_from_slice(&item_bytes.to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    fn fixed_str(value: &str, len: usize) -> Vec<u8> {
        let (encoded, _, _) = SHIFT_JIS.encode(value);
        let mut out = vec![0u8; len];
        out[..encoded.len()].copy_from_slice(encoded.as_ref());
        out
    }

    fn minimal_ks3() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(item(0x0001, 0x0001, 2, &0u16.to_le_bytes()));
        out.extend(item(0x0001, 0x0003, 32, &fixed_str("CTRS-100A", 32)));
        out.extend(item(0x0010, 0x0001, 4, &[232, 3, 0, 0, 0, 0, 0, 0]));
        out.extend(item(0x0020, 0x0001, 2, &1u16.to_le_bytes()));
        out.extend(item(0x0020, 0x0004, 2, &2u16.to_le_bytes()));
        out.extend(item(0x0020, 0x0008, 4, &8192000u32.to_le_bytes()));
        out.extend(item(0x0020, 0x0009, 2, &1u16.to_le_bytes()));
        out.extend(item(0x0020, 0x0019, 8, &0.0001220703125_f64.to_le_bytes()));
        out.extend(item(0x0020, 0x001a, 8, &0.0001220703125_f64.to_le_bytes()));
        out.extend(item(
            0x4000,
            0x0001,
            32,
            &fixed_str("2026/06/09 17:58:22.267", 32),
        ));
        out.extend(item(0x8000, 0x0001, 4, &1000_i32.to_le_bytes()));
        out
    }
}
