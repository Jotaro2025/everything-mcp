// build.rs — 构建期脚本
//
// 作用：告诉 linker 使用 everything_mcp.def 作为模块定义，
// 从而只导出 everything_plugin_proc 这一个符号。
//
// 仅针对 MSVC 工具链（rustup 在 Windows 上的默认值）。GNU/MinGW 工具链不需要
// .def —— #[no_mangle] + extern "system" 已经保证了唯一导出符号，
// 且 ld 默认不导出其他 Rust 内部符号。
//
// 注意：必须用 `rustc-cdylib-link-arg`（仅 cdylib 目标）而不是
// `rustc-link-arg`（所有目标）。.def 里的 `LIBRARY everything_mcp` 会把
// 输出强制变成 DLL —— 如果应用到这个包的其他目标（tests/ 集成测试等），
// 生成的 exe 会带上 DLL 特征位，运行时报 error 193。

use std::env;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let def_path = format!("{}\\everything_mcp.def", manifest);

    let target = env::var("TARGET").unwrap_or_default();
    if target.contains("pc-windows-msvc") {
        // MSVC：通过链接器选项传入模块定义文件，精确控制导出表。
        // 只对 cdylib 目标生效 —— 见文件头说明。
        println!("cargo:rustc-cdylib-link-arg=/DEF:{}", def_path);
        println!("cargo:rerun-if-changed={}", def_path);
    }
    // 其他工具链（GNU/LLVM）依赖 #[no_mangle] 自动控制符号可见性，
    // 不需要在这里追加链接参数。
}
