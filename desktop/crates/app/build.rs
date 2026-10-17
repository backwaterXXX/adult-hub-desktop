fn main() {
    println!("cargo:rerun-if-changed=ui/main.slint");
    slint_build::compile("ui/main.slint").expect("failed to compile Slint UI");
}
