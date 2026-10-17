"""按用户参考稿（Desktop\\canlapse-a.ico）重画：灰色圆角方 + 方头字母 C。

参考稿实测（256×256）：
  * 底：圆角方，半径 ≈45px，灰 #454950；
  * C：外框 132×188（x 54..186, y 33..221），笔画 ≈36px，外圆角 ≈45，
    右侧开口 ≈ y 71..184（高约 112），红色 #C0453A。

产出 icon/ 下：
  square-c1.ico/png   —— 完全对齐参考稿
  square-c2.ico/png   —— C 更方（外圆角 26、内圆角 6、开口略窄）
  square-c3.ico/png   —— 最方（外圆角 14、内圆角 3、笔画略厚）
  square-compare.png  —— 参考稿 vs c1/c2/c3（各 256）
"""

import os

from PIL import Image, ImageDraw

SS = 4
BG = (69, 73, 80)      # #454950
RED = (192, 69, 58)    # #C0453A
BG_RADIUS = 0.176      # 相对画布边长

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "icon")
os.makedirs(OUT, exist_ok=True)


def body(size):
    w = size * SS
    img = Image.new("RGBA", (w, w), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rounded_rectangle(
        [0, 0, w - 1, w - 1], radius=int(size * BG_RADIUS) * SS, fill=BG + (255,)
    )
    return img


def c_mask(size, r_out, r_in, stroke, notch_h, box_w, box_h, dx):
    """生成 C 的透明蒙版：外框-内框（圆角方环），再切掉右侧开口"""
    w = size * SS
    m = Image.new("L", (w, w), 0)
    d = ImageDraw.Draw(m)
    # 所有度量都按 256 画布给出，这里按目标尺寸等比例换算（否则小图标里 C 会撑出画布被裁切）
    k = size / 256.0
    cx = w / 2 + dx * k * SS
    cy = w / 2
    ow, oh = box_w * k * SS, box_h * k * SS
    o = [cx - ow / 2, cy - oh / 2, cx + ow / 2, cy + oh / 2]
    s = stroke * k * SS
    i = [o[0] + s, o[1] + s, o[2] - s, o[3] - s]
    d.rounded_rectangle(o, radius=r_out * k * SS, fill=255)
    d.rounded_rectangle(i, radius=max(0, r_in) * k * SS, fill=0)
    # 右侧开口：从内框右侧往外切一条横贯的矩形
    nh = notch_h * k * SS
    d.rectangle([i[0] + 1, cy - nh / 2, o[2] + SS, cy + nh / 2], fill=0)
    return m


def render(size, r_out, r_in, stroke, notch_h, box_w=132, box_h=188, dx=-8):
    img = body(size)
    m = c_mask(size, r_out, r_in, stroke, notch_h, box_w, box_h, dx)
    img.paste(RED + (255,), (0, 0), m)
    return img.resize((size, size), Image.LANCZOS)


VARIANTS = {
    # 名: (外圆角, 内圆角, 笔画, 开口高, 框宽, 框高, x 偏移)
    "square-c1": (45, 9, 36, 112, 132, 188, -8),
    "square-c2": (26, 6, 36, 104, 134, 190, -6),
    "square-c3": (14, 3, 38, 100, 136, 192, -5),
}


def write_ico(path, frames):
    """自己写 ICO：每档用**独立渲染的位图**，避免 Pillow 把大图裁成小图。

    * ≤64px 用 BMP(DIB) 条目（兼容性最好：老 shell/资源管理器都能显示）；
    * 128/256 用 PNG 条目（Vista+ 支持，体积小）。
    """
    import io
    import struct

    entries = []  # (size, payload)
    for size, img in frames:
        if size <= 64:
            w, h = img.size
            header = struct.pack(
                "<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, w * h * 4, 0, 0, 0, 0
            )
            px = img.load()
            xor = bytearray()
            for y in range(h - 1, -1, -1):
                for x in range(w):
                    r, g, b, a = px[x, y]
                    xor += bytes((b, g, r, a))
            mask_stride = ((w + 31) // 32) * 4
            and_mask = bytes(mask_stride * h)
            entries.append((size, header + bytes(xor) + and_mask))
        else:
            buf = io.BytesIO()
            img.save(buf, format="PNG", optimize=True)
            entries.append((size, buf.getvalue()))

    n = len(entries)
    out = bytearray(struct.pack("<HHH", 0, 1, n))
    offset = 6 + 16 * n
    for size, payload in entries:
        w = 0 if size >= 256 else size
        out += struct.pack(
            "<BBBBHHII", w, w, 0, 0, 1, 32, len(payload), offset
        )
        offset += len(payload)
    for _, payload in entries:
        out += payload
    with open(path, "wb") as f:
        f.write(out)


def save(name, params):
    sizes = [16, 24, 32, 48, 64, 128, 256]
    frames = [(s, render(s, *params)) for s in sizes]
    frames[-1][1].save(os.path.join(OUT, f"{name}.png"))
    write_ico(os.path.join(OUT, f"{name}.ico"), frames)
    return frames[-1][1]


if __name__ == "__main__":
    imgs = {n: save(n, p) for n, p in VARIANTS.items()}
    ref = Image.open(os.path.join(OUT, "user-ref-256.png")).convert("RGBA")

    pad = 16
    w = 256 * 4 + pad * 5
    comp = Image.new("RGBA", (w, 256 + pad * 2), (246, 246, 246, 255))
    comp.paste(ref, (pad, pad), ref)
    x = pad * 2 + 256
    for n in VARIANTS:
        comp.paste(imgs[n], (x, pad), imgs[n])
        x += 256 + pad
    comp.save(os.path.join(OUT, "square-compare.png"))

    # 尺寸条（c2 更方）
    sizes = [16, 24, 32, 48, 64, 128, 256]
    canvas = Image.new(
        "RGBA",
        (sum(sizes) + pad * (len(sizes) + 1), max(sizes) + pad * 2),
        (246, 246, 246, 255),
    )
    x = pad
    for s in sizes:
        small = render(s, *VARIANTS["square-c2"])
        canvas.paste(small, (x, pad + (max(sizes) - s) // 2), small)
        x += s + pad
    canvas.save(os.path.join(OUT, "square-sizes.png"))

    # 验证：把 ICO 里的每一档**读回来**放大 6 倍拼图（若被裁切，这里能直接看出来）
    ico = Image.open(os.path.join(OUT, "square-c2.ico"))
    sizes = [16, 24, 32, 48, 64, 128, 256]
    tiles = []
    for s in sizes:
        frame = ico.ico.getimage((s, s)).convert("RGBA")
        tiles.append((s, frame))
    tile = 150
    pad = 14
    ver = Image.new("RGBA", (tile * len(tiles) + pad * (len(tiles) + 1), tile + 40), (246, 246, 246, 255))
    x = pad
    for s, frame in tiles:
        big = frame.resize((tile, tile), Image.NEAREST)
        ver.paste(big, (x, pad), big)
        x += tile + pad
    ver.save(os.path.join(OUT, "square-ico-verify.png"))
    print("frames in ICO:", [s for s, _ in tiles])
    print("ok", OUT)
