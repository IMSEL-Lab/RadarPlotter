# CSV to PPI (Rust)

Render Furuno Plan Position Indicator (PPI) radar CSV captures to transparent PNGs.  Each CSV encodes one antenna sweep; one or many CSVs can be converted in a single run.

## CSV layout
```
Status,Scale,Range,Gain,Angle,EchoValues
1,496,3,60,0,12,34,56, ...
```
- **Status**: usually `1`; ignored for rendering.
- **Scale**: raw scale factor from the recorder; currently unused.
- **Range**: range setting for the sweep (integer, same for every row in the file).
- **Gain**: gain code for the sweep (integer, same for every row in the file).
- **Angle**: encoder ticks in `[0, 8191]`; converted to radians internally (0 at north, clockwise).
- **EchoValues**: the radar return for each range bin, one comma-separated integer per bin (0–255). All rows in a file must have the same number of bins.

## Quick start
```bash
# Build (release is much faster for batches)
cargo build --release

# Single CSV → PNG (writes beside the CSV)
cargo run --release -- run_csvs/csvs/data_pattern14/20250915_134512_925.csv
# produces: 3_60_20250915_134512_925.png

# Batch convert into a directory
cargo run --release -- \
  -o run_csvs/output_images \
  run_csvs/csvs/data_pattern14/*.csv
```

## Useful flags
- `-p, --pulses 720` — pulses per revolution used to regularize the sweep.
- `--gap-deg 1.0` — leave transparent gaps for missing angles wider than this (degrees).
- `--size 1024` — square PNG size in pixels.
- `--cmap viridis|turbo|magma|gray` — colormap for intensities.
- `-j, --jobs 0` — threads (0 = 90% of available cores).
- `-o, --output` — output path; with multiple inputs, give a directory.

## Output
- Transparent PNG with north at the top and angles increasing clockwise.
- Pixel values are scaled per-image by the maximum non‑zero echo value.
- Default filename: `<Range>_<Gain>_<timestamp>.png` where `timestamp` comes from the CSV filename stem.

## Example data
Sample captures live in `run_csvs/csvs/`. Running the quick-start command above yields an image like the provided `run_csvs/output/2_40_20250915_142926_694.png` (colors and transparency depend on the chosen colormap).
