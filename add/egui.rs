fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.heading("缩时录屏工具");
        ui.add_space(4.0);

        // ========== 拾取模块 ==========
        ui.horizontal(|ui| {
            if ui.button("🎯 拾取窗口").clicked() {
                // 拾取窗口逻辑
            }
            if ui.button("🔍 预览窗口").clicked() {
                // 弹出大图预览
            }
            let help_btn = ui.button("❓ 如何定位窗口");
            if help_btn.clicked() {
                self.show_help_dialog = true;
            }
            help_btn.on_hover_text("了解拾取画布窗口、分离浮动画布");
        });
        ui.add_space(6.0);

        // 预览缩略图
        if let Some(tex) = &self.preview_tex {
            ui.image(tex, egui::vec2(160.0, 100.0));
        } else {
            ui.label("预览区域");
        }

        // 拾取状态提示
        match &self.pick_status {
            PickStatus::Empty => ui.label("尚未选择画布窗口"),
            PickStatus::Ok => ui.colored_label(egui::Color32::GREEN, "✅ 已选中画布窗口"),
            PickStatus::HasPanel => {
                ui.colored_label(egui::Color32::RED, "⚠️ 检测到窗口包含工具栏，画布未浮动");
                if ui.link("如何定位窗口").clicked() {
                    self.show_help_dialog = true;
                }
            }
        }
        ui.add_space(8.0);

        // ========== 录制参数（默认展开） ==========
        egui::CollapsingHeader::new("录制参数")
            .default_open(true)
            .show(ui, |ui| {
                ui.label(format!("帧密度：{} fps", self.cfg.max_save_fps));
                ui.add(egui::Slider::new(&mut self.cfg.max_save_fps, 1.0..=8.0));
                ui.label(format!("像素差异阈值：{:.3}", self.cfg.diff_threshold));
                ui.add(egui::Slider::new(&mut self.cfg.diff_threshold, 0.0..=0.05));
            });
        ui.add_space(4.0);

        // ========== 导出视频参数（默认折叠） ==========
        egui::CollapsingHeader::new("导出视频参数")
            .default_open(false)
            .show(ui, |ui| {
                ui.colored_label(egui::Color32::from_rgb(160,160,0),
                    "💡提示：如果取消勾选「合成成功自动清理缓存」，PNG帧图片会保留在缓存目录，占用磁盘空间，请记得定期手动删除缓存。");
                ui.add_space(6.0);

                ui.checkbox(&mut self.auto_clear_cache, "合成成功自动清理缓存");
                ui.add_space(4.0);

                ui.label("播放帧率");
                egui::ComboBox::from_label("")
                    .selected_text(format!("{} fps", self.export_fps))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.export_fps, 5, "5 fps");
                        ui.selectable_value(&mut self.export_fps, 10, "10 fps");
                        ui.selectable_value(&mut self.export_fps, 15, "15 fps");
                        ui.selectable_value(&mut self.export_fps, 20, "20 fps");
                        ui.selectable_value(&mut self.export_fps, 24, "24 fps");
                    });

                ui.label("画质档位");
                egui::ComboBox::from_label("")
                    .selected_text(self.quality.to_string())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.quality, Quality::Low, "低");
                        ui.selectable_value(&mut self.quality, Quality::Medium, "中");
                        ui.selectable_value(&mut self.quality, Quality::High, "高");
                    });

                ui.horizontal(|ui| {
                    ui.label("输出路径：");
                    ui.text_edit_singleline(&mut self.output_path);
                    if ui.button("选择").clicked() {
                        // 打开文件夹选择器
                    }
                });
            });
        ui.add_space(4.0);

        // ========== 历史缓存会话（默认折叠） ==========
        egui::CollapsingHeader::new("历史缓存会话")
            .default_open(false)
            .show(ui, |ui| {
                if self.cache_sessions.is_empty() {
                    ui.label("暂无缓存会话");
                } else {
                    for session in &mut self.cache_sessions {
                        ui.horizontal(|ui| {
                            ui.label(format!("{}｜{}帧", session.name, session.frame_count));
                            if ui.button("重新导出").clicked() {
                                // 加载该会话缓存并进入导出
                            }
                            if ui.button("打开文件夹").clicked() {
                                // 打开会话目录
                            }
                            if ui.button("删除").clicked() {
                                // 删除会话缓存文件夹
                            }
                        });
                    }
                }
            });

        ui.add_space(12.0);

        // ========== 底部操作区 ==========
        match self.state {
            AppState::Idle => {
                if ui.button("开始录制").clicked() {
                    // 启动录制，主窗口最小化，打开悬浮条
                }
            }
            AppState::Recording => {
                ui.label("录制进行中，主窗口已最小化，请使用悬浮条操作");
            }
            AppState::Encoding => {
                let progress = self.encode_done_frames as f32 / self.encode_total_frames.max(1) as f32;
                ui.add(egui::ProgressBar::new(progress).text(format!("{:.1}%")));
                ui.label(format!("已处理 {}/{}", self.encode_done_frames, self.encode_total_frames));
                if ui.button("取消合成").clicked() {
                    self.encode_cancel = true;
                }
            }
        }
    });

    // 帮助弹窗：如何定位窗口
    if self.show_help_dialog {
        egui::Modal::new(egui::Id::new("canvas_help")).show(ctx, |ui| {
            ui.heading("如何定位画布窗口");
            ui.separator();
            ui.label("想要只录制画布，需要把画布拆成独立浮动窗口，再使用【拾取窗口】选中画布。");
            ui.add_space(8.0);

            ui.label("🖌️ SAI2");
            ui.label("视图 → 新建浮动视图");
            ui.label("");

            ui.label("🖌️ Photoshop");
            ui.label("拖拽画布标签向外拉出，脱离主框架");
            ui.label("");

            ui.label("🖌️ CSP / 优动漫PAINT");
            ui.label("拖拽画布标签分离，或 视图 → 新建视图");
            ui.label("");

            ui.label("🖌️ Krita");
            ui.label("窗口 → 新建窗口");
            ui.label("");

            ui.label("💡 提示：拾取后查看预览窗口，如果画面包含工具栏/图层面板，说明画布没有浮动。");
            ui.add_space(12.0);
            if ui.button("关闭").clicked() {
                self.show_help_dialog = false;
            }
        });
    }
}
