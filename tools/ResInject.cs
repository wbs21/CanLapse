// CanLapse 构建后资源注入：把 .res 里的图标/版本信息复制进已生成的 exe。
//
// 为什么不用链接期方案（都试过、都不行）：
//   cargo:rustc-link-arg=<res>      -> 裸路径没进链接命令行
//   cargo:rustc-link-arg-bins=<res> -> LNK1123
//   资源库 + /WHOLEARCHIVE          -> 无报错但资源未进 exe
// 换成构建后注入（本文件）后可用：types=3 / copied=9。
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public class ResInject {
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr LoadLibraryExW(string f, IntPtr h, uint flags);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr BeginUpdateResourceW(string f, bool deleteExisting);
    [DllImport("kernel32.dll")]
    static extern bool UpdateResourceW(IntPtr h, IntPtr type, IntPtr name, ushort lang, byte[] data, uint cb);
    [DllImport("kernel32.dll")]
    static extern bool EndUpdateResourceW(IntPtr h, bool discard);
    [DllImport("kernel32.dll")]
    static extern IntPtr FindResourceExW(IntPtr h, IntPtr type, IntPtr name, ushort lang);
    [DllImport("kernel32.dll")]
    static extern IntPtr LoadResource(IntPtr h, IntPtr res);
    [DllImport("kernel32.dll")]
    static extern uint SizeofResource(IntPtr h, IntPtr res);
    [DllImport("kernel32.dll")]
    static extern IntPtr LockResource(IntPtr data);
    delegate bool TypeProc(IntPtr h, IntPtr type, IntPtr p);
    delegate bool NameProc(IntPtr h, IntPtr type, IntPtr name, IntPtr p);
    delegate bool LangProc(IntPtr h, IntPtr type, IntPtr name, ushort lang, IntPtr p);
    [DllImport("kernel32.dll")]
    static extern bool EnumResourceTypesW(IntPtr h, TypeProc cb, IntPtr p);
    [DllImport("kernel32.dll")]
    static extern bool EnumResourceNamesW(IntPtr h, IntPtr type, NameProc cb, IntPtr p);
    [DllImport("kernel32.dll")]
    static extern bool EnumResourceLanguagesW(IntPtr h, IntPtr type, IntPtr name, LangProc cb, IntPtr p);

    /// 从 src（含资源的 PE，例如 link /dll /noentry 产出的 res.dll）复制全部资源到 exe。
    public static string Inject(string src, string exe) {
        IntPtr s = LoadLibraryExW(src, IntPtr.Zero, 0x2 /* LOAD_LIBRARY_AS_DATAFILE */);
        if (s == IntPtr.Zero) return "失败：无法加载资源模块";
        IntPtr u = BeginUpdateResourceW(exe, false);
        if (u == IntPtr.Zero) return "失败：BeginUpdateResource";
        var types = new List<IntPtr>();
        EnumResourceTypesW(s, (h, t, p) => { types.Add(t); return true; }, IntPtr.Zero);
        int copied = 0;
        foreach (IntPtr t in types) {
            IntPtr tt = t;
            EnumResourceNamesW(s, tt, (h, ty, nm, p) => {
                IntPtr nn = nm;
                EnumResourceLanguagesW(s, tt, nn, (h2, t2, n2, lang, p2) => {
                    IntPtr r = FindResourceExW(s, t2, n2, lang);
                    if (r == IntPtr.Zero) return true;
                    uint sz = SizeofResource(s, r);
                    IntPtr d = LoadResource(s, r);
                    if (d == IntPtr.Zero) return true;
                    byte[] buf = new byte[sz];
                    Marshal.Copy(LockResource(d), buf, 0, (int)sz);
                    if (UpdateResourceW(u, t2, n2, lang, buf, sz)) copied++;
                    return true;
                }, IntPtr.Zero);
                return true;
            }, IntPtr.Zero);
        }
        bool ok = EndUpdateResourceW(u, false);
        return "资源类型=" + types.Count + " 已复制=" + copied + " 写回=" + ok;
    }
}