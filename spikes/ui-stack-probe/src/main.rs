//! egui/eframe 路线探针：只用于回答三个可实测的问题。
//!
//! 1. 在本机**离线**环境下，eframe + egui + rfd 这套依赖能不能真的构建出来？
//! 2. 产物体积与运行时依赖是什么规模（对比原生 Win32 的 654 KB 单 exe）？
//! 3. **中文与 emoji 开箱能不能正常显示**？
//!    （egui 默认只自带拉丁字体，而 CanLapse 的界面全中文 + 大量图标）
//!
//! 界面照抄 CanLapse 规格里的关键文案，若显示异常一眼就能看出来。

#[derive(Default)]
struct ProbeApp {
    frame_density: f32,
    threshold: f32,
    clear_cache: bool,
    help_open: bool,
}

impl eframe::App for ProbeApp {
    // 注意：egui 0.36 的 eframe::App 已经**没有** `update(&mut self, ctx, frame)` 了，
    // 取而代之的是 `ui(&mut self, ui: &mut Ui, frame)`；面板也改成接收 `&mut Ui`。
    // CanLapse/add/egui.rs 是按旧 API 写的，直接搬过来编译不过 —— 这正是探针要暴露的事。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("缩时录屏工具");
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.button("🎯 拾取窗口");
                ui.button("🔍 预览窗口");
                if ui.button("❓ 如何定位窗口").clicked() {
                    self.help_open = true;
                }
            });
            ui.add_space(6.0);

            ui.label("预览区域（160×100）");
            ui.colored_label(egui::Color32::GREEN, "✅ 已选中画布窗口");
            ui.colored_label(
                egui::Color32::RED,
                "⚠️ 检测到窗口包含工具栏，画布未浮动",
            );
            ui.add_space(8.0);

            egui::CollapsingHeader::new("录制参数")
                .default_open(true)
                .show(ui, |ui| {
                    ui.label(format!("帧密度：{} fps", self.frame_density));
                    ui.add(egui::Slider::new(&mut self.frame_density, 1.0..=8.0));
                    ui.label(format!("像素差异阈值：{:.3}", self.threshold));
                    ui.add(egui::Slider::new(&mut self.threshold, 0.0..=0.05));
                });

            egui::CollapsingHeader::new("导出视频参数")
                .default_open(false)
                .show(ui, |ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(160, 160, 0),
                        "💡提示：如果取消勾选「合成成功自动清理缓存」，PNG帧图片会保留在缓存目录",
                    );
                    ui.checkbox(&mut self.clear_cache, "合成成功自动清理缓存");
                });

            egui::CollapsingHeader::new("历史缓存会话")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label("2026-10-17_07-57-45_e2e ｜ 24 帧");
                    ui.horizontal(|ui| {
                        ui.button("重新导出");
                        ui.button("打开文件夹");
                        ui.button("删除");
                    });
                });

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.button("开始录制");
                ui.label("⏸暂停 🏷标记 ⏹停止 ｜ 00:12 ｜ 37 帧");
            });

            if self.help_open {
                egui::Window::new("如何定位画布窗口")
                    .collapsible(false)
                    .show(&ctx, |ui| {
                        ui.label("SAI2：视图 → 新建浮动视图");
                        ui.label("Photoshop：拖拽画布标签向外拉出，脱离主框架");
                        if ui.button("关闭").clicked() {
                            self.help_open = false;
                        }
                    });
            }
        });
    }
}

/// 加载系统 CJK 字体。egui 自带字体只有拉丁字形，不挂这个的话整个中文界面都是豆腐块。
fn install_cjk_font(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    for path in candidates {
        match std::fs::read(path) {
            Ok(bytes) => {
                println!("已加载中文字体: {path} ({} 字节)", bytes.len());
                let name = "cjk".to_owned();
                fonts
                    .font_data
                    .insert(name.clone(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, name.clone());
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .push(name);
                ctx.set_fonts(fonts);
                return;
            }
            Err(e) => println!("跳过 {path}: {e}"),
        }
    }
    println!("没找到任何中文字体，界面会显示豆腐块");
}

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([460.0, 380.0])
            .with_min_inner_size([400.0, 320.0])
            .with_title("CanLapse UI 探针"),
        ..Default::default()
    };
    eframe::run_native(
        "CanLapse UI 探针",
        native_options,
        Box::new(|cc| {
            install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(ProbeApp::default()))
        }),
    )
}
