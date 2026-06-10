use anyhow::{Context, Result, bail};
use encoding_rs::{SHIFT_JIS, UTF_8};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const SIGNATURE: &[u8; 4] = b"@@@@";
const HEADER_SIZE: usize = 24;

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct Config {
    pub input_path: PathBuf,
    pub output_dir: PathBuf,
    #[serde(default = "default_output_file_name")]
    pub output_file_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunSummary {
    pub records: usize,
    pub channels: usize,
    pub sampling_frequency_hz: f64,
    pub output_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterEncoding {
    ShiftJis,
    Utf8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalogDataType {
    Short,
    Long,
    Float,
    Double,
}

impl AnalogDataType {
    fn from_code(code: u16) -> Result<Self> {
        match code {
            1 => Ok(Self::Short),
            2 => Ok(Self::Long),
            3 => Ok(Self::Float),
            4 => Ok(Self::Double),
            _ => bail!("unsupported analog data type code: {code}"),
        }
    }

    fn byte_len(self) -> usize {
        match self {
            Self::Short => 2,
            Self::Long | Self::Float => 4,
            Self::Double => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Ks3Document {
    pub model: String,
    pub file_memo: String,
    pub sampling_frequency_hz: f64,
    pub internal_counter_enabled: bool,
    pub marker_bit_count: u16,
    pub channel_count: usize,
    pub analog_data_type: AnalogDataType,
    pub channel_numbers: Vec<u16>,
    pub ad_full_scales: Vec<u32>,
    pub range_coefficients: Vec<f64>,
    pub engineering_coefficients: Vec<f64>,
    pub offset_zero_enabled: Vec<bool>,
    pub offsets: Vec<f64>,
    pub offset_zero_values: Vec<f64>,
    pub cable_coefficients: Vec<f64>,
    pub arbitrary_coefficients: Vec<f64>,
    pub channel_names: Vec<String>,
    pub ranges: Vec<String>,
    pub units: Vec<String>,
    pub start_datetime: String,
    pub records: Vec<Vec<f64>>,
}

fn default_output_file_name() -> String {
    "output.csv".to_string()
}

pub fn load_config<P: AsRef<Path>>(path: P) -> Result<Config> {
    let text = fs::read_to_string(path.as_ref())
        .with_context(|| format!("failed to read config: {}", path.as_ref().display()))?;
    toml::from_str(&text).context("failed to parse TOML config")
}

pub fn save_config<P: AsRef<Path>>(path: P, config: &Config) -> Result<()> {
    let text = toml::to_string_pretty(config).context("failed to serialize config to TOML")?;
    fs::write(path.as_ref(), text)
        .with_context(|| format!("failed to write config: {}", path.as_ref().display()))
}

pub fn run_pipeline(config: &mut Config) -> Result<RunSummary> {
    validate_config(config)?;
    fs::create_dir_all(&config.output_dir).with_context(|| {
        format!(
            "failed to create output dir: {}",
            config.output_dir.display()
        )
    })?;

    let bytes = fs::read(&config.input_path)
        .with_context(|| format!("failed to read input file: {}", config.input_path.display()))?;
    let document = parse_ks3(&bytes)?;
    let output_path = config.output_dir.join(&config.output_file_name);
    write_sample_compatible_csv(&output_path, &document)?;

    Ok(RunSummary {
        records: document.records.len(),
        channels: document.channel_count,
        sampling_frequency_hz: document.sampling_frequency_hz,
        output_path,
    })
}

pub fn validate_config(config: &Config) -> Result<()> {
    if config.output_file_name.trim().is_empty() {
        bail!("output_file_name must not be empty");
    }
    Ok(())
}

pub fn parse_ks3(bytes: &[u8]) -> Result<Ks3Document> {
    let mut builder = Ks3Builder::default();
    let mut pos = 0usize;

    while pos < bytes.len() {
        if bytes.len() - pos < HEADER_SIZE {
            bail!("trailing bytes are shorter than KS3 identifier header at offset {pos}");
        }
        let item = HeaderItem::read(bytes, pos)?;
        let data_start = pos + HEADER_SIZE;
        let data_end = data_start
            .checked_add(item.data_bytes)
            .context("overflow while reading KS3 data item")?;
        if data_end > bytes.len() {
            bail!(
                "KS3 item 0x{:04x}/0x{:04x} exceeds file size",
                item.major,
                item.minor
            );
        }
        builder.accept(
            item.major,
            item.minor,
            item.item_bytes,
            &bytes[data_start..data_end],
        )?;
        pos = data_end;
    }

    builder.finish()
}

pub fn write_sample_compatible_csv(path: &Path, document: &Ks3Document) -> Result<()> {
    let mut text = String::new();
    push_quoted_row(&mut text, &["ID番号", &document.model]);
    push_quoted_row(&mut text, &["タイトル", &document.file_memo]);

    let (date, time) = split_datetime(&document.start_datetime);
    push_mixed_row(
        &mut text,
        &[Cell::Text("試験日時"), Cell::Text(date), Cell::Text(time)],
    );
    push_mixed_row(
        &mut text,
        &[
            Cell::Text("測定CH数"),
            Cell::Raw(document.channel_count.to_string()),
        ],
    );
    push_quoted_row(&mut text, &["デジタル入力", "OFF"]);
    push_mixed_row(
        &mut text,
        &[
            Cell::Text("サンプリング周波数(Hz)"),
            Cell::Raw(format_number(document.sampling_frequency_hz)),
        ],
    );
    push_mixed_row(
        &mut text,
        &[
            Cell::Text("集録データ数/CH"),
            Cell::Raw(document.records.len().to_string()),
        ],
    );
    push_mixed_row(
        &mut text,
        &[
            Cell::Text("測定時間(sec)"),
            Cell::Raw(format_number(
                document.records.len() as f64 / document.sampling_frequency_hz,
            )),
        ],
    );

    push_quoted_row(&mut text, &["CH名称"]);
    push_mixed_row(
        &mut text,
        &std::iter::once(Cell::Text("CH No"))
            .chain(
                document
                    .channel_labels()
                    .into_iter()
                    .map(|label| Cell::TextOwned(label)),
            )
            .collect::<Vec<_>>(),
    );
    push_channel_numeric_row(&mut text, "レンジ", &document.ranges_as_numbers());
    push_channel_numeric_row(&mut text, "校正係数", &document.calibration_coefficients());
    push_channel_numeric_row(&mut text, "オフセット", &document.offsets);
    push_mixed_row(
        &mut text,
        &std::iter::once(Cell::Text("単位"))
            .chain(document.units.iter().map(|unit| Cell::Text(unit)))
            .collect::<Vec<_>>(),
    );

    let dt = 1.0 / document.sampling_frequency_hz;
    for (index, row) in document.records.iter().enumerate() {
        let mut cells = Vec::with_capacity(row.len() + 1);
        cells.push(Cell::Raw(format!("{:.3}", index as f64 * dt)));
        cells.extend(row.iter().map(|value| Cell::Raw(value.to_string())));
        push_mixed_row(&mut text, &cells);
    }

    let (encoded, _, had_errors) = SHIFT_JIS.encode(&text);
    if had_errors {
        bail!("failed to encode CSV as Shift-JIS");
    }
    fs::write(path, encoded.as_ref())
        .with_context(|| format!("failed to write CSV: {}", path.display()))
}

impl Ks3Document {
    fn channel_labels(&self) -> Vec<String> {
        self.channel_numbers
            .iter()
            .enumerate()
            .map(|(index, ch)| {
                if *ch == 0 {
                    format!("CH{}", index + 1)
                } else {
                    format!("CH{ch}")
                }
            })
            .collect()
    }

    fn ranges_as_numbers(&self) -> Vec<f64> {
        self.range_coefficients
            .iter()
            .zip(&self.ad_full_scales)
            .map(|(coefficient, full_scale)| coefficient * *full_scale as f64)
            .collect()
    }

    fn calibration_coefficients(&self) -> Vec<f64> {
        self.engineering_coefficients
            .iter()
            .zip(&self.range_coefficients)
            .map(|(engineering, range)| {
                if *range == 0.0 {
                    0.0
                } else {
                    engineering / range
                }
            })
            .collect()
    }
}

#[derive(Default)]
struct Ks3Builder {
    encoding: Option<CharacterEncoding>,
    model: Option<String>,
    file_memo: Option<String>,
    sampling_frequency_hz: Option<f64>,
    internal_counter_enabled: Option<bool>,
    marker_bit_count: Option<u16>,
    channel_count: Option<usize>,
    analog_data_type: Option<AnalogDataType>,
    channel_numbers: Option<Vec<u16>>,
    ad_full_scales: Option<Vec<u32>>,
    range_coefficients: Option<Vec<f64>>,
    engineering_coefficients: Option<Vec<f64>>,
    offset_zero_enabled: Option<Vec<bool>>,
    offsets: Option<Vec<f64>>,
    offset_zero_values: Option<Vec<f64>>,
    cable_coefficients: Option<Vec<f64>>,
    arbitrary_coefficients: Option<Vec<f64>>,
    channel_names: Option<Vec<String>>,
    ranges: Option<Vec<String>>,
    units: Option<Vec<String>>,
    start_datetime: Option<String>,
    raw_analog_data: Option<Vec<u8>>,
}

impl Ks3Builder {
    fn accept(&mut self, major: u16, minor: u16, item_bytes: usize, data: &[u8]) -> Result<()> {
        match (major, minor) {
            (0x0001, 0x0001) => {
                self.encoding = Some(match read_u16(data)? {
                    0 => CharacterEncoding::ShiftJis,
                    1 => CharacterEncoding::Utf8,
                    code => bail!("unsupported character encoding code: {code}"),
                });
            }
            (0x0001, 0x0003) => self.model = Some(self.decode_first_string(data)),
            (0x0001, 0x0004) => self.file_memo = Some(self.decode_first_string(data)),
            (0x0010, 0x0001) => {
                let parts = read_u32_vec(data)?;
                if parts.len() != 2 {
                    bail!("sampling frequency item must contain two DWORD values");
                }
                self.sampling_frequency_hz = Some(parts[0] as f64 + parts[1] as f64 * 1e-8);
            }
            (0x0010, 0x0004) => self.internal_counter_enabled = Some(read_u16(data)? != 0),
            (0x0010, 0x0008) => self.marker_bit_count = Some(read_u16(data)?),
            (0x0020, 0x0001) => self.channel_count = Some(read_u16(data)? as usize),
            (0x0020, 0x0004) => {
                self.analog_data_type = Some(AnalogDataType::from_code(read_u16(data)?)?)
            }
            (0x0020, 0x0008) => self.ad_full_scales = Some(read_u32_vec(data)?),
            (0x0020, 0x0009) => self.channel_numbers = Some(read_u16_vec(data)?),
            (0x0020, 0x000d) => {
                self.offset_zero_enabled =
                    Some(read_u16_vec(data)?.into_iter().map(|v| v != 0).collect());
            }
            (0x0020, 0x0019) => self.range_coefficients = Some(read_f64_vec(data)?),
            (0x0020, 0x001a) => self.engineering_coefficients = Some(read_f64_vec(data)?),
            (0x0020, 0x0020) => self.offsets = Some(self.decode_number_strings(data, item_bytes)?),
            (0x0020, 0x0021) => {
                self.offset_zero_values = Some(self.decode_number_strings(data, item_bytes)?)
            }
            (0x0020, 0x0022) => {
                self.cable_coefficients = Some(self.decode_number_strings(data, item_bytes)?)
            }
            (0x0020, 0x0023) => {
                self.arbitrary_coefficients = Some(self.decode_number_strings(data, item_bytes)?)
            }
            (0x0020, 0x0026) => {
                self.channel_names = Some(self.decode_string_vec(data, item_bytes)?)
            }
            (0x0020, 0x002b) => self.ranges = Some(self.decode_string_vec(data, item_bytes)?),
            (0x0020, 0x0035) => self.units = Some(self.decode_string_vec(data, item_bytes)?),
            (0x4000, 0x0001) => self.start_datetime = Some(self.decode_first_string(data)),
            (0x8000, 0x0001) => self.raw_analog_data = Some(data.to_vec()),
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<Ks3Document> {
        let channel_count = self.channel_count.context("missing analog channel count")?;
        if channel_count == 0 {
            bail!("analog channel count must not be 0");
        }
        let analog_data_type = self.analog_data_type.context("missing analog data type")?;
        let sampling_frequency_hz = self
            .sampling_frequency_hz
            .context("missing sampling frequency")?;
        if sampling_frequency_hz <= 0.0 {
            bail!("sampling frequency must be greater than 0");
        }

        let channel_numbers = require_len(
            self.channel_numbers.context("missing channel numbers")?,
            channel_count,
            "channel numbers",
        )?;
        let ad_full_scales = require_len(
            self.ad_full_scales.context("missing AD full scales")?,
            channel_count,
            "AD full scales",
        )?;
        let range_coefficients = require_len(
            self.range_coefficients
                .context("missing range coefficients")?,
            channel_count,
            "range coefficients",
        )?;
        let engineering_coefficients = require_len(
            self.engineering_coefficients
                .context("missing engineering coefficients")?,
            channel_count,
            "engineering coefficients",
        )?;
        let offset_zero_enabled = normalize_len(self.offset_zero_enabled, channel_count, false);
        let offsets = normalize_len(self.offsets, channel_count, 0.0);
        let offset_zero_values = normalize_len(self.offset_zero_values, channel_count, 0.0);
        let cable_coefficients = normalize_len(self.cable_coefficients, channel_count, 1.0)
            .into_iter()
            .map(|v| if v == 0.0 { 1.0 } else { v })
            .collect::<Vec<_>>();
        let arbitrary_coefficients = normalize_len(self.arbitrary_coefficients, channel_count, 1.0);
        let channel_names = normalize_len(self.channel_names, channel_count, String::new());
        let ranges = normalize_len(self.ranges, channel_count, String::new());
        let units = normalize_len(self.units, channel_count, String::new());
        let raw = self.raw_analog_data.context("missing analog data block")?;
        let records = parse_analog_records(
            &raw,
            channel_count,
            analog_data_type,
            self.internal_counter_enabled.unwrap_or(false),
            &engineering_coefficients,
            &offset_zero_enabled,
            &offsets,
            &offset_zero_values,
            &cable_coefficients,
            &arbitrary_coefficients,
        )?;

        Ok(Ks3Document {
            model: self.model.unwrap_or_default(),
            file_memo: self.file_memo.unwrap_or_default(),
            sampling_frequency_hz,
            internal_counter_enabled: self.internal_counter_enabled.unwrap_or(false),
            marker_bit_count: self.marker_bit_count.unwrap_or(0),
            channel_count,
            analog_data_type,
            channel_numbers,
            ad_full_scales,
            range_coefficients,
            engineering_coefficients,
            offset_zero_enabled,
            offsets,
            offset_zero_values,
            cable_coefficients,
            arbitrary_coefficients,
            channel_names,
            ranges,
            units,
            start_datetime: self.start_datetime.unwrap_or_default(),
            records,
        })
    }

    fn encoding(&self) -> CharacterEncoding {
        self.encoding.unwrap_or(CharacterEncoding::ShiftJis)
    }

    fn decode_first_string(&self, data: &[u8]) -> String {
        decode_null_terminated(data, self.encoding())
    }

    fn decode_string_vec(&self, data: &[u8], item_bytes: usize) -> Result<Vec<String>> {
        if item_bytes == 0 {
            bail!("string item has variable item size; fixed size is required here");
        }
        if !data.len().is_multiple_of(item_bytes) {
            bail!("string data length is not a multiple of item size");
        }
        Ok(data
            .chunks_exact(item_bytes)
            .map(|chunk| decode_null_terminated(chunk, self.encoding()))
            .collect())
    }

    fn decode_number_strings(&self, data: &[u8], item_bytes: usize) -> Result<Vec<f64>> {
        self.decode_string_vec(data, item_bytes)?
            .into_iter()
            .map(|s| {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    Ok(0.0)
                } else {
                    trimmed
                        .parse::<f64>()
                        .with_context(|| format!("failed to parse numeric string: {trimmed:?}"))
                }
            })
            .collect()
    }
}

#[derive(Debug)]
struct HeaderItem {
    major: u16,
    minor: u16,
    item_bytes: usize,
    data_bytes: usize,
}

impl HeaderItem {
    fn read(bytes: &[u8], offset: usize) -> Result<Self> {
        if &bytes[offset..offset + 4] != SIGNATURE {
            bail!("invalid KS3 signature at offset {offset}");
        }
        Ok(Self {
            major: u16::from_le_bytes(bytes[offset + 4..offset + 6].try_into()?),
            minor: u16::from_le_bytes(bytes[offset + 6..offset + 8].try_into()?),
            item_bytes: u64::from_le_bytes(bytes[offset + 8..offset + 16].try_into()?)
                .try_into()
                .context("item byte size does not fit usize")?,
            data_bytes: u64::from_le_bytes(bytes[offset + 16..offset + 24].try_into()?)
                .try_into()
                .context("data byte size does not fit usize")?,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn parse_analog_records(
    data: &[u8],
    channel_count: usize,
    data_type: AnalogDataType,
    counter_enabled: bool,
    engineering_coefficients: &[f64],
    offset_zero_enabled: &[bool],
    offsets: &[f64],
    offset_zero_values: &[f64],
    cable_coefficients: &[f64],
    arbitrary_coefficients: &[f64],
) -> Result<Vec<Vec<f64>>> {
    let channel_bytes = data_type.byte_len() * channel_count;
    let record_bytes = channel_bytes + if counter_enabled { 8 } else { 0 };
    if record_bytes == 0 || !data.len().is_multiple_of(record_bytes) {
        bail!(
            "analog data length ({}) is not a multiple of record size ({record_bytes})",
            data.len()
        );
    }

    let mut records = Vec::with_capacity(data.len() / record_bytes);
    for record in data.chunks_exact(record_bytes) {
        let mut row = Vec::with_capacity(channel_count);
        for ch in 0..channel_count {
            let start = ch * data_type.byte_len();
            let raw = match data_type {
                AnalogDataType::Short => {
                    i16::from_le_bytes(record[start..start + 2].try_into()?) as f64
                }
                AnalogDataType::Long => {
                    i32::from_le_bytes(record[start..start + 4].try_into()?) as f64
                }
                AnalogDataType::Float => {
                    f32::from_le_bytes(record[start..start + 4].try_into()?) as f64
                }
                AnalogDataType::Double => f64::from_le_bytes(record[start..start + 8].try_into()?),
            };
            let zero = if offset_zero_enabled[ch] {
                offset_zero_values[ch]
            } else {
                0.0
            };
            row.push(
                raw * engineering_coefficients[ch]
                    * cable_coefficients[ch]
                    * arbitrary_coefficients[ch]
                    + offsets[ch]
                    + zero,
            );
        }
        records.push(row);
    }
    Ok(records)
}

fn decode_null_terminated(bytes: &[u8], encoding: CharacterEncoding) -> String {
    let field = bytes.split(|b| *b == 0).next().unwrap_or(bytes);
    let (decoded, _, _) = match encoding {
        CharacterEncoding::ShiftJis => SHIFT_JIS.decode(field),
        CharacterEncoding::Utf8 => UTF_8.decode(field),
    };
    decoded.trim_end().to_string()
}

fn read_u16(data: &[u8]) -> Result<u16> {
    if data.len() != 2 {
        bail!("expected exactly 2 bytes, got {}", data.len());
    }
    Ok(u16::from_le_bytes(data.try_into()?))
}

fn read_u16_vec(data: &[u8]) -> Result<Vec<u16>> {
    if !data.len().is_multiple_of(2) {
        bail!("WORD data length is not a multiple of 2");
    }
    Ok(data
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes(chunk.try_into().unwrap()))
        .collect())
}

fn read_u32_vec(data: &[u8]) -> Result<Vec<u32>> {
    if !data.len().is_multiple_of(4) {
        bail!("DWORD data length is not a multiple of 4");
    }
    Ok(data
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect())
}

fn read_f64_vec(data: &[u8]) -> Result<Vec<f64>> {
    if !data.len().is_multiple_of(8) {
        bail!("double data length is not a multiple of 8");
    }
    Ok(data
        .chunks_exact(8)
        .map(|chunk| f64::from_le_bytes(chunk.try_into().unwrap()))
        .collect())
}

fn require_len<T>(values: Vec<T>, expected: usize, name: &str) -> Result<Vec<T>> {
    if values.len() != expected {
        bail!("{name} length must be {expected}, got {}", values.len());
    }
    Ok(values)
}

fn normalize_len<T: Clone>(values: Option<Vec<T>>, expected: usize, default: T) -> Vec<T> {
    let mut values = values.unwrap_or_default();
    values.resize(expected, default);
    values.truncate(expected);
    values
}

fn split_datetime(value: &str) -> (&str, &str) {
    let mut parts = value.split_whitespace();
    let date = parts.next().unwrap_or("");
    let time = parts.next().unwrap_or("");
    let time_without_millis = time.split('.').next().unwrap_or(time);
    (date, time_without_millis)
}

fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

enum Cell<'a> {
    Text(&'a str),
    TextOwned(String),
    Raw(String),
}

fn push_quoted_row(out: &mut String, cells: &[&str]) {
    push_mixed_row(
        out,
        &cells
            .iter()
            .map(|cell| Cell::Text(cell))
            .collect::<Vec<_>>(),
    );
}

fn push_channel_numeric_row(out: &mut String, label: &str, values: &[f64]) {
    let cells = std::iter::once(Cell::Text(label))
        .chain(values.iter().map(|value| Cell::Raw(format_number(*value))))
        .collect::<Vec<_>>();
    push_mixed_row(out, &cells);
}

fn push_mixed_row(out: &mut String, cells: &[Cell<'_>]) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        match cell {
            Cell::Text(s) => push_csv_quoted(out, s),
            Cell::TextOwned(s) => push_csv_quoted(out, s),
            Cell::Raw(s) => out.push_str(s),
        }
    }
    out.push_str("\r\n");
}

fn push_csv_quoted(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn item(major: u16, minor: u16, item_bytes: u64, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(SIGNATURE);
        out.extend_from_slice(&major.to_le_bytes());
        out.extend_from_slice(&minor.to_le_bytes());
        out.extend_from_slice(&item_bytes.to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    fn fixed_str(value: &str, len: usize) -> Vec<u8> {
        let (encoded, _, had_errors) = SHIFT_JIS.encode(value);
        assert!(!had_errors);
        let mut out = vec![0u8; len];
        out[..encoded.len()].copy_from_slice(encoded.as_ref());
        out
    }

    fn fixed_strs(values: &[&str], len: usize) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| fixed_str(value, len))
            .collect()
    }

    fn words(values: &[u16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn dwords(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn doubles(values: &[f64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn base_ks3(data_type: u16, analog_data: Vec<u8>, counter: bool) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(item(0x0001, 0x0001, 2, &0u16.to_le_bytes()));
        out.extend(item(0x0001, 0x0003, 32, &fixed_str("CTRS-100A", 32)));
        out.extend(item(0x0001, 0x0004, 128, &fixed_str("", 128)));
        out.extend(item(0x0010, 0x0001, 4, &dwords(&[1000, 0])));
        out.extend(item(0x0010, 0x0004, 2, &(counter as u16).to_le_bytes()));
        out.extend(item(0x0010, 0x0008, 2, &0u16.to_le_bytes()));
        out.extend(item(0x0020, 0x0001, 2, &2u16.to_le_bytes()));
        out.extend(item(0x0020, 0x0004, 2, &data_type.to_le_bytes()));
        out.extend(item(0x0020, 0x0008, 4, &dwords(&[8192000, 8192000])));
        out.extend(item(0x0020, 0x0009, 2, &words(&[1, 2])));
        out.extend(item(0x0020, 0x000d, 2, &words(&[0, 1])));
        out.extend(item(
            0x0020,
            0x0019,
            8,
            &doubles(&[0.0001220703125, 0.0001220703125]),
        ));
        out.extend(item(
            0x0020,
            0x001a,
            8,
            &doubles(&[0.0001220703125, 0.0001220703125]),
        ));
        out.extend(item(0x0020, 0x0020, 32, &fixed_strs(&["0.0", "1.0"], 32)));
        out.extend(item(0x0020, 0x0021, 32, &fixed_strs(&["0.0", "2.0"], 32)));
        out.extend(item(0x0020, 0x0022, 32, &fixed_strs(&["1.0", "1.0"], 32)));
        out.extend(item(0x0020, 0x0023, 32, &fixed_strs(&["1.0", "1.0"], 32)));
        out.extend(item(0x0020, 0x0026, 128, &fixed_strs(&["", ""], 128)));
        out.extend(item(0x0020, 0x002b, 32, &fixed_strs(&["1kμε", "1kμε"], 32)));
        out.extend(item(0x0020, 0x0035, 32, &fixed_strs(&["με", "με"], 32)));
        out.extend(item(
            0x4000,
            0x0001,
            32,
            &fixed_str("2026/06/09 17:58:22.267", 32),
        ));
        out.extend(item(0x9999, 0x0001, 4, &[1, 2, 3, 4]));
        out.extend(item(0x8000, 0x0001, 0, &analog_data));
        out
    }

    #[test]
    fn load_save_roundtrip() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.toml");
        let dst = dir.path().join("dst.toml");
        fs::write(
            &src,
            format!(
                "input_path = \"{}\"\noutput_dir = \"{}\"\noutput_file_name = \"out.csv\"\n",
                dir.path().join("a.KS3").display(),
                dir.path().join("out").display()
            ),
        )
        .unwrap();
        let config = load_config(&src).unwrap();
        save_config(&dst, &config).unwrap();
        assert_eq!(config, load_config(&dst).unwrap());
    }

    #[test]
    fn default_output_name() {
        let config: Config =
            toml::from_str("input_path = \"a.KS3\"\noutput_dir = \"out\"\n").unwrap();
        assert_eq!(config.output_file_name, "output.csv");
    }

    #[test]
    fn parse_long_records_and_scale() {
        let raw = [1000_i32, -1000, 2000, -2000]
            .into_iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let doc = parse_ks3(&base_ks3(2, raw, false)).unwrap();
        assert_eq!(doc.channel_count, 2);
        assert_eq!(doc.records.len(), 2);
        assert_eq!(doc.records[0][0], 0.1220703125);
        assert_eq!(doc.records[0][1], 1.0 - 0.1220703125 + 2.0);
    }

    #[test]
    fn parse_counter_enabled_records() {
        let raw = [1_i32, 2]
            .into_iter()
            .flat_map(|v| v.to_le_bytes())
            .chain(99_u64.to_le_bytes())
            .collect();
        let doc = parse_ks3(&base_ks3(2, raw, true)).unwrap();
        assert_eq!(doc.records.len(), 1);
        assert!(doc.internal_counter_enabled);
    }

    #[test]
    fn parse_short_float_double_types() {
        let short_raw = [10_i16, 20]
            .into_iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(
            parse_ks3(&base_ks3(1, short_raw, false))
                .unwrap()
                .records
                .len(),
            1
        );

        let float_raw = [10.0_f32, 20.0]
            .into_iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(
            parse_ks3(&base_ks3(3, float_raw, false))
                .unwrap()
                .records
                .len(),
            1
        );

        let double_raw = [10.0_f64, 20.0]
            .into_iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        assert_eq!(
            parse_ks3(&base_ks3(4, double_raw, false))
                .unwrap()
                .records
                .len(),
            1
        );
    }

    #[test]
    fn write_csv_is_shift_jis_and_sample_shaped() {
        let dir = tempdir().unwrap();
        let raw = [1000_i32, -1000]
            .into_iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let doc = parse_ks3(&base_ks3(2, raw, false)).unwrap();
        let path = dir.path().join("out.csv");
        write_sample_compatible_csv(&path, &doc).unwrap();
        let bytes = fs::read(path).unwrap();
        let (decoded, _, had_errors) = SHIFT_JIS.decode(&bytes);
        assert!(!had_errors);
        let text = decoded.as_ref();
        assert!(text.contains("\"ID番号\",\"CTRS-100A\"\r\n"));
        assert!(text.contains("\"試験日時\",\"2026/06/09\",\"17:58:22\"\r\n"));
        assert!(text.contains("\"CH No\",\"CH1\",\"CH2\"\r\n"));
        assert!(text.contains("0.000,0.1220703125,2.8779296875\r\n"));
    }

    #[test]
    fn run_pipeline_writes_csv() {
        let dir = tempdir().unwrap();
        let input = dir.path().join("in.KS3");
        let output_dir = dir.path().join("out");
        let raw = [1_i32, 2]
            .into_iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        fs::write(&input, base_ks3(2, raw, false)).unwrap();
        let mut config = Config {
            input_path: input,
            output_dir: output_dir.clone(),
            output_file_name: "result.csv".into(),
        };
        let summary = run_pipeline(&mut config).unwrap();
        assert_eq!(summary.records, 1);
        assert_eq!(summary.channels, 2);
        assert!(output_dir.join("result.csv").exists());
    }

    #[test]
    fn invalid_inputs_fail() {
        assert!(parse_ks3(b"bad").is_err());
        let mut bad = base_ks3(9, Vec::new(), false);
        assert!(parse_ks3(&bad).is_err());
        bad = base_ks3(2, vec![0; 3], false);
        assert!(parse_ks3(&bad).is_err());
        assert!(
            validate_config(&Config {
                input_path: PathBuf::from("a.KS3"),
                output_dir: PathBuf::from("out"),
                output_file_name: String::new(),
            })
            .is_err()
        );
    }

    #[test]
    #[ignore]
    fn local_samples_match_reference_csv() {
        let Some(sample_dir) = std::env::var_os("KS3_SAMPLE_DIR") else {
            return;
        };
        let sample_dir = PathBuf::from(sample_dir);
        for entry in fs::read_dir(&sample_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) != Some("KS3") {
                continue;
            }
            let expected = path.with_extension("CSV");
            if !expected.exists() {
                continue;
            }
            let dir = tempdir().unwrap();
            let mut config = Config {
                input_path: path.clone(),
                output_dir: dir.path().to_path_buf(),
                output_file_name: "out.csv".into(),
            };
            run_pipeline(&mut config).unwrap();
            let actual_bytes = fs::read(dir.path().join("out.csv")).unwrap();
            let expected_bytes = fs::read(&expected).unwrap();
            assert_eq!(actual_bytes, expected_bytes, "{}", path.display());
        }
    }
}
