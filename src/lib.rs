use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub input_path: PathBuf,
    pub output_dir: PathBuf,
    #[serde(default = "default_output_file_name")]
    pub output_file_name: String,
    #[serde(default)]
    pub auto_detect_offsets: bool,

    pub header_byte: usize,
    pub variable_header_byte: usize,
    pub data_header_byte: usize,
    pub data_skip_byte: usize,
    pub footer_byte: usize,

    pub values_per_record: usize,
    pub endianness: Endianness,
    #[serde(rename = "ADConverterScale")]
    pub ad_converter_scale: f64,
    #[serde(rename = "ADRangeCoefficient")]
    pub ad_range_coefficient: f64,
    #[serde(rename = "ADCoefficient")]
    pub ad_coefficient: f64,
    pub coefficient: ChannelCoefficient,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ChannelCoefficient {
    #[serde(rename = "CH1")]
    pub ch1: f64,
    #[serde(rename = "CH2")]
    pub ch2: f64,
    #[serde(rename = "CH3")]
    pub ch3: f64,
    #[serde(rename = "CH4")]
    pub ch4: f64,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Endianness {
    Little,
    Big,
}

#[derive(Debug, Clone)]
pub struct RunSummary {
    pub records: usize,
    pub variable_header_byte: usize,
    pub data_header_byte: usize,
    pub footer_byte: usize,
}

fn default_output_file_name() -> String {
    "output.csv".to_string()
}

pub fn load_config<P: AsRef<Path>>(path: P) -> Result<Config> {
    let text = fs::read_to_string(path.as_ref())
        .with_context(|| format!("failed to read config: {}", path.as_ref().display()))?;
    let config: Config = toml::from_str(&text).context("failed to parse TOML config")?;
    Ok(config)
}

pub fn save_config<P: AsRef<Path>>(path: P, config: &Config) -> Result<()> {
    let text = toml::to_string_pretty(config).context("failed to serialize config to TOML")?;
    fs::write(path.as_ref(), text)
        .with_context(|| format!("failed to write config: {}", path.as_ref().display()))?;
    Ok(())
}

pub fn run_pipeline(config: &mut Config) -> Result<RunSummary> {
    fs::create_dir_all(&config.output_dir)
        .with_context(|| format!("failed to create output dir: {}", config.output_dir.display()))?;

    let bytes = fs::read(&config.input_path)
        .with_context(|| format!("failed to read input file: {}", config.input_path.display()))?;

    if config.auto_detect_offsets {
        apply_auto_detected_offsets(config, &bytes)?;
    }

    validate_config(config)?;

    let data_region = extract_data_region(&bytes, config)?;
    let records = parse_records(data_region, config)?;

    write_combined_csv(config, &records)?;

    Ok(RunSummary {
        records: records.len(),
        variable_header_byte: config.variable_header_byte,
        data_header_byte: config.data_header_byte,
        footer_byte: config.footer_byte,
    })
}

pub fn validate_config(config: &Config) -> Result<()> {
    if config.values_per_record != 4 {
        bail!(
            "this program expects values_per_record = 4, but got {}",
            config.values_per_record
        );
    }
    if config.ad_converter_scale == 0.0 {
        bail!("ADConverterScale must not be 0");
    }
    Ok(())
}

fn apply_auto_detected_offsets(config: &mut Config, bytes: &[u8]) -> Result<()> {
    config.variable_header_byte = parse_usize_after_crlf(bytes, 12)?;
    config.data_header_byte = parse_usize_after_crlf(bytes, 13)?;
    config.footer_byte = parse_usize_after_crlf(bytes, 14)?;
    Ok(())
}

fn parse_usize_after_crlf(bytes: &[u8], crlf_index_1based: usize) -> Result<usize> {
    let start = nth_crlf_end(bytes, crlf_index_1based)
        .with_context(|| format!("failed to find the {crlf_index_1based}th CRLF"))?;
    let field = bytes
        .get(start..start + 14)
        .with_context(|| format!("failed to read 14 bytes after the {crlf_index_1based}th CRLF"))?;
    let text = std::str::from_utf8(field)
        .with_context(|| format!("field after the {crlf_index_1based}th CRLF is not valid UTF-8"))?;
    let value = text
        .trim()
        .parse::<usize>()
        .with_context(|| format!("failed to parse integer from {:?}", text))?;
    Ok(value)
}

fn nth_crlf_end(bytes: &[u8], target_1based: usize) -> Option<usize> {
    let mut count = 0;
    let mut i = 0;

    while i + 1 < bytes.len() {
        if bytes[i] == b'\r' && bytes[i + 1] == b'\n' {
            count += 1;
            if count == target_1based {
                return Some(i + 2);
            }
            i += 2;
        } else {
            i += 1;
        }
    }

    None
}

fn extract_data_region<'a>(bytes: &'a [u8], config: &Config) -> Result<&'a [u8]> {
    let start = config
        .header_byte
        .checked_add(config.variable_header_byte)
        .and_then(|v| v.checked_add(config.data_header_byte))
        .and_then(|v| v.checked_add(config.data_skip_byte))
        .context("overflow while calculating data start offset")?;

    let end = bytes
        .len()
        .checked_sub(config.footer_byte)
        .context("footer_byte is larger than input file size")?;

    if start > end {
        bail!(
            "invalid region: start offset ({start}) is greater than end offset ({end})"
        );
    }

    Ok(&bytes[start..end])
}

fn parse_records(data: &[u8], config: &Config) -> Result<Vec<[i32; 4]>> {
    let record_size = config.values_per_record * std::mem::size_of::<i32>();

    if record_size != 16 {
        bail!("record size must be 16 bytes, but got {record_size}");
    }

    if !data.len().is_multiple_of(record_size) {
        bail!(
            "data length ({}) is not a multiple of record size ({})",
            data.len(),
            record_size
        );
    }

    let mut records = Vec::with_capacity(data.len() / record_size);

    for chunk in data.chunks_exact(record_size) {
        let mut values = [0_i32; 4];

        for (i, slot) in values.iter_mut().enumerate() {
            let start = i * 4;
            let raw: [u8; 4] = chunk[start..start + 4]
                .try_into()
                .context("failed to convert 4-byte slice into array")?;

            *slot = match config.endianness {
                Endianness::Little => i32::from_le_bytes(raw),
                Endianness::Big => i32::from_be_bytes(raw),
            };
        }

        records.push(values);
    }

    Ok(records)
}

fn write_combined_csv(config: &Config, records: &[[i32; 4]]) -> Result<()> {
    let path = config.output_dir.join(&config.output_file_name);
    let channel_coefficients = [
        config.coefficient.ch1,
        config.coefficient.ch2,
        config.coefficient.ch3,
        config.coefficient.ch4,
    ];
    let mut wtr = csv::Writer::from_path(&path)
        .with_context(|| format!("failed to open CSV for writing: {}", path.display()))?;
    wtr.write_record(["index", "ch1", "ch2", "ch3", "ch4"])
        .with_context(|| format!("failed to write header: {}", path.display()))?;

    for (index, record) in records.iter().enumerate() {
        let mut row = [String::new(), String::new(), String::new(), String::new(), String::new()];
        row[0] = index.to_string();

        for ch in 0..4 {
            let scaled_value = (record[ch] as f64 / config.ad_converter_scale)
                * config.ad_range_coefficient
                * config.ad_coefficient
                * channel_coefficients[ch];
            row[ch + 1] = scaled_value.to_string();
        }

        wtr.write_record(row)
            .with_context(|| format!("failed to write record {index} to {}", path.display()))?;
    }

    wtr.flush().context("failed to flush CSV writer")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn record_le(values: [i32; 4]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect()
    }

    fn record_be(values: [i32; 4]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect()
    }

    fn auto_detect_preamble(v: usize, d: usize, f: usize) -> Vec<u8> {
        let mut b = Vec::new();
        for _ in 0..12 {
            b.extend_from_slice(b"x\r\n");
        }
        b.extend_from_slice(format!("{v:>14}").as_bytes());
        b.extend_from_slice(b"\r\n");
        b.extend_from_slice(format!("{d:>14}").as_bytes());
        b.extend_from_slice(b"\r\n");
        b.extend_from_slice(format!("{f:>14}").as_bytes());
        b
    }

    #[test]
    fn load_save_roundtrip() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.toml");
        let dst = dir.path().join("dst.toml");
        let ks2 = dir.path().join("dummy.ks2");
        fs::write(&ks2, vec![0u8; 20]).unwrap();
        let body = format!(
            r#"input_path = "{}"
output_dir = "{}"
output_file_name = "out.csv"
auto_detect_offsets = false
header_byte = 4
variable_header_byte = 0
data_header_byte = 0
data_skip_byte = 0
footer_byte = 0
values_per_record = 4
endianness = "little"
ADConverterScale = 3.0
ADRangeCoefficient = 1.0
ADCoefficient = 1.0

[coefficient]
CH1 = 1.0
CH2 = 1.0
CH3 = 1.0
CH4 = 1.0
"#,
            ks2.display(),
            dir.path().join("outdir").display()
        );
        fs::write(&src, body).unwrap();
        let c = load_config(&src).unwrap();
        save_config(&dst, &c).unwrap();
        let c2 = load_config(&dst).unwrap();
        assert_eq!(c.input_path, c2.input_path);
        assert_eq!(c.output_dir, c2.output_dir);
        assert_eq!(c.ad_converter_scale, c2.ad_converter_scale);
    }

    #[test]
    fn load_invalid_toml_fails() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        fs::write(&path, "not_toml {{{").unwrap();
        assert!(load_config(&path).is_err());
    }

    #[test]
    fn load_missing_file_fails() {
        let p = PathBuf::from("/nonexistent/ks2_parser_config.toml");
        assert!(load_config(&p).is_err());
    }

    #[test]
    fn default_output_file_name_from_toml() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("min.toml");
        let body = r#"
input_path = "a.ks2"
output_dir = "o"
auto_detect_offsets = false
header_byte = 0
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
"#;
        fs::write(&path, body).unwrap();
        let c = load_config(&path).unwrap();
        assert_eq!(c.output_file_name, "output.csv");
    }

    #[test]
    fn run_pipeline_manual_one_record_writes_csv() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("t.ks2");
        let out_dir = dir.path().join("csv_out");
        let mut raw = vec![0u8, 0u8, 0u8, 0u8];
        raw.extend(record_le([100, 200, -50, 42]));
        fs::write(&ks2, &raw).unwrap();

        let mut c = Config {
            input_path: ks2.clone(),
            output_dir: out_dir.clone(),
            output_file_name: "out.csv".into(),
            auto_detect_offsets: false,
            header_byte: 4,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 2.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };

        let s = run_pipeline(&mut c).unwrap();
        assert_eq!(s.records, 1);

        let csv_path = out_dir.join("out.csv");
        let text = fs::read_to_string(&csv_path).unwrap();
        assert!(text.contains("index,ch1,ch2,ch3,ch4"));
        assert!(text.contains("0,50,100,-25,21"));
    }

    #[test]
    fn run_pipeline_big_endian() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("b.ks2");
        let out_dir = dir.path().join("outb");
        let mut raw = vec![0u8; 4];
        raw.extend(record_be([1, 2, 3, 4]));
        fs::write(&ks2, &raw).unwrap();

        let mut c = Config {
            input_path: ks2,
            output_dir: out_dir.clone(),
            output_file_name: "x.csv".into(),
            auto_detect_offsets: false,
            header_byte: 4,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Big,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };

        run_pipeline(&mut c).unwrap();
        let text = fs::read_to_string(out_dir.join("x.csv")).unwrap();
        assert!(text.contains(",1,2,3,4"));
    }

    #[test]
    fn validate_rejects_wrong_values_per_record() {
        let c = Config {
            input_path: PathBuf::from("a"),
            output_dir: PathBuf::from("b"),
            output_file_name: "o.csv".into(),
            auto_detect_offsets: false,
            header_byte: 0,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 3,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        assert!(validate_config(&c).is_err());
    }

    #[test]
    fn validate_rejects_zero_scale() {
        let mut c = Config {
            input_path: PathBuf::from("a"),
            output_dir: PathBuf::from("b"),
            output_file_name: "o.csv".into(),
            auto_detect_offsets: false,
            header_byte: 0,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 0.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        assert!(validate_config(&c).is_err());
        c.ad_converter_scale = 1.0;
        assert!(validate_config(&c).is_ok());
    }

    #[test]
    fn extract_region_start_gt_end_fails() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("x.ks2");
        fs::write(&ks2, vec![0u8; 32]).unwrap();
        let mut c = Config {
            input_path: ks2.clone(),
            output_dir: dir.path().join("o"),
            output_file_name: "o.csv".into(),
            auto_detect_offsets: false,
            header_byte: 100,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        assert!(run_pipeline(&mut c).is_err());
    }

    #[test]
    fn extract_footer_larger_than_file_fails() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("y.ks2");
        fs::write(&ks2, vec![0u8; 8]).unwrap();
        let mut c = Config {
            input_path: ks2,
            output_dir: dir.path().join("o"),
            output_file_name: "o.csv".into(),
            auto_detect_offsets: false,
            header_byte: 0,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 100,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        assert!(run_pipeline(&mut c).is_err());
    }

    #[test]
    fn parse_records_rejects_bad_length() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("z.ks2");
        fs::write(&ks2, vec![0u8; 4 + 15]).unwrap();
        let mut c = Config {
            input_path: ks2,
            output_dir: dir.path().join("o"),
            output_file_name: "o.csv".into(),
            auto_detect_offsets: false,
            header_byte: 4,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        assert!(run_pipeline(&mut c).is_err());
    }

    #[test]
    fn run_pipeline_with_auto_detect() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("auto.ks2");
        let out_dir = dir.path().join("out_auto");

        let v = 6usize;
        let d = 4usize;
        let f = 2usize;
        let mut body = auto_detect_preamble(v, d, f);
        let header_byte = body.len();
        body.resize(header_byte + v + d, 0u8);
        body.extend(record_le([7, 8, 9, 10]));
        body.extend(vec![0xff, 0xfe]);

        fs::write(&ks2, &body).unwrap();

        let mut c = Config {
            input_path: ks2,
            output_dir: out_dir.clone(),
            output_file_name: "a.csv".into(),
            auto_detect_offsets: true,
            header_byte,
            variable_header_byte: 999,
            data_header_byte: 999,
            data_skip_byte: 0,
            footer_byte: 999,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };

        let s = run_pipeline(&mut c).unwrap();
        assert_eq!(s.records, 1);
        assert_eq!(c.variable_header_byte, v);
        assert_eq!(c.data_header_byte, d);
        assert_eq!(c.footer_byte, f);

        let csv = fs::read_to_string(out_dir.join("a.csv")).unwrap();
        assert!(csv.contains(",7,8,9,10"));
    }

    #[test]
    fn run_pipeline_fails_when_input_missing() {
        let dir = tempdir().unwrap();
        let mut c = Config {
            input_path: dir.path().join("nope.ks2"),
            output_dir: dir.path().join("o"),
            output_file_name: "o.csv".into(),
            auto_detect_offsets: false,
            header_byte: 0,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        assert!(run_pipeline(&mut c).is_err());
    }

    #[test]
    fn overflow_in_region_start_fails() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("ov.ks2");
        fs::write(&ks2, vec![0u8; 64]).unwrap();
        let mut c = Config {
            input_path: ks2,
            output_dir: dir.path().join("o"),
            output_file_name: "o.csv".into(),
            auto_detect_offsets: false,
            header_byte: usize::MAX / 2,
            variable_header_byte: usize::MAX / 2,
            data_header_byte: usize::MAX / 2,
            data_skip_byte: 1,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        assert!(run_pipeline(&mut c).is_err());
    }

    #[test]
    fn coefficient_scales_output() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("coef.ks2");
        let out_dir = dir.path().join("oc");
        let mut raw = vec![0u8; 4];
        raw.extend(record_le([10, 10, 10, 10]));
        fs::write(&ks2, &raw).unwrap();

        let mut c = Config {
            input_path: ks2,
            output_dir: out_dir.clone(),
            output_file_name: "c.csv".into(),
            auto_detect_offsets: false,
            header_byte: 4,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 2.0,
            ad_coefficient: 3.0,
            coefficient: ChannelCoefficient {
                ch1: 2.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        run_pipeline(&mut c).unwrap();
        let text = fs::read_to_string(out_dir.join("c.csv")).unwrap();
        assert!(text.contains("0,120,60,60,60"));
    }

    #[test]
    fn auto_detect_fails_without_enough_crlfs() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("badauto.ks2");
        fs::write(&ks2, b"a\r\nb\r\n").unwrap();
        let mut c = Config {
            input_path: ks2,
            output_dir: dir.path().join("o"),
            output_file_name: "o.csv".into(),
            auto_detect_offsets: true,
            header_byte: 0,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        assert!(run_pipeline(&mut c).is_err());
    }

    #[test]
    fn two_records() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("two.ks2");
        let out_dir = dir.path().join("ot");
        let mut raw = vec![0u8; 4];
        raw.extend(record_le([1, 0, 0, 0]));
        raw.extend(record_le([2, 0, 0, 0]));
        fs::write(&ks2, &raw).unwrap();

        let mut c = Config {
            input_path: ks2,
            output_dir: out_dir.clone(),
            output_file_name: "t.csv".into(),
            auto_detect_offsets: false,
            header_byte: 4,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        let s = run_pipeline(&mut c).unwrap();
        assert_eq!(s.records, 2);
        let text = fs::read_to_string(out_dir.join("t.csv")).unwrap();
        assert!(text.contains("0,1,0,0,0"));
        assert!(text.contains("1,2,0,0,0"));
    }

    #[test]
    fn data_skip_byte_skips_bytes() {
        let dir = tempdir().unwrap();
        let ks2 = dir.path().join("skip.ks2");
        let out_dir = dir.path().join("os");
        let mut raw = vec![0u8; 4];
        raw.extend(vec![0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        raw.extend(record_le([5, 0, 0, 0]));
        fs::write(&ks2, &raw).unwrap();

        let mut c = Config {
            input_path: ks2,
            output_dir: out_dir.clone(),
            output_file_name: "s.csv".into(),
            auto_detect_offsets: false,
            header_byte: 4,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 12,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        };
        run_pipeline(&mut c).unwrap();
        let text = fs::read_to_string(out_dir.join("s.csv")).unwrap();
        assert!(text.contains("0,5,0,0,0"));
    }

    fn minimal_config(output_dir: PathBuf) -> Config {
        Config {
            input_path: PathBuf::from("in.ks2"),
            output_dir,
            output_file_name: "out.csv".into(),
            auto_detect_offsets: false,
            header_byte: 0,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        }
    }

    #[test]
    fn save_config_rejects_bad_path() {
        let c = minimal_config(PathBuf::from("o"));
        let p = PathBuf::from("/nonexistent_dir_xyz/impossible.toml");
        assert!(save_config(&p, &c).is_err());
    }

    #[test]
    fn write_combined_csv_empty_records() {
        let dir = tempdir().unwrap();
        let mut c = minimal_config(dir.path().to_path_buf());
        c.output_file_name = "empty.csv".into();
        fs::create_dir_all(&c.output_dir).unwrap();
        write_combined_csv(&c, &[]).unwrap();
        let t = fs::read_to_string(dir.path().join("empty.csv")).unwrap();
        assert!(t.starts_with("index,ch1,ch2,ch3,ch4"));
    }
}
