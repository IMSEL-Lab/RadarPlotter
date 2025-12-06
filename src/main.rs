//! CSV to PPI GUI - Main Entry Point
//! 
//! A graphical interface for batch converting Furuno CSV radar data to PPI images.

slint::include_modules!();

mod processing;
mod queue;
mod config;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use slint::{ModelRc, SharedString, VecModel};

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    
    // Shared state
    let folders: Rc<RefCell<Vec<queue::FolderInfo>>> = Rc::new(RefCell::new(Vec::new()));
    let processing_handle: Rc<RefCell<Option<thread::JoinHandle<()>>>> = Rc::new(RefCell::new(None));
    let stop_flag: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    // Keep timer alive by storing it in shared state
    let progress_timer: Rc<RefCell<Option<slint::Timer>>> = Rc::new(RefCell::new(None));
    
    // Load saved settings
    if let Ok(settings) = config::load_settings() {
        ui.set_pulses(settings.pulses);
        ui.set_gap_deg(settings.gap_deg as f32);
        ui.set_image_size(settings.image_size);
        ui.set_colormap(settings.colormap.into());
        ui.set_jobs(settings.jobs);
        ui.set_output_dir(settings.output_dir.into());
    }

    
    // Add folder callback
    {
        let ui_weak = ui.as_weak();
        let folders = folders.clone();
        ui.on_add_folder(move || {
            let ui = ui_weak.unwrap();
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Select folder containing CSV files")
                .pick_folder()
            {
                let csv_count = queue::count_csv_files(&path);
                let folder_name = path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Unknown")
                    .to_string();
                
                let folder_info = queue::FolderInfo {
                    path: path.clone(),
                    name: folder_name.clone(),
                    file_count: csv_count,
                    status: queue::FolderStatus::Pending,
                    progress: 0.0,
                    error_message: None,
                };
                
                folders.borrow_mut().push(folder_info);
                update_folder_model(&ui, &folders.borrow());
            }
        });
    }
    
    // Remove folder callback
    {
        let ui_weak = ui.as_weak();
        let folders = folders.clone();
        ui.on_remove_folder(move |index| {
            let ui = ui_weak.unwrap();
            let mut folders_mut = folders.borrow_mut();
            if (index as usize) < folders_mut.len() {
                folders_mut.remove(index as usize);
                drop(folders_mut);
                update_folder_model(&ui, &folders.borrow());
            }
        });
    }
    
    // Move folder up callback
    {
        let ui_weak = ui.as_weak();
        let folders = folders.clone();
        ui.on_move_folder_up(move |index| {
            let ui = ui_weak.unwrap();
            let mut folders_mut = folders.borrow_mut();
            if index > 0 && (index as usize) < folders_mut.len() {
                folders_mut.swap(index as usize, (index - 1) as usize);
                drop(folders_mut);
                update_folder_model(&ui, &folders.borrow());
            }
        });
    }
    
    // Move folder down callback
    {
        let ui_weak = ui.as_weak();
        let folders = folders.clone();
        ui.on_move_folder_down(move |index| {
            let ui = ui_weak.unwrap();
            let mut folders_mut = folders.borrow_mut();
            if ((index + 1) as usize) < folders_mut.len() {
                folders_mut.swap(index as usize, (index + 1) as usize);
                drop(folders_mut);
                update_folder_model(&ui, &folders.borrow());
            }
        });
    }
    
    // Clear queue callback
    {
        let ui_weak = ui.as_weak();
        let folders = folders.clone();
        ui.on_clear_queue(move || {
            let ui = ui_weak.unwrap();
            folders.borrow_mut().clear();
            update_folder_model(&ui, &folders.borrow());
        });
    }
    
    // Settings changed callback
    {
        ui.on_settings_changed(move |pulses, gap_deg, image_size, colormap, jobs, output_dir| {
            let settings = config::Settings {
                pulses,
                gap_deg: gap_deg as f64,
                image_size,
                colormap: colormap.to_string(),
                jobs,
                output_dir: output_dir.to_string(),
            };
            let _ = config::save_settings(&settings);
        });
    }

    
    // Start processing callback
    {
        let ui_weak = ui.as_weak();
        let folders = folders.clone();
        let processing_handle = processing_handle.clone();
        let stop_flag = stop_flag.clone();
        let progress_timer = progress_timer.clone();
        
        ui.on_start_processing(move || {
            let ui = ui_weak.unwrap();
            
            // Don't start if already processing
            if ui.get_is_processing() {
                return;
            }
            
            // Reset stop flag
            stop_flag.store(false, Ordering::Relaxed);
            
            // Get settings
            let output_dir_str = ui.get_output_dir().to_string();
            let settings = processing::ProcessingSettings {
                pulses: ui.get_pulses() as usize,
                gap_deg: ui.get_gap_deg() as f64,
                size: ui.get_image_size() as u32,
                colormap: ui.get_colormap().to_string(),
                jobs: ui.get_jobs() as usize,
                output_dir: if output_dir_str.is_empty() { 
                    None 
                } else { 
                    Some(std::path::PathBuf::from(output_dir_str)) 
                },
            };

            
            // Get folder list
            let folder_list: Vec<queue::FolderInfo> = folders.borrow().clone();
            if folder_list.is_empty() {
                return;
            }
            
            // Create progress channel
            let (tx, rx) = mpsc::channel::<processing::ProgressUpdate>();
            
            // Update UI state
            ui.set_is_processing(true);
            ui.set_is_complete(false);
            ui.set_status_text("Starting...".into());
            ui.set_folders_completed(0);
            ui.set_files_completed(0);
            ui.set_files_total(0);
            ui.set_overall_progress(0.0);
            
            // Reset progress for all folders
            {
                let mut folders_mut = folders.borrow_mut();
                for folder in folders_mut.iter_mut() {
                    folder.status = queue::FolderStatus::Pending;
                    folder.progress = 0.0;
                }
            }
            update_folder_model(&ui, &folders.borrow());
            
            // Spawn processing thread
            let stop_flag_clone = stop_flag.clone();
            let handle = thread::spawn(move || {
                processing::process_folders(folder_list, settings, tx, stop_flag_clone);
            });
            
            *processing_handle.borrow_mut() = Some(handle);
            
            // Set up progress polling
            let ui_weak_poll = ui.as_weak();
            let folders_poll = folders.clone();
            let processing_handle_poll = processing_handle.clone();
            
            let timer = slint::Timer::default();
            timer.start(
                slint::TimerMode::Repeated,
                std::time::Duration::from_millis(50),
                move || {
                    let ui = match ui_weak_poll.upgrade() {
                        Some(ui) => ui,
                        None => return,
                    };
                    
                    // Process all pending updates
                    while let Ok(update) = rx.try_recv() {
                        match update {
                            processing::ProgressUpdate::FolderStarted { folder_index, folder_name } => {
                                ui.set_current_folder(folder_name.into());
                                ui.set_status_text(SharedString::from(format!("Processing folder {}", folder_index + 1)));
                                
                                let mut folders_mut = folders_poll.borrow_mut();
                                if folder_index < folders_mut.len() {
                                    folders_mut[folder_index].status = queue::FolderStatus::Processing;
                                }
                                drop(folders_mut);
                                update_folder_model(&ui, &folders_poll.borrow());
                            }
                            processing::ProgressUpdate::FileProgress { 
                                folder_index, 
                                files_done, 
                                files_total, 
                                current_file,
                                files_per_second,
                            } => {
                                let folder_progress = files_done as f32 / files_total.max(1) as f32;
                                ui.set_folder_progress(folder_progress);
                                ui.set_files_completed(files_done as i32);
                                ui.set_files_total(files_total as i32);
                                ui.set_current_file(current_file.into());
                                ui.set_files_per_second(files_per_second as f32);
                                
                                // Update folder progress
                                let mut folders_mut = folders_poll.borrow_mut();
                                if folder_index < folders_mut.len() {
                                    folders_mut[folder_index].progress = folder_progress;
                                }
                                drop(folders_mut);
                                update_folder_model(&ui, &folders_poll.borrow());
                                
                                // Calculate ETA
                                if files_per_second > 0.0 {
                                    let remaining = files_total - files_done;
                                    let eta_secs = (remaining as f64 / files_per_second) as u64;
                                    let eta_mins = eta_secs / 60;
                                    let eta_secs_rem = eta_secs % 60;
                                    ui.set_eta_text(SharedString::from(format!("{:02}:{:02}", eta_mins, eta_secs_rem)));
                                }
                            }
                            processing::ProgressUpdate::FolderCompleted { folder_index } => {
                                let mut folders_mut = folders_poll.borrow_mut();
                                if folder_index < folders_mut.len() {
                                    folders_mut[folder_index].status = queue::FolderStatus::Complete;
                                    folders_mut[folder_index].progress = 1.0;
                                }
                                ui.set_folders_completed(ui.get_folders_completed() + 1);
                                
                                // Update overall progress
                                let total_folders = folders_mut.len() as f32;
                                let completed = folders_mut.iter()
                                    .filter(|f| matches!(f.status, queue::FolderStatus::Complete))
                                    .count() as f32;
                                ui.set_overall_progress(completed / total_folders);
                                
                                drop(folders_mut);
                                update_folder_model(&ui, &folders_poll.borrow());
                            }
                            processing::ProgressUpdate::FolderError { folder_index, error } => {
                                let mut folders_mut = folders_poll.borrow_mut();
                                if folder_index < folders_mut.len() {
                                    folders_mut[folder_index].status = queue::FolderStatus::Error;
                                    folders_mut[folder_index].error_message = Some(error);
                                }
                                drop(folders_mut);
                                update_folder_model(&ui, &folders_poll.borrow());
                            }
                            processing::ProgressUpdate::AllComplete => {
                                ui.set_is_processing(false);
                                ui.set_is_complete(true);
                                ui.set_overall_progress(1.0);
                                ui.set_status_text("Processing complete!".into());
                                ui.set_eta_text("--:--".into());
                                
                                // Clean up handle
                                if let Some(handle) = processing_handle_poll.borrow_mut().take() {
                                    let _ = handle.join();
                                }
                            }
                            processing::ProgressUpdate::Cancelled => {
                                ui.set_is_processing(false);
                                ui.set_status_text("Cancelled".into());
                                
                                // Clean up handle
                                if let Some(handle) = processing_handle_poll.borrow_mut().take() {
                                    let _ = handle.join();
                                }
                            }
                        }
                    }
                },
            );
            
            // Store timer to keep it alive
            *progress_timer.borrow_mut() = Some(timer);
        });
    }
    
    // Stop processing callback
    {
        let stop_flag = stop_flag.clone();
        ui.on_stop_processing(move || {
            stop_flag.store(true, Ordering::Relaxed);
        });
    }
    
    // Browse output directory callback
    {
        let ui_weak = ui.as_weak();
        ui.on_browse_output_dir(move || {
            let ui = ui_weak.unwrap();
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Select output directory")
                .pick_folder()
            {
                ui.set_output_dir(path.to_string_lossy().to_string().into());
            }
        });
    }
    
    // Show help callback (placeholder - could open a dialog or window)
    {
        ui.on_show_help(move || {
            // For now, this is a no-op. Could show a help dialog in the future.
            println!("Help requested - settings explanations are shown inline.");
        });
    }
    
    ui.run()

}

/// Update the folder model in the UI from the internal state
fn update_folder_model(ui: &AppWindow, folders: &[queue::FolderInfo]) {
    let items: Vec<FolderItem> = folders.iter().map(|f| {
        FolderItem {
            path: f.path.to_string_lossy().to_string().into(),
            name: f.name.clone().into(),
            file_count: f.file_count as i32,
            status: match f.status {
                queue::FolderStatus::Pending => "pending".into(),
                queue::FolderStatus::Processing => "processing".into(),
                queue::FolderStatus::Complete => "complete".into(),
                queue::FolderStatus::Error => "error".into(),
            },
            progress: f.progress,
            error_message: f.error_message.clone().unwrap_or_default().into(),
        }
    }).collect();
    
    let model = Rc::new(VecModel::from(items));
    ui.set_folders(ModelRc::from(model));
}
