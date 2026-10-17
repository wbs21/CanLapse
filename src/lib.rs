//! CanLapse 库：所有功能模块都在这里，供两个入口共用。
//!
//! 两个可执行入口：
//! * `src/main.rs`（控制台子系统）—— 命令行：录制 / 合成 / 诊断
//! * `src/bin/canlapse-gui.rs`（windows 子系统）—— 图形界面，双击不弹控制台黑框
//!
//! 做成 lib 而不是让两个 bin 各自 `#[path]` 引入模块，有两个实在的理由：
//! 1. `#[path = "../encode.rs"] mod encode;` 会破坏子模块解析 ——
//!    `encode.rs` 里的 `mod ffi;` 会被解析到 `src/ffi.rs` 而不是 `src/encode/ffi.rs`；
//! 2. 同一份代码被两个 bin 各编译一遍，既慢又容易只改了一处。

pub mod capture;
pub mod encode;
pub mod frame;
pub mod png_io;
pub mod recorder;
pub mod session;
pub mod ui;
pub mod win;
