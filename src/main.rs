mod tui;

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use ks2_parser::{load_config, run_pipeline};

#[derive(Parser)]
#[command(name = "ks2_parser", about = "ks2 を CSV に変換（デフォルトは TUI）")]
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
        tui::run(args.config)
    }
}

fn run_cli(path: &PathBuf) -> Result<()> {
    let mut config = load_config(path)?;
    let summary = run_pipeline(&mut config)?;

    println!("done");
    println!("input: {}", config.input_path.display());
    println!("records: {}", summary.records);
    println!("variable_header_byte: {}", summary.variable_header_byte);
    println!("data_header_byte: {}", summary.data_header_byte);
    println!("footer_byte: {}", summary.footer_byte);
    println!("output dir: {}", config.output_dir.display());
    println!("output file: {}", config.output_file_name);

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    #[test]
    fn run_cli_end_to_end_writes_csv() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("cli.ks2");
        let mut raw = vec![0u8; 4];
        for v in [1_i32, 2, 3, 4] {
            raw.extend(v.to_le_bytes());
        }
        fs::write(&ks2, raw).unwrap();
        let out = dir.path().join("csv_out");
        let cfg = dir.path().join("cli.toml");
        let body = format!(
            r#"input_path = "{}"
output_dir = "{}"
output_file_name = "r.csv"
auto_detect_offsets = false
header_byte = 4
variable_header_byte = 0
data_header_byte = 0
data_skip_byte = 0
footer_byte = 0
values_per_record = 4
endianness = "little"
ADConverterScale = 1.0
ADRangeCoefficient = 1.0
ADCoefficient = 1.0

[coefficient]
CH1 = 1.0
CH2 = 1.0
CH3 = 1.0
CH4 = 1.0
"#,
            ks2.display(),
            out.display()
        );
        fs::write(&cfg, body).unwrap();

        super::run_cli(&cfg).expect("run_cli");
        let csv = fs::read_to_string(out.join("r.csv")).unwrap();
        assert!(csv.contains("index,ch1,ch2,ch3,ch4"));
        assert!(csv.contains("0,1,2,3,4"));
    }
}
