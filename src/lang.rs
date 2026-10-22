//! 极简 i18n：中英两套文案 + 跟随系统语言（默认英文）。
//!
//! 设计取舍（v1.1 需求）：
//! * **不做语言选择项**：启动时读系统 UI 语言，中文系统用中文，其余一律英文；
//! * 字典就是一个 `enum` + 一张 `(en, zh)` 表，新增文案只加一行；
//! * 带变量的文案用 `{}` 占位，`f()` 顺序填充 —— 够用且不引入格式化依赖；
//! * 字体不打包：英文用系统 Segoe UI、中文用 Microsoft YaHei UI（Win10/11 自带），
//!   缺字时由系统字体链接自动回退，包体不增加。
//!
//! 调试/测试可用环境变量 `CANLAPSE_LANG=en|zh` 强制指定（不是给用户的 UI 选项）。

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// 当前语言（首次调用时根据系统语言/环境变量确定）
pub fn lang() -> Lang {
    *LANG.get_or_init(detect)
}

fn detect() -> Lang {
    // 1) 调试覆盖
    if let Ok(v) = std::env::var("CANLAPSE_LANG") {
        let v = v.trim().to_ascii_lowercase();
        if v.starts_with("zh") {
            return Lang::Zh;
        }
        if v.starts_with("en") {
            return Lang::En;
        }
    }
    // 2) 系统 UI 语言：中文（简/繁）→ 中文，其余 → 英文
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() } as u32;
    let primary = id & 0x3FF; // LANGID 低 10 位为主语言
    if primary == 0x04 {
        Lang::Zh
    } else {
        Lang::En
    }
}

macro_rules! keys {
    ($( $variant:ident => ($en:expr, $zh:expr $(,)?) ),* $(,)?) => {
        /// 全部界面文案的键
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum K { $($variant),* }

        impl K {
            fn pair(self) -> (&'static str, &'static str) {
                match self { $(K::$variant => ($en, $zh)),* }
            }
        }
    };
}

keys! {
    // ---- 主窗口 ----
    Subtitle        => ("Canvas Timelapse Recorder", "画布缩时录屏"),
    PickWindow      => ("🎯 Pick Window", "🎯 拾取窗口"),
    HowToLocate     => ("❓ How to Locate", "❓ 如何定位窗口"),
    FrameDensity    => ("Frame density", "帧密度"),
    Sensitivity     => ("Change sensitivity", "画面检测灵敏度"),
    SensHint        => ("Larger canvas = stricter", "画布越大，要求越高。"),
    AutoClean       => ("Delete cache after successful export", "合成成功自动清理缓存"),
    CleanCache      => ("Clean Cache", "手动清理缓存"),
    OpenVideoFolder => ("Open Video Folder", "打开视频文件夹"),
    StartRecording  => ("Start Recording", "开始录制"),
    RecordingHint   => ("Recording… use the floating bar: pause / mark / stop", "录制中…用悬浮条：暂停 / 标记 / 停止"),
    Preparing       => ("Preparing export…", "正在准备合成…"),
    ExportingStatus => ("Exporting… {} / {} frames", "视频合成中… 已处理：{} / {} 帧"),
    CancelExport    => ("Cancel Export", "取消合成"),

    // 标题栏里的折叠头（默认折叠、不显示，仍翻译）
    HdrRecording    => ("Recording", "录制参数"),
    HdrExport       => ("Export", "导出视频参数"),
    HdrHistory      => ("Cached sessions", "历史缓存会话"),
    Reexport        => ("Re-export", "重新导出"),
    OpenFolder      => ("Open folder", "打开文件夹"),
    Delete          => ("Delete", "删除"),

    // 状态行
    StatusEmpty     => ("No canvas selected yet", "尚未选择画布窗口"),
    StatusOkWith    => ("✅ Canvas selected ({}, clicked {})", "✅ 已选中画布窗口（{}，点到{}）"),
    StatusOkPlain   => ("✅ Canvas selected ({})", "✅ 已选中画布窗口（{}）"),
    StatusPanelWarn => ("⚠️ Canvas not floating? See How to Locate", "⚠️ 画布未浮动？点【如何定位窗口】"),
    WhereSurface    => ("surface", "画面"),
    WhereBorder     => ("border", "边框"),
    GrabFailed      => ("Cannot capture this window: {}. Please pick again.", "无法抓取该窗口：{}　请重新拾取"),
    PickCanceled    => ("Picking canceled, please try again", "已取消拾取，请重新拾取"),
    PickInstr       => ("Click the canvas border; Esc to cancel", "请点击画布边框；Esc 取消"),
    PickHint        => ("Click the canvas border", "请点击画布边框"),

    ConfirmTitle    => ("Please confirm", "请确认"),
    AlreadyRunning  => ("CanLapse is already running (it may be minimized to the taskbar).", "CanLapse 已经在运行（可能被最小化到任务栏）。"),
    CanvasFallback  => ("canvas", "画布"),

    // 预览占位
    PreviewEmpty    => ("Preview\nshown after picking", "预览区域\n拾取窗口后显示"),

    // 帧密度选项
    Fps1of5         => ("1 frame / 5 s", "1 帧 / 5 秒"),
    Fps1of2         => ("1 frame / 2 s", "1 帧 / 2 秒"),
    Fps1            => ("1 frame / s", "1 帧 / 秒"),
    Fps2            => ("2 frames / s", "2 帧 / 秒"),

    // ---- 消息框 ----
    Notice          => ("Notice", "提示"),
    CannotRecord    => ("Cannot record", "无法录制"),
    RecordingFailed => ("Recording failed", "录制失败"),
    RecordingEnded  => ("Recording ended", "录制结束"),
    CannotDelete    => ("Cannot delete", "无法删除"),
    DeleteFailed    => ("Delete failed", "删除失败"),
    PanelWarnTitle  => ("Check the canvas window", "检查画布窗口"),
    ExportInProgress => ("An export is still in progress. Please wait.", "视频合成仍在进行中，请稍候。"),

    PickFirst       => ("Pick a canvas window first (🎯 Pick Window).", "请先点【🎯 拾取窗口】选择要录制的画布窗口。"),
    NoDwm           => ("Desktop composition (DWM) is off — thumbnail capture is unavailable.", "桌面合成（DWM）未开启，缩略图抓帧不可用。"),
    NoFrames        => ("No frames were saved this time (the picture may never have changed).", "这次没有保存到任何帧（画面可能始终没有变化）。"),
    SelectFirst     => ("Select a session in the list first.", "请先在列表里选择一个会话。"),
    BusyDelete      => ("A session that is recording or exporting cannot be deleted.", "正在录制或正在合成中的会话不能删除。"),
    BarFailed       => ("Cannot create the floating control bar. Recording was cancelled.", "无法创建悬浮控制条，录制已取消。"),
    ThreadTimeout   => ("The recording thread did not finish in time. Please close and reopen CanLapse.", "录制线程未能及时收尾，请关闭并重新打开 CanLapse。"),
    DeleteConfirm   => ("Delete session “{}” and all of its cached frames?", "确定删除会话「{}」及其全部缓存帧吗？"),

    // 帮助正文
    HelpTitle       => ("How to locate the canvas window", "如何定位画布窗口"),
    HelpBody        => (
        "To record only the canvas, detach it into its own floating window, then use Pick Window.\n\n\
         SAI2: View → New Floating View\n\
         Photoshop: drag the document tab out of the main frame\n\
         CSP / PAINT: drag the canvas tab out, or View → New View\n\
         Krita: Window → New Window\n\n\
         Tip: after picking, check the preview. If it shows toolbars or layer panels, the canvas is not floating.\n\n\
         About overlap: the drawing app's own floating panels (e.g. SAI2 Color) are recorded as-is — that is expected, no cropping, no warning.\n\
         When another app comes to the foreground, recording pauses automatically and resumes when you go back.",
        "想要只录制画布，需要把画布拆成独立浮动窗口，再使用【拾取窗口】选中画布。\n\n\
         SAI2：视图 → 新建浮动视图\n\
         Photoshop：拖拽画布标签向外拉出，脱离主框架\n\
         CSP / 优动漫 PAINT：拖拽画布标签分离，或 视图 → 新建视图\n\
         Krita：窗口 → 新建窗口\n\n\
         💡提示：拾取后看左上角的预览缩略图，如果画面包含工具栏 / 图层面板，说明画布没有浮动。\n\n\
         ℹ️ 关于遮挡：绘画软件自己的浮动面板（SAI2 的「颜色」等）若压在画布上，\n\
         会照原样录进画面 —— 这是正常的，不做裁切、也不提示。\n\
         而当你切到其他软件时，说明此刻没有在画画，录制会自动暂停，切回画布即继续。",
    ),

    // ---- 合成弹窗 ----
    DlgTitle        => ("Export", "合成进度"),
    DlgExporting    => ("Exporting video", "视频合成中"),
    DlgDone         => ("Export complete", "合成完成"),
    DlgFrames       => ("Processed: {} / {} frames", "已处理：{} / {} 帧"),
    DlgDoneText     => ("Done: {} frames · {} MB · {}x{}\n{}", "合成完成：{} 帧 · {} MB · {}x{}\n{}"),
    DlgCanceledText => ("Cancelled: {} frames written, the file is still playable\n{}", "已取消合成：写入 {} 帧，文件仍可播放\n{}"),
    DlgFailedText   => ("Export failed: {}", "合成失败：{}"),

    // ---- 悬浮控制条 ----
    PauseBtn        => ("Pause", "暂停"),
    ResumeBtn       => ("Resume", "继续"),
    MarkBtn         => ("Mark", "标记"),
    StopBtn         => ("Stop", "停止"),
    BarFrames       => ("{} · {} frames", "{} · {} 帧"),
    BarFrameOne     => ("{} · 1 frame", "{} · 1 帧"),
    BarMarked       => ("{} marks", "{} 标记"),
    BarDropped      => ("{} dropped", "丢帧 {}"),
    BarBlank        => ("blank capture? check preview", "纯色抓帧？看预览"),
    BarPaused       => ("Paused", "已暂停"),
    BarBackToCanvas => ("Back to canvas to resume", "回到画布继续"),

    // ---- 会话列表 ----
    SessionItem     => ("{} ｜ {} frames", "{} ｜ {} 帧"),
}

/// 取文案
pub fn s(k: K) -> &'static str {
    let (en, zh) = k.pair();
    match lang() {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// 取带占位符的文案并按顺序填充（模板里用 `{}`）
pub fn f(k: K, args: &[&dyn std::fmt::Display]) -> String {
    fill(s(k), args)
}

pub fn fill(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    let mut idx = 0;
    while let Some(pos) = rest.find("{}") {
        out.push_str(&rest[..pos]);
        if let Some(a) = args.get(idx) {
            out.push_str(&a.to_string());
        } else {
            out.push_str("{}");
        }
        idx += 1;
        rest = &rest[pos + 2..];
    }
    out.push_str(rest);
    out
}

/// 界面字体族：英文用 Segoe UI，中文用 Microsoft YaHei UI（都随系统，不打包字体）
pub fn ui_font_face() -> &'static str {
    match lang() {
        Lang::En => "Segoe UI",
        Lang::Zh => "Microsoft YaHei UI",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 填充占位符按顺序() {
        let out = fill("/{}/{}/{}", &[&1, &"a", &2.5]);
        assert_eq!(out, "/1/a/2.5");
    }

    #[test]
    fn 参数不足时保留占位符() {
        assert_eq!(fill("{} + {}", &[&1]), "1 + {}");
    }

    #[test]
    fn 中英文案都不为空() {
        for k in [
            K::PickWindow,
            K::StartRecording,
            K::CleanCache,
            K::OpenVideoFolder,
            K::DlgExporting,
            K::PauseBtn,
        ] {
            let (en, zh) = k.pair();
            assert!(!en.is_empty());
            assert!(!zh.is_empty());
            assert_ne!(en, zh);
        }
    }

    #[test]
    fn 带参数文案只使用顺序占位符() {
        // lang::fill 只认识 "{}"。模板里一旦出现 "{:.2}" 之类的格式说明，
        // 参数就会整体错位（v1.3 的导出完成弹窗踩过）。
        for k in [K::DlgDoneText, K::DlgCanceledText, K::DlgFailedText] {
            let (en, zh) = k.pair();
            for t in [en, zh] {
                assert!(!t.contains("{:"), "模板含不支持的格式占位符: {t}");
            }
        }
        let s = f(
            K::DlgDoneText,
            &[&12, &"1.25", &1920, &1080, &"D:\\out.mp4"],
        );
        assert!(s.contains("12") && s.contains("1.25") && s.contains("1920"));
        assert!(s.contains("out.mp4"));
    }

    #[test]
    fn 帮助正文不使用消息框不支持的标记() {
        let (en, zh) = K::HelpBody.pair();
        assert!(!en.contains("**") && !zh.contains("**"));
    }
}
