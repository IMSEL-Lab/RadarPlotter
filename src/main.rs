use anyhow::{anyhow, Context, Result};
use clap::Parser;
use colorous::{Color, MAGMA, TURBO, VIRIDIS};
use image::{ImageBuffer, Rgba};
use rayon::prelude::*;
use std::f64::consts::PI;
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "ppi_rs", about = "Render Furuno PPI CSV to transparent PNG")]
struct Args {
    /// Input CSV files (one or many)
    #[arg(required = true)]
    csv: Vec<PathBuf>,
    /// Output path: for one input, defaults to <range>_<gain>_<ts>.png; for many, provide a directory
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Pulses per revolution to regularize onto
    #[arg(short = 'p', long, default_value_t = 720)]
    pulses: usize,
    /// Gap threshold in degrees; gaps larger than this stay transparent
    #[arg(long = "gap-deg", default_value_t = 1.0)]
    gap_deg: f64,
    /// Image size (square) in pixels
    #[arg(long = "size", default_value_t = 1024)]
    size: u32,
    /// Colormap: viridis | turbo | magma | gray
    #[arg(long = "cmap", default_value = "viridis")]
    cmap: String,
    /// Parallel jobs (0 = auto ~90% of cores)
    #[arg(short = 'j', long, default_value_t = 0)]
    jobs: usize,
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

fn main() -> Result<()> {
    let args = Args::parse();
    let cmap = CMap::from_str(&args.cmap)?;
    let jobs = if args.jobs == 0 {
        ((num_cpus::get() as f64) * 0.9).ceil().max(1.0) as usize
    } else {
        args.jobs
    };

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build()?;

    let out_base = args.output.clone();
    let pulses = args.pulses;
    let gap = args.gap_deg.to_radians();

    let results: Vec<_> = pool.install(|| {
        args.csv
            .par_iter()
            .map(|csv_path| -> Result<PathBuf> {
                let (angles, bins, range_setting, gain, ts_str) = read_csv(csv_path)?;
                let default_name = format!("{}_{}_{}.png", range_setting, gain, ts_str);
                let out_path = match &out_base {
                    Some(p) if args.csv.len() == 1 => p.clone(),
                    Some(p) => {
                        let dir = if p.is_dir() { p.clone() } else { p.parent().unwrap_or(p).to_path_buf() };
                        dir.join(&default_name)
                    }
                    None => csv_path.with_file_name(default_name),
                };

                let (_theta_edges, bins_resampled) =
                    regularize(&angles, &bins, pulses, gap);
                let png = render_png(&bins_resampled, range_setting, args.size, cmap)?;
                png.save(&out_path)
                    .with_context(|| format!("saving {}", out_path.display()))?;
                println!("Saved {}", out_path.display());
                Ok(out_path)
            })
            .collect()
    });

    // surface first error if any
    for r in results {
        r?;
    }
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

    let mut angles = Vec::new();
    let mut bins: Vec<Vec<f32>> = Vec::new();
    let mut range_setting = 0i32;
    let mut gain_code = 0i32;

    for line in lines {
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 6 {
            return Err(anyhow!(
                "line has too few columns ({}): {}",
                parts.len(),
                line
            ));
        }
        let angle_ticks: f64 = parts[4].parse()?;
        let row_bins: Vec<f32> = parts[5..]
            .iter()
            .map(|s| s.parse::<f32>().unwrap_or(0.0))
            .collect();
        angles.push(angle_ticks * (2.0 * PI / 8192.0));
        bins.push(row_bins);
        // Range is column 2
        if range_setting == 0 {
            range_setting = parts[2].parse().unwrap_or(0);
        }
        // Gain is column 3
        if gain_code == 0 {
            gain_code = parts[3].parse().unwrap_or(0);
        }
    }
    if angles.is_empty() {
        return Err(anyhow!("no data rows in {}", path.display()));
    }
    // timestamp from filename stem
    let ts_str = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    Ok((angles, bins, range_setting, gain_code, ts_str))
}

/// Regularize pulses onto fixed grid, blanking gaps larger than gap_thresh radians.
fn regularize(
    angles: &[f64],
    bins: &[Vec<f32>],
    pulses: usize,
    gap_thresh: f64,
) -> (Vec<f64>, Vec<Vec<f32>>) {
    let n_bins = bins[0].len();
    // sort by angle
    let mut idx: Vec<usize> = (0..angles.len()).collect();
    idx.sort_by(|&a, &b| angles[a].partial_cmp(&angles[b]).unwrap());

    let mut bins_resampled = vec![vec![f32::NAN; n_bins]; pulses];

    for &i in &idx {
        let theta = angles[i];
        let pulse = ((theta / (2.0 * PI)) * pulses as f64).floor() as usize % pulses;
        bins_resampled[pulse] = bins[i].clone();
    }

    // gap detection
    let mut angles_sorted: Vec<f64> = idx.iter().map(|&i| angles[i]).collect();
    angles_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut diffs = Vec::with_capacity(angles_sorted.len());
    for w in angles_sorted.windows(2) {
        diffs.push(w[1] - w[0]);
    }
    diffs.push((angles_sorted[0] + 2.0 * PI) - angles_sorted.last().unwrap());

    let mut gap_idx: Vec<usize> = Vec::new();
    for (i, &d) in diffs.iter().enumerate() {
        if d > gap_thresh {
            gap_idx.push(i);
        }
    }

    if !gap_idx.is_empty() {
        let centers: Vec<f64> = (0..pulses)
            .map(|p| p as f64 * 2.0 * PI / pulses as f64)
            .collect();
        for &g in &gap_idx {
            let start = angles_sorted[g];
            let mut end = angles_sorted[(g + 1) % angles_sorted.len()];
            if end < start {
                end += 2.0 * PI;
            }
            for (pi, &c) in centers.iter().enumerate() {
                let mut cc = c;
                if cc < start {
                    cc += 2.0 * PI;
                }
                if cc >= start && cc < end {
                    bins_resampled[pi] = vec![f32::NAN; n_bins];
                }
            }
        }
    }

    let theta_edges: Vec<f64> = (0..=pulses)
        .map(|p| p as f64 * 2.0 * PI / pulses as f64)
        .collect();
    (theta_edges, bins_resampled)
}

/// Render to RGBA PNG with transparent background and zero-values transparent.
fn render_png(
    bins: &[Vec<f32>],
    range_setting: i32,
    size: u32,
    cmap: CMap,
) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let pulses = bins.len();
    let n_bins = bins[0].len();

    // find max value (non-zero, finite)
    let mut max_val = 0.0f32;
    for row in bins {
        for &v in row {
            if v.is_finite() && v > max_val {
                max_val = v;
            }
        }
    }
    let mut img = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(size, size);
    // If no signal, return fully transparent image
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
        let dy = cy - (y as f64 + 0.5); // y up
        let r_norm = (dx * dx + dy * dy).sqrt() / radius;
        if r_norm > 1.0 {
            *pixel = Rgba([0, 0, 0, 0]);
            continue;
        }
        let mut theta = dx.atan2(dy); // 0 at north, clockwise
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

        let v = bins[pulse_idx][bin_idx];
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
