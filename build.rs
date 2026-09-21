// build.rs — 构建期脚本
//
// 作用：告诉 linker 使用 everything_mcp.def 作为模块定义，
// 从而只导出 everything_plugin_proc 这一个符号。
//
// 仅针对 MSVC 工具链（rustup 在 Windows 上的默认值）。GNU/MinGW 工具链不需要
// .def —— #[no_mangle] + extern "system" 已经保证了唯一导出符号，
// 且 ld 默认不导出其他 Rust 内部符号。

use std::env;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let def_path = format!("{}\\everything_mcp.def", manifest);

    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("pc-windows-msvc") {
        // MSVC：通过链接器选项传入模块定义文件，精确控制导出表。
        println!("cargo:rustc-link-arg=/DEF:{}", def_path);
        println!("cargo:rerun-if-changed={}", def_path);
    }
    // 其他工具链（GNU/LLVM）依赖 #[no_mangle] 自动控制符号可见性，
    // 不需要在这里追加链接参数。
}
