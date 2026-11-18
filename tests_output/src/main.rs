#![forbid(unsafe_code)]
mod sample_js;
mod sample_py;
mod sample_ts;
mod sample_kt;
mod sample_java;
mod sample_go;
fn main() {
    sample_js::__init_module__();
    sample_py::__init_module__();
    println!("all generated modules compiled and ran (js+py main executed)");
}
