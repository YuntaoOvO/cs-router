fn main() {
    // 前端目录在 crate 之外，显式声明监听，保证界面改动触发重新打包
    println!("cargo:rerun-if-changed=../ui/index.html");
    println!("cargo:rerun-if-changed=../ui/main.js");
    println!("cargo:rerun-if-changed=../ui/style.css");
    tauri_build::build()
}
