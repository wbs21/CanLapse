//! CanLapse 图形界面入口，**windows 子系统**：双击不会弹出控制台黑框。
//!
//! 与 `canlapse.exe`（控制台子系统，命令行用）共用同一套模块 ——
//! 用一个 exe 兼顾两种形态的话，GUI 版必然会带一个黑框，
//! 而 `#![windows_subsystem = "windows"]` 又会把 CLI 的输出全部丢掉，
//! 所以拆成两个入口是唯一干净的解法。
#![windows_subsystem = "windows"]

use canlapse::{ui, win};

fn main() {
    // 支持 `CanLapse.exe --hwnd 0x…` / `--title 子串` 预先指定目标
    // （便于做快捷方式，也便于脚本化验证）
    let args: Vec<String> = std::env::args().collect();
    let mut target: Option<isize> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--hwnd" => {
                i += 1;
                if i < args.len() {
                    let t = args[i].trim_start_matches("0x").trim_start_matches("0X");
                    target = isize::from_str_radix(t, 16).ok();
                }
            }
            "--title" => {
                i += 1;
                if i < args.len() {
                    target = win::find_window(&args[i]).map(|w| w.hwnd as isize);
                }
            }
            _ => {}
        }
        i += 1;
    }
    std::process::exit(ui::run(target));
}
