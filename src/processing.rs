//! CSV to PPI processing logic
//! 
//! Adapted from the original CSV_to_PPI_rust CLI tool

use std::f64::consts::PI;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use colorous::{Color, MAGMA, TURBO, VIRIDIS};
use image::{ImageBuffer, Rgba};
use rayon::prelude::*;

use crate::queue::{self, FolderInfo};

#[derive(Clone)]
pub struct ProcessingSettings {
    pub pulses: usize,
    pub gap_deg: f64,
    pub size: u32,
    pub colormap: String,
    pub jobs: usize,
    pub output_dir: Option<PathBuf>,
}


#[derive(Debug)]
pub enum ProgressUpdate {
    FolderStarted { folder_index: usize, folder_name: String },
    FileProgress { 
        folder_index: usize, 
        files_done: usize, 
        files_total: usize,
        current_file: String,
        files_per_second: f64,
    },
    FolderCompleted { folder_index: usize },
    FolderError { folder_index: usize, error: String },
    AllComplete,
    Cancelled,
}

#[derive(Clone, Copy)]
enum CMap {
    Viridis,
    Turbo,
    Magma,
    Gray,
}

impl CMap {
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "viridis" => Ok(Self::Viridis),
            "turbo" => Ok(Self::Turbo),
            "magma" => Ok(Self::Magma),
            "gray" | "grey" | "grayscale" => Ok(Self::Gray),
            _ => Err(anyhow!("Unknown colormap: {s}")),
        }
    }

    fn eval(&self, v: f64) -> (u8, u8, u8) {
        let v = v.clamp(0.0, 1.0);
        match self {
            Self::Viridis => to_rgb(VIRIDIS.eval_continuous(v)),
            Self::Turbo => to_rgb(TURBO.eval_continuous(v)),
            Self::Magma => to_rgb(MAGMA.eval_continuous(v)),
            Self::Gray => {
                let g = (v * 255.0).round() as u8;
                (g, g, g)
            }
        }
    }
}

fn to_rgb(c: Color) -> (u8, u8, u8) {
    (c.r, c.g, c.b)
}

/// Process all folders in the queue
pub fn process_folders(
    folders: Vec<FolderInfo>,
    settings: ProcessingSettings,
    tx: Sender<ProgressUpdate>,
    stop_flag: Arc<AtomicBool>,
) {
    let cmap = match CMap::from_str(&settings.colormap) {
        Ok(c) => c,
        Err(_) => CMap::Viridis,
    };
    
    let jobs = if settings.jobs == 0 {
        ((num_cpus::get() as f64) * 0.9).ceil().max(1.0) as usize
    } else {
        settings.jobs
    };
    
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(jobs).build() {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(ProgressUpdate::FolderError {
                folder_index: 0,
                error: format!("Failed to create thread pool: {}", e),
            });
            return;
        }
    };
    
    for (folder_idx, folder) in folders.iter().enumerate() {
        // Check stop flag
        if stop_flag.load(Ordering::Relaxed) {
            let _ = tx.send(ProgressUpdate::Cancelled);
            return;
        }
        
        let _ = tx.send(ProgressUpdate::FolderStarted {
            folder_index: folder_idx,
            folder_name: folder.name.clone(),
        });
        
        // Get CSV files
        let csv_files = queue::get_csv_files(&folder.path);
        let files_total = csv_files.len();
        
        if files_total == 0 {
            let _ = tx.send(ProgressUpdate::FolderError {
                folder_index: folder_idx,
                error: "No CSV files found".to_string(),
            });
            continue;
        }
        
        // Determine output directory
        let output_dir = match &settings.output_dir {
            Some(custom_dir) if !custom_dir.as_os_str().is_empty() => {
                // Use custom output dir, create subfolder with input folder name
                custom_dir.join(&folder.name)
            }
            _ => {
                // Default: create sibling folder with _img_N suffix
                let folder_name = folder.path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("output");
                let output_folder_name = format!("{}_img_{}", folder_name, settings.pulses);
                folder.path.parent()
                    .map(|p| p.join(&output_folder_name))
                    .unwrap_or_else(|| folder.path.join("ppi_output"))
            }
        };
        
        if let Err(e) = fs::create_dir_all(&output_dir) {
            let _ = tx.send(ProgressUpdate::FolderError {
                folder_index: folder_idx,
                error: format!("Failed to create output directory: {}", e),
            });
            continue;
        }

        
        // Process files
        let files_done = AtomicUsize::new(0);
        let start_time = Instant::now();
        let last_update = Mutex::new(Instant::now());
        let tx_clone = tx.clone();
        let stop_flag_clone = stop_flag.clone();
        
        let results: Vec<Result<()>> = pool.install(|| {
            csv_files.par_iter().map(|csv_path| -> Result<()> {
                // Check stop flag periodically
                if stop_flag_clone.load(Ordering::Relaxed) {
                    return Ok(());
                }
                
                // Process single file
                let result = process_single_csv(
                    csv_path,
                    &output_dir,
                    settings.pulses,
                    settings.gap_deg.to_radians(),
                    settings.size,
                    cmap,
                );
                
                // Update progress
                let done = files_done.fetch_add(1, Ordering::Relaxed) + 1;
                
                // Only send updates every 100ms to avoid flooding
                let mut last = last_update.lock().unwrap();
                if last.elapsed().as_millis() >= 100 || done == files_total {
                    *last = Instant::now();
                    
                    let elapsed = start_time.elapsed().as_secs_f64();
                    let files_per_second = if elapsed > 0.0 { done as f64 / elapsed } else { 0.0 };
                    
                    let current_file = csv_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string();
                    
                    let _ = tx_clone.send(ProgressUpdate::FileProgress {
                        folder_index: folder_idx,
                        files_done: done,
                        files_total,
                        current_file,
                        files_per_second,
                    });
                }
                
                result
            }).collect()
        });
        
        // Check for errors
        let errors: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
        if !errors.is_empty() {
            let _ = tx.send(ProgressUpdate::FolderError {
                folder_index: folder_idx,
                error: format!("{} files failed to process", errors.len()),
            });
        } else {
            let _ = tx.send(ProgressUpdate::FolderCompleted { folder_index: folder_idx });
        }
    }
    
    let _ = tx.send(ProgressUpdate::AllComplete);
}

/// Process a single CSV file
fn process_single_csv(
    csv_path: &PathBuf,
    output_dir: &PathBuf,
    pulses: usize,
    gap_thresh: f64,
    size: u32,
    cmap: CMap,
) -> Result<()> {
    let (angles, bins, range_setting, gain, ts_str) = read_csv(csv_path)?;
    
    let output_name = format!("{}_{}_{}.png", ts_str, gain, range_setting);
    let output_path = output_dir.join(output_name);
    
    let (_theta_edges, bins_resampled) = regularize(&angles, &bins, pulses, gap_thresh);
    let png = render_png(&bins_resampled, range_setting, size, cmap)?;
    
    png.save(&output_path)
        .with_context(|| format!("saving {}", output_path.display()))?;
    
    Ok(())
}

/// Read CSV into angle radians and bin matrix.
fn read_csv(path: &PathBuf) -> Result<(Vec<f64>, Vec<Vec<f32>>, i32, i32, String)> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut lines = text.lines();
    let _header = lines
        .next()
        .ok_or_else(|| anyhow!("empty CSV: {}", path.display()))?;

    let mut raw_angles = Vec::new();
    let mut raw_bins: Vec<Vec<f32>> = Vec::new();
    let mut range_setting = 0i32;
    let mut gain_code = 0i32;

    for line in lines {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 6 {
            continue; // Skip malformed lines
        }
        let angle_ticks: f64 = parts[4].parse().unwrap_or(0.0);
        let row_bins: Vec<f32> = parts[5..]
            .iter()
            .map(|s| s.parse::<f32>().unwrap_or(0.0))
            .collect();
        raw_angles.push(angle_ticks * (2.0 * PI / 8192.0));
        raw_bins.push(row_bins);
        if range_setting == 0 {
            range_setting = parts[2].parse().unwrap_or(0);
        }
        if gain_code == 0 {
            gain_code = parts[3].parse().unwrap_or(0);
        }
    }
    
    if raw_angles.is_empty() {
        return Err(anyhow!("no data rows in {}", path.display()));
    }

    // Merge duplicate angles by averaging
    use std::collections::HashMap;
    let mut angle_map: HashMap<u64, (Vec<Vec<f32>>, f64)> = HashMap::new();
    
    for (angle, bin_row) in raw_angles.iter().zip(raw_bins.iter()) {
        let angle_key = (angle * 100000.0).round() as u64;
        angle_map.entry(angle_key)
            .or_insert_with(|| (Vec::new(), *angle))
            .0.push(bin_row.clone());
    }

    let mut angles = Vec::new();
    let mut bins = Vec::new();

    for (_key, (bin_rows, angle)) in angle_map.into_iter() {
        angles.push(angle);
        if bin_rows.len() == 1 {
            bins.push(bin_rows[0].clone());
        } else {
            let n_bins = bin_rows[0].len();
            let mut avg_bins = vec![0.0f32; n_bins];
            for bin_row in &bin_rows {
                for (i, &val) in bin_row.iter().enumerate() {
                    if i < avg_bins.len() {
                        avg_bins[i] += val;
                    }
                }
            }
            for val in &mut avg_bins {
                *val /= bin_rows.len() as f32;
            }
            bins.push(avg_bins);
        }
    }

    let ts_str = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    Ok((angles, bins, range_setting, gain_code, ts_str))
}

/// Regularize pulses onto fixed grid
fn regularize(
    angles: &[f64],
    bins: &[Vec<f32>],
    pulses: usize,
    gap_thresh: f64,
) -> (Vec<f64>, Vec<Vec<f32>>) {
    if bins.is_empty() {
        return (Vec::new(), Vec::new());
    }
    
    let n_bins = bins[0].len();
    let mut idx: Vec<usize> = (0..angles.len()).collect();
    idx.sort_by(|&a, &b| angles[a].partial_cmp(&angles[b]).unwrap());

    let mut bins_resampled = vec![vec![f32::NAN; n_bins]; pulses];

    for &i in &idx {
        let theta = angles[i];
        let pulse = ((theta / (2.0 * PI)) * pulses as f64).floor() as usize % pulses;
        bins_resampled[pulse] = bins[i].clone();
    }

    let step_rad = 2.0 * PI / pulses as f64;
    let mut has_data: Vec<bool> = bins_resampled
        .iter()
        .map(|row| row.iter().any(|v| v.is_finite()))
        .collect();

    let mut i = 0;
    while i < pulses {
        if has_data[i] {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < pulses && !has_data[i] {
            i += 1;
        }
        let run_end = i - 1;

        let mut prev = (run_start + pulses - 1) % pulses;
        while !has_data[prev] && prev != run_start {
            prev = (prev + pulses - 1) % pulses;
        }
        let mut next = run_end % pulses;
        next = (next + 1) % pulses;
        while !has_data[next] && next != run_start {
            next = (next + 1) % pulses;
        }

        if !has_data[prev] || !has_data[next] {
            continue;
        }

        let gap_steps = (next + pulses - prev) % pulses;
        if gap_steps == 0 {
            continue;
        }
        let missing_len = gap_steps - 1;
        let gap_angle = gap_steps as f64 * step_rad;

        if gap_angle <= gap_thresh {
            let prev_row = bins_resampled[prev].clone();
            let next_row = bins_resampled[next].clone();
            for k in 1..=missing_len {
                let t = k as f32 / (missing_len + 1) as f32;
                let idx_fill = (prev + k) % pulses;
                let mut filled = vec![0.0f32; n_bins];
                for b in 0..n_bins {
                    filled[b] = prev_row[b] * (1.0 - t) + next_row[b] * t;
                }
                bins_resampled[idx_fill] = filled;
                has_data[idx_fill] = true;
            }
        }
    }

    let theta_edges: Vec<f64> = (0..=pulses)
        .map(|p| p as f64 * 2.0 * PI / pulses as f64)
        .collect();
    (theta_edges, bins_resampled)
}

/// Render to RGBA PNG
fn render_png(
    bins: &[Vec<f32>],
    range_setting: i32,
    size: u32,
    cmap: CMap,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    if bins.is_empty() {
        return Ok(ImageBuffer::new(size, size));
    }
    
    let pulses = bins.len();
    let n_bins = bins[0].len();

    let mut max_val = 0.0f32;
    for row in bins {
        for &v in row {
            if v.is_finite() && v > max_val {
                max_val = v;
            }
        }
    }
    
    let mut img = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(size, size);
    
    if max_val <= 0.0 {
        for (_, _, pixel) in img.enumerate_pixels_mut() {
            *pixel = Rgba([0, 0, 0, 0]);
        }
        return Ok(img);
    }

    let cx = size as f64 / 2.0;
    let cy = size as f64 / 2.0;
    let radius = cx.min(cy);
    let range_max = if range_setting > 0 {
        range_setting as f64
    } else {
        n_bins as f64
    };

    for (x, y, pixel) in img.enumerate_pixels_mut() {
        let dx = x as f64 + 0.5 - cx;
        let dy = cy - (y as f64 + 0.5);
        let r_norm = (dx * dx + dy * dy).sqrt() / radius;
        if r_norm > 1.0 {
            *pixel = Rgba([0, 0, 0, 0]);
            continue;
        }
        let mut theta = dx.atan2(dy);
        if theta < 0.0 {
            theta += 2.0 * PI;
        }

        let pulse_idx = ((theta / (2.0 * PI)) * pulses as f64).floor() as usize % pulses;
        let r_val = r_norm * range_max;
        let bin_idx = (r_val / range_max * n_bins as f64).floor() as usize;
        if bin_idx >= n_bins {
            *pixel = Rgba([0, 0, 0, 0]);
            continue;
        }

        let row = &bins[pulse_idx];
        if bin_idx >= row.len() {
            *pixel = Rgba([0, 0, 0, 0]);
            continue;
        }
        let v = row[bin_idx];
        if !v.is_finite() || v == 0.0 {
            *pixel = Rgba([0, 0, 0, 0]);
            continue;
        }
        let norm = (v / max_val).clamp(0.0, 1.0) as f64;
        let (r, g, b) = cmap.eval(norm);
        *pixel = Rgba([r, g, b, 255]);
    }
    
    Ok(img)
}
