# FFmpeg 依赖与 FFI 偏移量升级指南

CanLapse 的视频编码路径（`src/encode.rs` + `src/encode/ffi.rs`）使用**手写 FFI** 调用
自编译的最小 FFmpeg + OpenH264 编码器，**不走 `ffmpeg-sys` crate**。本页说明升级依赖时
必须遵守的三步流程 —— 跳过任何一步都会导致**静默内存损坏**（读写了错误的结构体字段）。

---

## 背景

### 为什么是手写 FFI？

| 方案 | 问题 |
|------|------|
| `ffmpeg-sys` crate | 需要系统安装 libclang.dll，Windows 离线/纯净开发环境下不可用 |
| `ffmpeg-next` crate | 同上，且引入大量间接依赖 |
| 手写 FFI | 零 clang 依赖、编译快、行为完全可预测 |

代价：必须手动维护 FFmpeg 结构体内部字段的**字节偏移量**。

### 偏移量是什么？

`src/encode/ffi.rs` 里大约 30 个 `off::XXX = NNN` 常量，例如：

```rust
// AVCodecContext 的各字段在内存中的字节偏移
pub mod off {
    pub const CC_BIT_RATE:   usize = 56;   // bit_rate  (i64)
    pub const CC_TIME_BASE:  usize = 84;   // time_base (AVRational: 2× i32)
    pub const CC_PIX_FMT:    usize = 136;  // pix_fmt   (enum/i32)
    // ...
}
```

这些值是在 **MSVC x64 ABI + 特定 FFmpeg 头文件版本** 下实测出来的。
**FFmpeg 任何小版本（n9.0 → n9.1）都可能插入/删除结构体字段，导致后续字段的偏移量整体错位。**

运行时 `check_ffi_version()` 会校验 major 版本号（avcodec-63 / avutil-61），
但 **major 相同不代表偏移量一样**。

### 当前锁定版本

- **FFmpeg**: tag n9.0（release 9.0.2）
- **avcodec major**: 63
- **avutil major**: 61
- **swscale major**: 10
- **OpenH264**: v2.6.0
- **编译器**: MSVC 2022（x64），静态 CRT（`/MT`）

---

## 升级三步流程

### 第一步：编译新版 FFmpeg + OpenH264

构建参数（与仓库 `deps/ffmpeg-static/README.md` 一致）：

```bash
# FFmpeg
./configure --arch=x86_64 --prefix=... --toolchain=msvc \
  --disable-everything --enable-encoder=openh264 \
  --enable-parser=h264 --enable-muxer=mp4 --enable-demuxer=h264 \
  --disable-shared --enable-static --disable-debug --disable-doc

# OpenH264 2.6.0
# CMake + ninja，MSVC x64 Release
```

把产物复制到仓库 `deps/ffmpeg-static/lib/`，替换旧 `.lib` 文件。

### 第二步：重跑偏移量探针（最关键）

`spikes/encode-mp4/` 目录曾是最早验证编码链路的原型代码。它里面有一个小程序，
可以在编译期把 FFmpeg 头文件里的各 `offsetof(AVCodecContext, field)` 值打印出来。

**如果 `spikes/encode-mp4/` 已被移除**（本项目清理历史验证代码时可能删掉了它），
用下面的 C 程序重新探针 —— 把它放在一个临时目录，用与 FFmpeg 相同的 MSVC 编译器编译：

```c
#include <stdio.h>
#include <stddef.h>
#include <libavcodec/avcodec.h>
#include <libavutil/avutil.h>

// 用 offsetof 宏直接取结构体成员的字节偏移
#define DUMP(type, field) \
  printf("off::%s_%s = %zu;\n", #type, #field, offsetof(type, field))

int main(void) {
    DUMP(AVCodecContext, bit_rate);
    DUMP(AVCodecContext, time_base);
    DUMP(AVCodecContext, width);
    DUMP(AVCodecContext, height);
    DUMP(AVCodecContext, pix_fmt);
    DUMP(AVCodecContext, gop_size);
    DUMP(AVCodecContext, max_b_frames);
    DUMP(AVCodecContext, flags);
    DUMP(AVCodecContext, extradata);
    DUMP(AVCodecContext, extradata_size);
    DUMP(AVCodecContext, profile);
    DUMP(AVCodecContext, level);
    DUMP(AVCodecContext, codec);
    DUMP(AVCodecContext, codec_id);
    DUMP(AVCodecContext, coded_frame);
    DUMP(AVCodecContext, frame);
    DUMP(AVCodecContext, hw_frames_ctx);
    DUMP(AVFrame, data);
    DUMP(AVFrame, linesize);
    DUMP(AVFrame, width);
    DUMP(AVFrame, height);
    DUMP(AVFrame, format);
    DUMP(AVPacket, data);
    DUMP(AVPacket, size);
    DUMP(AVPacket, pts);
    DUMP(AVPacket, dts);
    DUMP(AVPacket, duration);
    return 0;
}
```

编译命令（MSVC Native x64 命令行）：

```cmd
cl /EHsc /I"<ffmpeg>\include" probe.c /link /LIBPATH:"<ffmpeg>\lib" avcodec.lib avutil.lib
probe.exe
```

### 第三步：更新偏移常量 + 版本号

把探针输出的数值粘贴到 `src/encode/ffi.rs` 的 `pub mod off` 里。
然后更新文件顶部的版本锁定注释：

```rust
//! // 锁定版本：FFmpeg n9.1 / avcodec-63 / avutil-62
```

最后，**全量测试 + 实际录制一次 30 秒片段 + 播放确认视频画面完整**。
不要只跑单元测试 —— 偏移量错了单元测试可能过（mock 数据），但实际编码出来的视频会花屏。

---

## 风险与兜底

| 情况 | 处理 |
|------|------|
| 探针输出和旧值差了 4 或 8 字节 | 说明 FFmpeg 在结构体中间插入了一个指针/整数字段。仔细对照 FFmpeg 的 git log，找出哪个新版本加了这个字段 |
| OpenH264 新版改了 API | 看 `encode::Codecs::openh264` 里的 `encoder_open` 参数；如果 openh264 改了回调签名，改 `ffi.rs` 里的回调函数 |
| 不想再碰手写 FFI | 改 `encode.rs` 为通过命令行调用外部 `ffmpeg.exe`（`process::Command`），放弃静态链接单文件分发 |

---

## 相关文件

- `src/encode.rs` — 高层编码逻辑（encode_session → muxer）
- `src/recorder.rs` — 录制循环、`SessionWriter` 有界队列和写入线程
- `src/encode/ffi.rs` — 手写 FFI + 偏移量表 + RAII 封装
- `build.rs` — 静态/动态链接选择逻辑
- `deps/ffmpeg-static/README.md` — 静态库的构建参数
- `docs/decisions/0003-video-encoding-ffmpeg-min-openh264.md` — 为什么选这套编码方案的 ADR
