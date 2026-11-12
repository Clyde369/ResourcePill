//! Data for the Recordings window (the pill writes and opens the window itself; see
//! src/recorder.rs `open_viewer`). Each recording folder gets a rec.js that hands its data to the
//! page as it loads.

use std::path::Path;

pub fn write_rec(dir: &Path, rec: &serde_json::Value) -> Result<(), String> {
    std::fs::write(dir.join("rec.js"), format!("window.RP_ADD && RP_ADD(document.currentScript, {rec});\n")).map_err(|e| e.to_string())
}
