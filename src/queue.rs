//! Folder queue management

use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum FolderStatus {
    Pending,
    Processing,
    Complete,
    Error,
}

#[derive(Clone, Debug)]
pub struct FolderInfo {
    pub path: PathBuf,
    pub name: String,
    pub file_count: usize,
    pub status: FolderStatus,
    pub progress: f32,
    pub error_message: Option<String>,
}

/// Count CSV files in a directory
pub fn count_csv_files(path: &PathBuf) -> usize {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("csv"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Get list of CSV files in a directory, sorted
pub fn get_csv_files(path: &PathBuf) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.extension()
                        .and_then(|ext| ext.to_str())
                        .map(|ext| ext.eq_ignore_ascii_case("csv"))
                        .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    
    files.sort();
    files
}
