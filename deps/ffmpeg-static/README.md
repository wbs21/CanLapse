# deps/ffmpeg-static —— 静态链接用的 ffmpeg / openh264 库（随仓库一起走）

把静态库放进仓库，是为了**任何一台机器 clone 下来都能直接编出 release**
（不再依赖某台机器 `D:\tools` 下的目录）。

## 内容

| 文件 | 大小 | 说明 |
|---|---|---|
| `lib/avcodec.lib` | 6.9 MB | ffmpeg 9.0.2，MSVC 静态库 |
| `lib/avformat.lib` | 6.3 MB | 同上 |
| `lib/avutil.lib` | 7.0 MB | 同上 |
| `lib/swscale.lib` | 8.1 MB | 同上 |
| `lib/swresample.lib` | 1.0 MB | 同上 |
| `lib/openh264.lib` | 8.0 MB | openh264 2.6.0，MSVC 静态库 |

合计约 36 MB。`build.rs` 会**优先**找这里（`deps/ffmpeg-static/lib/avcodec.lib`）。

## 构建参数（复现用）

openh264：

```bash
# 在 MSYS2 + vcvars64 环境下
cd <openh264-src>
make OS=msvc ARCH=x86_64 BUILDTYPE=Release
# 产物 libopenh264.lib
```

ffmpeg（只保留本程序用到的组件，产物明显更小）：

```bash
cd <ffmpeg-src>
mkdir build-msvc-static && cd build-msvc-static
../configure --toolchain=msvc --arch=x86_64 \
  --enable-static --disable-shared \
  --disable-programs --disable-doc --disable-network \
  --disable-avdevice --disable-avfilter --disable-everything \
  --enable-avcodec --enable-avformat --enable-swscale --enable-swresample \
  --enable-encoder=libopenh264 --enable-muxer=mov --enable-muxer=mp4 \
  --enable-protocol=file --enable-libopenh264
make -j8
```

然后把 `libavcodec/avcodec.lib`、`libavformat/avformat.lib`、`libavutil/avutil.lib`、
`libswscale/swscale.lib`、`libswresample/swresample.lib` 以及 openh264 的 `openh264.lib`
拷到本目录的 `lib/` 下即可。

> ffmpeg 的 `--toolchain=msvc` 不显式指定 `/MD` 或 `/MT`，cl 默认用 **/MT（静态 CRT）**，
> 与 Rust 的 `-C target-feature=+crt-static` 一致；混用 /MD 的库会出现 CRT 符号冲突。

## 许可

* ffmpeg：**LGPL v2.1 or later**（构建时未启用 `--enable-gpl`，未链接 x264 等 GPL 组件），
  见 `licenses/ffmpeg-LGPLv2.1.txt`；
* openh264：**BSD 2-Clause**，见 `licenses/openh264-BSD.txt`。

分发本程序的 exe 时，请一并保留上述许可与出处说明（LGPL 要求可替换库；
本项目以静态链接方式使用 ffmpeg，若需替换版本，可按上面的参数重新构建并重新链接）。
