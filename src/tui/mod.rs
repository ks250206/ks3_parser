mod app;
mod clipboard;
mod ui;

use anyhow::Result;
use std::path::PathBuf;

pub fn run(config_path: PathBuf) -> Result<()> {
    app::run_app(config_path)
}
