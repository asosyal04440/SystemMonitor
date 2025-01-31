use eframe::egui;
use egui::{Color32, ColorImage, RichText, ScrollArea};
use egui_plot::{Line, Plot};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::time::{Duration, Instant};
use sysinfo::{CpuExt, DiskExt, NetworkExt, NetworksExt, Pid, ProcessExt, System, SystemExt};
use winapi::shared::windef::HICON;
use winapi::um::shellapi::ExtractIconExW;
use winapi::um::wingdi::{
    CreateCompatibleDC, GetDIBits, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
};
use winapi::um::wingdi::{GetObjectW, BITMAP};
use winapi::um::winuser::GetDC;
use winapi::um::winuser::GetIconInfo;
use std::collections::HashMap;
use std::iter::Map;
use std::sync::{Arc, Mutex};
use std::thread;

const HISTORY_LEN: usize = 60;
const COLORS: [Color32; 4] = [
    Color32::from_rgb(255, 99, 132),
    Color32::from_rgb(54, 162, 235),
    Color32::from_rgb(255, 206, 86),
    Color32::from_rgb(75, 192, 192),
];

struct IconCache {
    cache: HashMap<String, Option<egui::TextureHandle>>,
}

impl IconCache {
    fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }

    fn get_or_insert(
        &mut self,
        process_path: &str,
        ctx: &egui::Context,
    ) -> Option<egui::TextureHandle> {
        if let Some(icon) = self.cache.get(process_path) {
            return icon.clone();
        }

        let icon = get_icon_image(process_path, ctx);
        self.cache.insert(process_path.to_string(), icon.clone());
        icon
    }
}

fn get_icon_image(process_path: &str, ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let path: Vec<u16> = PathBuf::from(process_path)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut small_icon: HICON = std::ptr::null_mut();
    let mut large_icon: HICON = std::ptr::null_mut();

    unsafe {
        let icons_extracted = ExtractIconExW(path.as_ptr(), 0, &mut large_icon, &mut small_icon, 1);
        if icons_extracted > 0 && !small_icon.is_null() {
            if let Some(image) = convert_hicon_to_image(small_icon) {
                return Some(ctx.load_texture(
                    "process_icon",
                    image,
                    egui::TextureOptions::default(),
                ));
            }
        }
    }
    None
}

fn convert_hicon_to_image(hicon: HICON) -> Option<ColorImage> {
    unsafe {
        let mut icon_info = std::mem::zeroed();
        if GetIconInfo(hicon, &mut icon_info) == 0 {
            return None;
        }

        let hdc = GetDC(null_mut());
        let hdc_mem = CreateCompatibleDC(hdc);
        SelectObject(hdc_mem, icon_info.hbmColor as *mut c_void);

        let mut bitmap: BITMAP = std::mem::zeroed();
        GetObjectW(
            icon_info.hbmColor as *mut _,
            std::mem::size_of::<BITMAP>() as i32,
            &mut bitmap as *mut _ as *mut _,
        );

        let width = bitmap.bmWidth as usize;
        let height = bitmap.bmHeight as usize;
        let mut pixels = vec![0u8; width * height * 4];

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: bitmap.bmWidth,
                biHeight: -bitmap.bmHeight,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [std::mem::zeroed(); 1],
        };

        if GetDIBits(
            hdc_mem,
            icon_info.hbmColor,
            0,
            bitmap.bmHeight as u32,
            pixels.as_mut_ptr() as *mut _,
            &mut bmi as *mut _ as *mut _,
            BI_RGB,
        ) == 0
        {
            return None;
        }

        Some(egui::ColorImage::from_rgba_unmultiplied(
            [width, height],
            &pixels,
        ))
    }
}


struct SystemMonitor {
    system: Arc<Mutex<System>>,
    dark_mode: bool,
    cpu_history: Vec<VecDeque<f32>>,
    memory_history: VecDeque<f32>,
    network_history: VecDeque<(f64, f64)>,
    last_update: Instant,
    update_interval: Duration,
    icon_cache: IconCache,
    last_process_update: Instant,
    process_update_interval: Duration,
    cached_process_list: Vec<(Pid, String, f64)>,
    cached_disk_info: Vec<(String, f64, f64, f64)>,}

impl SystemMonitor {
    fn new() -> Self {
        let system = Arc::new(Mutex::new(System::new_all()));
        {
            let mut sys = system.lock().unwrap();
            sys.refresh_all();
        }

        let cpu_count = system.lock().unwrap().cpus().len();
        let mut cpu_history = Vec::with_capacity(cpu_count);
        for _ in 0..cpu_count {
            let mut deque = VecDeque::new();
            deque.resize(HISTORY_LEN, 0.0);
            cpu_history.push(deque);
        }


        let system_clone = Arc::clone(&system);
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_millis(1000));
                let mut sys = system_clone.lock().unwrap();
                sys.refresh_cpu();
                sys.refresh_memory();
                sys.refresh_networks();
            }
        });

        Self {
            system,
            dark_mode: true,
            cpu_history,
            memory_history: VecDeque::from(vec![0.0; HISTORY_LEN]),
            network_history: VecDeque::from(vec![(0.0, 0.0); HISTORY_LEN]),
            last_update: Instant::now(),
            update_interval: Duration::from_millis(1000),
            icon_cache: IconCache::new(),
            last_process_update: Instant::now(),
            process_update_interval: Duration::from_secs(5),
            cached_process_list: Vec::new(),
            cached_disk_info: Vec::new(),
        }
    }
    fn get_process_list(sys: &System) -> Vec<(Pid, String, f64)> {
        let mut processes: Vec<(Pid, String, f64)> = sys
            .processes()
            .iter()
            .map(|(pid, process)| {
                let total_io = (process.disk_usage().total_written_bytes
                    + process.disk_usage().total_read_bytes) as f64
                    / 1_048_576.0;
                (*pid, process.name().to_string(), total_io)
            })
            .collect();

        processes.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        processes.into_iter().take(5).collect()
    }

    fn update(&mut self) {
        if self.last_update.elapsed() >= self.update_interval {
            let mut sys = self.system.lock().unwrap();

            // Update CPU and memory data
            for (i, cpu) in sys.cpus().iter().enumerate() {
                self.cpu_history[i].push_back(cpu.cpu_usage());
                if self.cpu_history[i].len() > HISTORY_LEN {
                    self.cpu_history[i].pop_front();
                }
            }

            let memory_usage = (sys.used_memory() as f64 / sys.total_memory() as f64 * 100.0) as f32;
            self.memory_history.push_back(memory_usage);
            if self.memory_history.len() > HISTORY_LEN {
                self.memory_history.pop_front();
            }

            // Update network data
            let mut total_rx = 0.0;
            let mut total_tx = 0.0;
            for (_, data) in sys.networks() {
                total_rx += data.received() as f64;
                total_tx += data.transmitted() as f64;
            }
            self.network_history
                .push_back((total_rx / 1024.0, total_tx / 1024.0));
            if self.network_history.len() > HISTORY_LEN {
                self.network_history.pop_front();
            }


            sys.refresh_disks();
            self.cached_disk_info = sys
                .disks()
                .iter()
                .map(|disk| {
                    let total = disk.total_space() as f64 / 1_073_741_824.0;
                    let available = disk.available_space() as f64 / 1_073_741_824.0;
                    let used = total - available;
                    let ratio = used / total;
                    (disk.name().to_string_lossy().to_string(), used, total, ratio)
                })
                .collect();

            self.last_update = Instant::now();
        }


        if self.last_process_update.elapsed() >= self.process_update_interval {
            let sys = self.system.lock().unwrap();
            self.cached_process_list = SystemMonitor::get_process_list(&sys);
            self.last_process_update = Instant::now();
        }
    }
    fn update_process_list(&mut self, sys: &System) {
        let mut processes: Vec<(Pid, String, f64)> = sys
            .processes()
            .iter()
            .map(|(pid, process)| {
                let total_io = (process.disk_usage().total_written_bytes
                    + process.disk_usage().total_read_bytes) as f64
                    / 1_048_576.0;
                (*pid, process.name().to_string(), total_io)
            })
            .collect();

        processes.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        self.cached_process_list = processes.into_iter().take(5).collect();
    }

    fn system_info(&self) -> String {
        let sys = self.system.lock().unwrap();
        format!(
            "OS: {} {}\nHostname: {}\nKernel: {}\nUptime: {} mins",
            sys.name().unwrap_or("Unknown".into()),
            sys.os_version().unwrap_or("Unknown".into()),
            sys.host_name().unwrap_or("Unknown".into()),
            sys.kernel_version().unwrap_or("Unknown".into()),
            sys.uptime() / 60
        )
    }
}


impl eframe::App for SystemMonitor {
fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    self.update();


    let mut style = (*ctx.style()).clone();
    style.visuals = if self.dark_mode {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    ctx.set_style(style);

    egui::TopBottomPanel::top("header").show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.heading(
                RichText::new("🖥️ System Monitor").color(Color32::from_rgb(94, 200, 217)),
            );
            ui.separator();
            ui.checkbox(&mut self.dark_mode, "🌓 Dark Mode");
            if ui.button("🔄 Refresh").clicked() {
                self.system.lock().unwrap().refresh_all();
            }
        });
    });

    egui::CentralPanel::default().show(ctx, |ui| {
        ScrollArea::vertical().show(ui, |ui| {
            ui.vertical(|ui| {
                ui.collapsing(RichText::new("📋 System Information").heading(), |ui| {
                    ui.monospace(self.system_info());
                });
                ui.collapsing(RichText::new("🚦 CPU Usage").heading(), |ui| {
                    let sys = self.system.lock().unwrap();
                    egui::Grid::new("cpu_grid")
                        .num_columns(2)
                        .spacing([40.0, 4.0])
                        .show(ui, |ui| {
                            for (i, cpu) in sys.cpus().iter().enumerate() {
                                let usage = cpu.cpu_usage();
                                ui.label(format!("Core {}:", i));
                                ui.add(
                                    egui::ProgressBar::new(usage as f32 / 100.0)
                                        .text(format!("{:.1}%", usage))
                                        .fill(COLORS[i % COLORS.len()]),
                                );
                                ui.end_row();
                            }
                        });

                        let plot = Plot::new("cpu_plot")
                            .height(150.0)
                            .show_x(false)
                            .show_axes([false, true]);
                        plot.show(ui, |plot_ui| {
                            for (i, history) in self.cpu_history.iter().enumerate() {
                                let values: Vec<_> = history
                                    .iter()
                                    .enumerate()
                                    .map(|(x, y)| [x as f64, *y as f64])
                                    .collect();
                                plot_ui.line(
                                    Line::new(values)
                                        .color(COLORS[i % COLORS.len()])
                                        .name(format!("Core {}", i)),
                                );
                            }
                        });
                    });

                    ui.collapsing(RichText::new("🧠 Memory Usage").heading(), |ui| {
                        let sys = self.system.lock().unwrap();
                        let used_mem = sys.used_memory() as f64 / 1_073_741_824.0;
                        let total_mem = sys.total_memory() as f64 / 1_073_741_824.0;
                        let free_mem = total_mem - used_mem;

                        ui.monospace(format!(
                            "Used: {:.2} GB\nFree: {:.2} GB\nTotal: {:.2} GB ({:.1}%)",
                            used_mem,
                            free_mem,
                            total_mem,
                            (used_mem / total_mem) * 100.0
                        ));

                        ui.add(
                            egui::ProgressBar::new(used_mem as f32 / total_mem as f32)
                                .text("RAM Usage")
                                .fill(Color32::from_rgb(255, 99, 132))
                                .show_percentage(),
                        );

                        let plot = Plot::new("memory_plot")
                            .height(100.0)
                            .show_x(false)
                            .show_axes([false, true]);
                        plot.show(ui, |plot_ui| {
                            let values: Vec<_> = self
                                .memory_history
                                .iter()
                                .enumerate()
                                .map(|(x, y)| [x as f64, *y as f64])
                                .collect();
                            plot_ui.line(Line::new(values).color(Color32::from_rgb(255, 159, 64)));
                        });
                    });

                ui.collapsing(RichText::new("💾 Storage").heading(), |ui| {
                    egui::Grid::new("disks_grid")
                        .num_columns(3)
                        .spacing([20.0, 4.0])
                        .show(ui, |ui| {
                            for (name, used, total, ratio) in &self.cached_disk_info {
                                ui.label(name);
                                ui.add(
                                    egui::ProgressBar::new(*ratio as f32)
                                        .text(format!("{:.1} GB / {:.1} GB", used, total)),
                                );
                                ui.monospace(format!("{:.1}%", ratio * 100.0));
                                ui.end_row();
                            }
                        });

                    ui.separator();
                    ui.heading("🚨 Space Hogging Apps");

                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            for (pid, name, total_io) in &self.cached_process_list {
                                ui.horizontal(|ui| {
                                    if let Some(process) = self.system.lock().unwrap().process(*pid) {
                                        if let Some(exe_path) = process.exe().to_str() {
                                            if let Some(icon) = self.icon_cache.get_or_insert(exe_path, ctx) {
                                                ui.add(egui::Image::new(&icon).fit_to_exact_size(egui::vec2(16.0, 16.0)));
                                            }
                                        }
                                    }
                                    ui.label(format!("{} (PID: {})", name, pid));
                                    ui.monospace(format!("{:.2} MB I/O", total_io));
                                });
                            }
                        });
                });

                ui.collapsing(RichText::new("🌐 Network").heading(), |ui| {
                    let (current_rx, current_tx) = self.network_history.back().unwrap_or(&(0.0, 0.0));
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⬇️").color(Color32::GREEN));
                        ui.monospace(format!("{:.1} KB/s", current_rx));
                        ui.label(RichText::new("⬆️").color(Color32::RED));
                        ui.monospace(format!("{:.1} KB/s", current_tx));
                    });

                    let plot = Plot::new("network_plot")
                        .height(100.0)
                        .show_x(false)
                        .show_axes([false, true]);
                    plot.show(ui, |plot_ui| {
                        let rx_values: Vec<_> = self
                            .network_history
                            .iter()
                            .enumerate()
                            .map(|(x, (rx, _))| [x as f64, *rx])
                            .collect();
                        let tx_values: Vec<_> = self
                            .network_history
                            .iter()
                            .enumerate()
                            .map(|(x, (_, tx))| [x as f64, *tx])
                            .collect();

                        plot_ui.line(Line::new(rx_values).color(Color32::GREEN).name("Download"));
                        plot_ui.line(Line::new(tx_values).color(Color32::RED).name("Upload"));
                    });
                });
            });
        });
    });

    ctx.request_repaint_after(Duration::from_millis(500));
}
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([600.0, 800.0]),
        ..Default::default()
    };

    eframe::run_native(
        "System Monitor",
        options,
        Box::new(|_cc| Box::new(SystemMonitor::new())),
    )
}
